use super::*;
use okf_mcp::{
    investigator::{
        journal::{Journal, PendingMutation},
        types::{AllowedTool, ModelRequest, ModelResult, ModelUsage, Proposal, ToolRequest},
    },
    maintenance::types::{Lane, Lease, RevisionPin},
};
use rmcp::model::Tool;
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command as Process, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Seen {
    connect: AtomicU32,
    model: AtomicU32,
    inference: AtomicU32,
    lists: AtomicU32,
    calls: Mutex<Vec<AllowedTool>>,
    secrets_address: Mutex<Option<usize>>,
}
struct Fake {
    seen: Arc<Seen>,
    mode: String,
    root: PathBuf,
}
struct FakeClient {
    seen: Arc<Seen>,
    mode: String,
    state: PathBuf,
}
struct FakeModel {
    seen: Arc<Seen>,
    mode: String,
    root: PathBuf,
}
impl Backend for Fake {
    type Client = FakeClient;
    type Model = FakeModel;
    async fn connect(
        &mut self,
        _: &RunnerConfig,
        secrets: &Secrets,
    ) -> Result<FakeClient, RunnerError> {
        self.seen.connect.fetch_add(1, Ordering::SeqCst);
        assert_eq!(secrets.token(), "TOKEN_CANARY");
        *self.seen.secrets_address.lock().unwrap() = Some(secrets as *const Secrets as usize);
        tracing::error!(target: "rmcp", body = "SOURCE_BODY_CANARY", token = secrets.token(), "REMOTE_ERROR_CANARY");
        tracing::trace!(target: "reqwest", "AUTHORIZATION_CANARY");
        if self.mode.starts_with("network-") {
            if self.mode.contains("connect") {
                std::fs::write(self.root.join("ready"), "connect").unwrap();
                tokio::time::sleep(Duration::from_secs(4)).await;
            } else if self.mode.contains("shared") {
                tokio::time::sleep(Duration::from_millis(650)).await;
            }
        }
        Ok(FakeClient {
            seen: self.seen.clone(),
            mode: self.mode.clone(),
            state: self.root.join("state/journal.json"),
        })
    }
    fn model(&mut self, _: &RunnerConfig, secrets: &Secrets) -> Result<FakeModel, RunnerError> {
        self.seen.model.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            *self.seen.secrets_address.lock().unwrap(),
            Some(secrets as *const Secrets as usize)
        );
        assert_eq!(secrets.anthropic_api_key(), "PROVIDER_CANARY");
        Ok(FakeModel {
            seen: self.seen.clone(),
            mode: self.mode.clone(),
            root: self.root.clone(),
        })
    }
}
impl FakeClient {
    fn report(&self, state: &str) -> Value {
        let now = chrono::Utc::now();
        json!({"id":"run", "state":state,"actor":"fixture", "created_at":now.to_rfc3339(),
            "deadline":(now + chrono::Duration::seconds(3600)).to_rfc3339(),
            "ended_at":null,"reason":null,"primary_count":if self.mode == "aging" || self.mode == "signal" {1} else {0},
            "completed_count":0,"failed_count":0,"inconclusive_count":0,"superseded_count":0})
    }
}
impl CatalogClient for FakeClient {
    async fn list_tools(&mut self) -> Result<Vec<Tool>, RunnerError> {
        self.seen.lists.fetch_add(1, Ordering::SeqCst);
        if self.mode.starts_with("network-") && !self.mode.contains("connect") {
            std::fs::write(
                self.state.parent().unwrap().parent().unwrap().join("ready"),
                "discovery",
            )
            .unwrap();
            // Private factory simulates two paginated network reads. Actual SDK
            // pagination cancellation is covered separately in client_tests.
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_millis(if self.mode.contains("shared") {
                    350
                } else {
                    2000
                }))
                .await;
            }
        }
        if self.mode == "schema" {
            return Ok(vec![]);
        }
        Ok(serde_json::from_str(include_str!("../../investigator/contract_tools.json")).unwrap())
    }
    async fn call(&mut self, request: ToolRequest) -> Result<Value, RunnerError> {
        self.seen.calls.lock().unwrap().push(request.tool);
        let j: Journal = serde_json::from_slice(&std::fs::read(&self.state).unwrap()).unwrap();
        assert_eq!(j.binding.credential_sha256.len(), 64);
        assert!(!serde_json::to_string(&j).unwrap().contains("TOKEN_CANARY"));
        if matches!(
            request.tool,
            AllowedTool::Claim | AllowedTool::Submit | AllowedTool::Close
        ) {
            assert_eq!(j.pending.as_ref().unwrap().request.args, request.args);
        }
        if self.mode == "partial" && request.tool == AllowedTool::GetRun {
            return Err(RunnerError::Contract);
        }
        let mut value = match request.tool {
            AllowedTool::Claim => {
                json!({"run_id":"run", "leases":if self.mode == "aging" || self.mode == "signal" { vec![Lease {
                run_id:"run".into(),work_id:"work".into(),token:"LEASE_CANARY".into(),
                pin:RevisionPin {path:"/fixture.md".into(), revision:1}, reasons:vec![Lane::Aging],
                expires_at:(chrono::Utc::now()+chrono::Duration::seconds(900)).to_rfc3339(),
            }] } else {vec![]}})
            }
            AllowedTool::GetRun => json!({"run":self.report("running"),"work":[]}),
            AllowedTool::ReadRevision => {
                json!({"pin":{"path":"/fixture.md","revision":1},"content":"Synthetic evidence","outdated":false})
            }
            AllowedTool::GetConcept => {
                json!({"catalog":"synthetic","path":"/fixture.md","title":"Fixture","frontmatter":{},"frontmatter_error":null,"lines":1,"chars":18,"truncated":false,"body":"Synthetic evidence","raw":"Synthetic evidence","links":[],"backlinks":[]})
            }
            AllowedTool::Search => json!({"hits":[],"skipped":[]}),
            AllowedTool::Submit => {
                if self.mode == "aging" {
                    assert_eq!(request.args["submission"]["outcome"], "inconclusive");
                    assert_eq!(request.args["submission"]["assessment"], "unverifiable");
                }
                json!({"run_id":"run","work_id":"work","outcome":request.args["submission"]["outcome"],"finding_ids":[],"outdated":false})
            }
            AllowedTool::Close => self.report(request.args["state"].as_str().unwrap()),
            _ => panic!("unexpected tool"),
        };
        if request.tool != AllowedTool::Search {
            value["catalog"] = json!("synthetic");
        }
        Ok(value)
    }
}
impl Inference for FakeModel {
    async fn infer(
        &self,
        _: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelResult, RunnerError> {
        self.seen.inference.fetch_add(1, Ordering::SeqCst);
        if self.mode == "signal" {
            std::fs::write(self.root.join("ready"), "ready").unwrap();
            cancel.cancelled().await;
            // Completion must be awaited, not merely cancellation requested.
            tokio::time::sleep(Duration::from_millis(80)).await;
            std::fs::write(self.root.join("reaped"), "completed").unwrap();
            return Err(RunnerError::Cancelled);
        }
        Ok(ModelResult {
            proposal: Proposal {
                summary: "Catalog only".into(),
                findings: vec![],
            },
            usage: ModelUsage {
                input: 1,
                output: 1,
                cache_creation: 0,
                cache_read: 0,
                cost_microusd: 1,
            },
        })
    }
}

fn child(mode: &str, root: &std::path::Path) -> Process {
    let mut command = Process::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "tests::cli_scenario", "--nocapture"])
        .env("CLI_TEST_MODE", mode)
        .env("CLI_TEST_ROOT", root)
        .env("OKF_MAINTENANCE_TOKEN", "TOKEN_CANARY")
        .env("ANTHROPIC_API_KEY", "PROVIDER_CANARY")
        .env_remove("DATABASE_URL")
        .env("RUST_LOG", "trace,rmcp=trace,reqwest=trace");
    command
}
fn check_output(output: std::process::Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("CANARY"));
    assert!(stdout.contains("test result: ok"));
}
fn scenario(mode: &str) {
    let root = tempfile::tempdir_in("/var/tmp").unwrap();
    check_output(child(mode, root.path()).output().unwrap());
}
#[test]
fn investigator_cli_preflight_is_nonmutating() {
    scenario("preflight");
    scenario("schema");
}
#[test]
fn investigator_cli_consent_precedes_all_side_effects() {
    for mode in ["no-consent", "upload-only", "budget-only"] {
        scenario(mode);
    }
}
#[test]
fn investigator_cli_lock_contention_exits_3() {
    scenario("lock");
}
#[test]
fn investigator_cli_partial_recovery_exits_4() {
    scenario("partial");
}
#[test]
fn investigator_cli_empty_and_aging_exit_0() {
    scenario("empty");
    scenario("aging");
}
#[test]
fn investigator_cli_quiet_sdk_and_child_stderr() {
    scenario("preflight");
    scenario("childfail");
}
#[test]
fn investigator_cli_signals_await_cleanup() {
    for signal in ["-INT", "-TERM"] {
        let root = tempfile::tempdir_in("/var/tmp").unwrap();
        let mut process = child("signal", root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !root.path().join("ready").exists() {
            assert!(
                process.try_wait().unwrap().is_none(),
                "helper exited before inference"
            );
            if Instant::now() > deadline {
                process.kill().unwrap();
                let _ = process.wait();
                panic!("helper did not start inference");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(Process::new("kill")
            .args([signal, &process.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(5);
        while process.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                process.kill().unwrap();
                process.wait().unwrap();
                panic!("helper did not finish signal cleanup");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        check_output(process.wait_with_output().unwrap());
        assert!(root.path().join("reaped").exists());
    }
}

#[test]
fn investigator_cli_network_setup_obeys_deadline_and_signals() {
    for command in ["runonce", "preflight"] {
        for phase in ["connect", "discovery", "shared"] {
            for stop in ["deadline", "INT", "TERM"] {
                let root = tempfile::tempdir_in("/var/tmp").unwrap();
                let mode = format!("network-{command}-{phase}-{stop}");
                let mut process = child(&mode, root.path())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let started = Instant::now();
                if stop != "deadline" {
                    while !root.path().join("ready").exists() {
                        if started.elapsed() > Duration::from_secs(5)
                            || process.try_wait().unwrap().is_some()
                        {
                            let _ = process.kill();
                            let output = process.wait_with_output().unwrap();
                            panic!("setup not reached: {mode}: {:?}", output);
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    assert!(Process::new("kill")
                        .args([&format!("-{stop}"), &process.id().to_string()])
                        .status()
                        .unwrap()
                        .success());
                }
                let stopping = Instant::now();
                while process.try_wait().unwrap().is_none() {
                    if stopping.elapsed() > Duration::from_millis(1800) {
                        process.kill().unwrap();
                        process.wait().unwrap();
                        panic!("setup did not stop promptly: {mode}");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                check_output(process.wait_with_output().unwrap());
            }
        }
    }
}

#[test]
fn investigator_cli_readiness_signals_reap_before_exit() {
    for command in ["preflight", "runonce"] {
        for phase in ["version", "help"] {
            for signal in ["SIGINT", "SIGTERM"] {
                probe_scenario(&format!("probe-{command}-{phase}"), signal);
            }
        }
    }
}

#[test]
fn investigator_cli_readiness_deadline_reaps_before_exit() {
    for phase in ["version", "help"] {
        probe_scenario(&format!("probe-deadline-{phase}"), "deadline");
    }
}

fn probe_scenario(mode: &str, signal: &str) {
    let root = tempfile::tempdir_in("/var/tmp").unwrap();
    let output = Process::new("python3")
        .arg("tests/fixtures/investigator/claude/readiness_signals.py")
        .arg(std::env::current_exe().unwrap())
        .arg(signal)
        .env("CLI_TEST_MODE", mode)
        .env("CLI_TEST_ROOT", root.path())
        .env("OKF_MAINTENANCE_TOKEN", "TOKEN_CANARY")
        .env("ANTHROPIC_API_KEY", "PROVIDER_CANARY")
        .env_remove("DATABASE_URL")
        .env("RUST_LOG", "trace,rmcp=trace")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{mode}/{signal}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "subprocess helper: invoked by CLI boundary tests with isolated synthetic environment"]
fn cli_scenario() {
    // Harness arguments are invalid runner arguments; entry must install the
    // production logging policy before parsing or any SDK use.
    assert_eq!(entry(), Err(RunnerError::Config));
    let mode = std::env::var("CLI_TEST_MODE").unwrap();
    let root = PathBuf::from(std::env::var_os("CLI_TEST_ROOT").unwrap());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = root.join("fake-ready");
    let script = if mode == "childfail" {
        "#!/bin/sh\nprintf 'CHILD_STDERR_CANARY' >&2\nexit 7\n"
    } else {
        "#!/bin/sh\nprintf 'CHILD_STDERR_CANARY' >&2\ncase \"$1\" in\n--version) printf 'fixture\\n';;\n--help) printf '%s\\n' '--print --output-format --json-schema --tools --strict-mcp-config --mcp-config --bare --restricted --disable-slash-commands --no-session-persistence --no-chrome --setting-sources --model --max-budget-usd';;\n*) exit 99;;\nesac\n"
    };
    let stalled;
    let script = if mode.starts_with("probe-") {
        let phase = mode.rsplit('-').next().unwrap();
        stalled = format!("#!/usr/bin/python3\nimport os, sys, time\nif sys.argv[1] == '--{phase}':\n    with open({:?}, 'w') as f: f.write(str(os.getpid()))\n    time.sleep(60)\nelif sys.argv[1] == '--version': print('fixture')\nelse: print('--print --output-format --json-schema --tools --strict-mcp-config --mcp-config --bare --restricted --disable-slash-commands --no-session-persistence --no-chrome --setting-sources --model --max-budget-usd')\n", root.join("probe.pid"));
        &stalled
    } else {
        script
    };
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config: RunnerConfig = serde_json::from_value(json!({
        "endpoint":"https://example.invalid/mcp", "catalog":"synthetic", "claude_binary":binary,
        "expected_claude_version":"fixture", "model":"fake", "state_dir":root.join("state"),
        "max_run_seconds": if mode.starts_with("probe-deadline-") || (mode.starts_with("network-") && mode.contains("deadline")) { 1 } else { 1800 },
        "allow_model_upload": !matches!(mode.as_str(), "no-consent" | "budget-only"),
        "acknowledge_inflight_budget_overshoot": !matches!(mode.as_str(), "no-consent" | "upload-only")
    }))
    .unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let binding = config.binding("TOKEN_CANARY").unwrap();
    let held = if mode == "lock" {
        Some(JournalStore::open(&config.state_dir, &binding).unwrap())
    } else {
        None
    };
    if mode == "partial" {
        let store = JournalStore::open(&config.state_dir, &binding).unwrap();
        let mut journal = Journal::new(binding.clone());
        journal.pending = Some(
            PendingMutation::new(
                ToolRequest {
                    tool: AllowedTool::Claim,
                    args: json!({"batch_size":10,"idempotency_key":"original-claim-key"}),
                },
                "synthetic",
            )
            .unwrap(),
        );
        store.save(&journal).unwrap();
    }
    let seen = Arc::new(Seen::default());
    let mut backend = Fake {
        seen: seen.clone(),
        mode: mode.clone(),
        root: root.clone(),
    };
    let preflight = matches!(mode.as_str(), "preflight" | "schema" | "childfail")
        || mode.starts_with("probe-preflight-")
        || mode.contains("network-preflight");
    let cli = Cli {
        command: if preflight {
            Command::Preflight { config: path }
        } else {
            Command::RunOnce { config: path }
        },
    };
    let mut output = Vec::new();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let result = dispatch_with(cli, &mut backend, &mut output).await;
            if mode.starts_with("probe-") {
                let pid = std::fs::read_to_string(root.join("probe.pid")).unwrap();
                assert!(
                    !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                    "probe not reaped at dispatch return"
                );
            }
            result
        });
    let status = result.unwrap_or_else(|error| i32::from(error.exit_code()));
    match mode.as_str() {
        mode if mode.starts_with("network-") => {
            assert_eq!(
                result,
                Err(if mode.contains("deadline") {
                    RunnerError::Budget
                } else {
                    RunnerError::Cancelled
                })
            );
            assert_eq!(status, 4);
            assert!(output.is_empty());
            assert!(seen.calls.lock().unwrap().is_empty());
            assert_eq!(seen.inference.load(Ordering::SeqCst), 0);
        }
        mode if mode.starts_with("probe-") => {
            assert_eq!(
                result,
                Err(if mode.starts_with("probe-deadline-") {
                    RunnerError::Budget
                } else {
                    RunnerError::Cancelled
                })
            );
            assert_eq!(status, 4);
            assert!(output.is_empty());
            assert_eq!(seen.lists.load(Ordering::SeqCst), 0);
            assert!(seen.calls.lock().unwrap().is_empty());
            assert_eq!(seen.inference.load(Ordering::SeqCst), 0);
        }
        "no-consent" | "upload-only" | "budget-only" => {
            assert_eq!(status, 2);
            assert!(output.is_empty());
            assert!(!config.state_dir.exists());
            assert_eq!(seen.connect.load(Ordering::SeqCst), 0);
            assert_eq!(seen.model.load(Ordering::SeqCst), 0);
        }
        "preflight" => {
            assert_eq!(status, 0);
            assert_eq!(
                serde_json::from_slice::<Value>(&output).unwrap(),
                json!({"run_id":null,"reported":0,"inconclusive":0,"failed":0,"skipped":0,"invocations":0,"accounting_known":true,"reason":"preflight_passed"})
            );
            assert!(!config.state_dir.exists());
            assert_eq!(seen.lists.load(Ordering::SeqCst), 1);
        }
        "schema" | "childfail" => {
            assert_eq!(status, 2);
            assert!(output.is_empty());
            assert!(!config.state_dir.exists());
        }
        "lock" => {
            assert_eq!(status, 3);
            assert!(output.is_empty());
            assert_eq!(seen.connect.load(Ordering::SeqCst), 0);
        }
        _ => {
            let summary: RunSummary = serde_json::from_slice(&output).unwrap();
            assert_eq!(
                status,
                if mode == "partial" || mode == "signal" {
                    4
                } else {
                    0
                }
            );
            assert_eq!(summary.run_id.as_deref(), Some("run"));
            if mode == "aging" {
                assert_eq!(summary.inconclusive, 1);
                assert_eq!(summary.invocations, 1);
            }
            if mode == "empty" {
                assert_eq!(summary.reason, "empty");
                assert_eq!(summary.invocations, 0);
            }
            if mode == "partial" {
                assert_eq!(summary.reason, "contract");
                assert_eq!(summary.invocations, 0);
            }
            if mode == "signal" {
                assert_eq!(summary.reason, "cancelled");
                assert!(root.join("reaped").exists());
            }
            let store = JournalStore::open(&config.state_dir, &binding).unwrap();
            let j = store.load().unwrap().unwrap();
            assert_eq!(j.binding, binding);
            assert_eq!(j.terminal, mode != "partial");
        }
    }
    if preflight {
        assert_eq!(seen.model.load(Ordering::SeqCst), 0);
        assert_eq!(seen.inference.load(Ordering::SeqCst), 0);
        assert!(seen.calls.lock().unwrap().is_empty());
    }
    assert!(!String::from_utf8(output).unwrap().contains("CANARY"));
    drop(held);
}
