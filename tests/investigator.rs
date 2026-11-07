use okf_mcp::investigator::config::{RunnerConfig, Secrets};
use okf_mcp::investigator::error::RunnerError;
use std::process::Command;

#[path = "support/investigator_evaluation.rs"]
mod evaluation;

#[cfg(unix)]
#[path = "support/investigator_acceptance.rs"]
mod acceptance;
mod support;

const BASE: &str = r#"
endpoint = "https://EXAMPLE.com:443/mcp?catalog=pg"
catalog = "pg"
claude_binary = "/usr/bin/claude"
expected_claude_version = "2.0.0"
model = "explicit-model"
state_dir = "/tmp/maintenance"
"#;

fn parse(extra: &str) -> RunnerConfig {
    toml::from_str(&format!("{BASE}\n{extra}")).unwrap()
}

#[test]
fn investigator_config_requires_explicit_consent() {
    let defaults = parse("");
    defaults.validate().unwrap();
    assert!(!defaults.allow_model_upload);
    assert!(!defaults.acknowledge_inflight_budget_overshoot);
    assert!(!defaults.require_hard_billing_cap);
    assert_eq!(defaults.validate_consent(), Err(RunnerError::Config));
    for extra in [
        "allow_model_upload = true",
        "acknowledge_inflight_budget_overshoot = true",
    ] {
        assert!(parse(extra).validate_consent().is_err());
    }
    parse("allow_model_upload = true\nacknowledge_inflight_budget_overshoot = true")
        .validate_consent()
        .unwrap();
    assert!(parse("require_hard_billing_cap = true").validate().is_err());
}

#[test]
fn investigator_config_bounds_are_exact() {
    let defaults = parse("");
    assert_eq!(defaults.batch_size, 10);
    assert_eq!(defaults.max_invocations, 10);
    assert_eq!(defaults.max_run_seconds, 1_800);
    assert_eq!(defaults.invocation_microusd, 100_000);
    assert_eq!(defaults.run_microusd, 1_000_000);
    assert_eq!(defaults.observed_token_stop, 100_000);
    assert_eq!(parse("batch_size = 1").max_invocations, 1);
    assert_eq!(parse("batch_size = 50").max_invocations, 10);
    for extra in [
        "batch_size = 0",
        "batch_size = 51",
        "max_invocations = 0",
        "max_invocations = 11",
        "max_invocations = 51",
        "max_run_seconds = 0",
        "max_run_seconds = 3001",
        "invocation_microusd = 0",
        "run_microusd = 0",
        "observed_token_stop = 0",
        "invocation_microusd = 1000001",
    ] {
        assert!(parse(extra).validate().is_err(), "{extra}");
    }
    parse("batch_size = 50\nmax_invocations = 50\nmax_run_seconds = 3000\ninvocation_microusd = 1\nrun_microusd = 1\nobserved_token_stop = 18446744073709551615").validate().unwrap();
    for extra in [
        "token = 'secret'",
        "anthropic_api_key = 'secret'",
        "typo = 1",
        "batch_size = -1",
        "max_run_seconds = 18446744073709551616",
    ] {
        assert!(toml::from_str::<RunnerConfig>(&format!("{BASE}\n{extra}")).is_err());
    }
}

#[test]
fn investigator_config_required_inputs_and_load_redaction() {
    for (old, new) in [
        (
            "claude_binary = \"/usr/bin/claude\"",
            "claude_binary = \"claude\"",
        ),
        ("state_dir = \"/tmp/maintenance\"", "state_dir = \"state\""),
        ("model = \"explicit-model\"", "model = \" \""),
        (
            "expected_claude_version = \"2.0.0\"",
            "expected_claude_version = \"\"",
        ),
        ("catalog = \"pg\"", "catalog = \"\""),
    ] {
        assert!(toml::from_str::<RunnerConfig>(&BASE.replace(old, new))
            .unwrap()
            .validate()
            .is_err());
    }
    for line in BASE.lines().filter(|line| line.contains('=')) {
        assert!(toml::from_str::<RunnerConfig>(&BASE.replace(line, "")).is_err());
    }
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), BASE).unwrap();
    RunnerConfig::load(file.path()).unwrap();
    std::fs::write(file.path(), "token = 'never-echo-this'").unwrap();
    let error = RunnerConfig::load(file.path()).err().unwrap();
    assert_eq!(error.to_string(), "runner configuration is invalid");
    assert!(!format!("{error:?}").contains("never-echo-this"));
    assert_eq!(
        RunnerConfig::load(std::path::Path::new("/nonexistent/config.toml")).err(),
        Some(RunnerError::Io)
    );
}

#[test]
fn investigator_binding_includes_full_endpoint() {
    let config = parse("");
    let binding = config.binding("token-one").unwrap();
    assert_eq!(binding.origin, "https://example.com");
    assert_eq!(binding.endpoint, "https://example.com/mcp?catalog=pg");
    assert_eq!(binding.catalog, "pg");
    // Independent known answers from sha256sum over these exact bytes.
    assert_eq!(
        binding.credential_sha256,
        "75c5c10f256c75b650c6ffc2e83c6588af02766efd2de28f4de34547006e8d1a"
    );
    assert_eq!(
        config.binding("token-one ").unwrap().credential_sha256,
        "e57791c4c79696bbe1c6882ff7f20685e20b7043d762652560b0e2f3f9e3d7d8"
    );
    assert!(!format!("{binding:?}").contains("token-one"));
    assert_eq!(binding, config.binding("token-one").unwrap());
    assert_ne!(binding, config.binding("token-two").unwrap());
    let mut other = parse("");
    other.endpoint = "https://example.com/other?catalog=pg".into();
    assert_ne!(binding, other.binding("token-one").unwrap());
    other.endpoint = "https://example.com/mcp?catalog=other".into();
    assert_ne!(binding, other.binding("token-one").unwrap());
    other.catalog = "other".into();
    other.endpoint = config.endpoint.clone();
    assert_ne!(binding, other.binding("token-one").unwrap());
    for endpoint in [
        "http://example.com/mcp",
        "https://user:secret@example.com/mcp",
        "https://example.com/mcp#fragment",
        "https://secret@[",
        "not-a-url",
    ] {
        other.endpoint = endpoint.into();
        let error = other.binding("token-one").unwrap_err();
        assert_eq!(error, RunnerError::Config);
        assert!(!format!("{error} {error:?}").contains("secret"));
    }
    assert!(config.binding("").is_err());
}

#[test]
fn investigator_config_secrets_environment_isolated() {
    for (token, key, valid) in [
        (None, None, false),
        (Some("token"), None, false),
        (None, Some("api-key"), false),
        (Some(""), Some("api-key"), false),
        (Some("token"), Some(" "), false),
        (Some("token"), Some("api-key"), true),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "investigator_config_secrets_child",
                "--nocapture",
            ])
            .env_remove("OKF_MAINTENANCE_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env(
                "INVESTIGATOR_ENV_FIXTURE",
                if valid { "valid" } else { "invalid" },
            );
        if let Some(token) = token {
            child.env("OKF_MAINTENANCE_TOKEN", token);
        }
        if let Some(key) = key {
            child.env("ANTHROPIC_API_KEY", key);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("api-key"));
    }
}

#[test]
fn investigator_config_secrets_child() {
    let Ok(fixture) = std::env::var("INVESTIGATOR_ENV_FIXTURE") else {
        return;
    };
    let result = Secrets::from_env();
    if fixture == "valid" {
        let secrets = result.ok().unwrap();
        assert_eq!(secrets.token(), "token");
        assert_eq!(secrets.anthropic_api_key(), "api-key");
    } else {
        assert_eq!(result.err(), Some(RunnerError::Config));
    }
    // Permission/401 failures carry no secret-bearing transport response.
    assert_eq!(
        RunnerError::Permission.to_string(),
        "runner permission denied"
    );
}

#[test]
fn investigator_contracts_are_closed_and_errors_have_exit_codes() {
    use okf_mcp::investigator::types::{AllowedTool, Proposal, ToolRequest};
    let tools = [
        AllowedTool::Search,
        AllowedTool::GetConcept,
        AllowedTool::History,
        AllowedTool::Claim,
        AllowedTool::Renew,
        AllowedTool::ReadRevision,
        AllowedTool::Submit,
        AllowedTool::Close,
        AllowedTool::GetRun,
    ];
    let expected = [
        "search",
        "get_concept",
        "history",
        "maintenance_claim",
        "maintenance_renew",
        "maintenance_read_revision",
        "maintenance_submit",
        "maintenance_close",
        "maintenance_get_run",
    ];
    assert_eq!(tools.map(|tool| tool.name()), expected);
    assert!(serde_json::from_str::<ToolRequest>(r#"{"tool":"publish","args":{}}"#).is_err());
    assert!(
        serde_json::from_str::<Proposal>(r#"{"summary":"x","findings":[],"extra":true}"#).is_err()
    );
    let proposal: Proposal = serde_json::from_str(r#"{"summary":"x","findings":[{"category":"suggested_link","assertion":"a","follow_up":"f","uncertainty":"u","relationship":"inference","evidence":[{"source_id":"s1","excerpt":"e"}]}]}"#).unwrap();
    assert_eq!(proposal.findings[0].evidence[0].source_id, "s1");
    let value = serde_json::to_value(&proposal).unwrap();
    for pointer in ["", "/findings/0", "/findings/0/evidence/0"] {
        let mut unknown = value.clone();
        unknown
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<Proposal>(unknown).is_err());
    }
    for (error, code) in [
        (RunnerError::Config, 2),
        (RunnerError::Contract, 2),
        (RunnerError::Busy, 3),
        (RunnerError::Permission, 4),
        (RunnerError::Transport, 4),
        (RunnerError::LeaseLost, 4),
        (RunnerError::Budget, 4),
        (RunnerError::InvalidEvidence, 4),
        (RunnerError::InvalidProposal, 4),
        (RunnerError::UnknownAccounting, 4),
        (RunnerError::Child, 4),
        (RunnerError::Journal, 4),
        (RunnerError::Cancelled, 4),
        (RunnerError::Io, 4),
    ] {
        assert_eq!(error.exit_code(), code);
    }
}

#[cfg(target_os = "linux")]
mod journal_process {
    use super::*;
    use okf_mcp::investigator::{
        client::CatalogClient,
        journal::{Journal, JournalStore, PendingMutation, Phase, WorkState},
        types::{AllowedTool, ToolRequest},
    };
    use okf_mcp::maintenance::types::{new_id, ClaimResult};
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Child, Stdio},
    };

    fn binding() -> okf_mcp::investigator::types::Binding {
        parse("").binding("bearer-never-persist").unwrap()
    }

    fn child(dir: &Path, action: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "journal_process::investigator_journal_child",
                "--nocapture",
            ])
            .env("JOURNAL_TEST_DIR", dir)
            .env("JOURNAL_TEST_ACTION", action)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn ready(child: &mut Child) {
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.unwrap().trim() == "journal-ready" {
                    let _ = sender.send(());
                    return;
                }
            }
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(10));
        if result.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        reader.join().unwrap();
        result.expect("child did not reach journal boundary");
    }

    struct FakeClient<'a> {
        dir: &'a Path,
        lose_reply: bool,
    }
    impl CatalogClient for FakeClient<'_> {
        async fn list_tools(&mut self) -> Result<Vec<rmcp::model::Tool>, RunnerError> {
            unreachable!()
        }
        async fn call(&mut self, request: ToolRequest) -> Result<Value, RunnerError> {
            assert_eq!(request.tool, AllowedTool::Claim);
            assert_eq!(request.args["catalog"], "pg");
            let path = self.dir.join("remote.json");
            let mut calls: Vec<Value> = std::fs::read(&path)
                .map(|v| serde_json::from_slice(&v).unwrap())
                .unwrap_or_default();
            if let Some(previous) = calls.first() {
                assert_eq!(*previous, request.args);
            }
            calls.push(request.args);
            std::fs::write(path, serde_json::to_vec(&calls).unwrap()).unwrap();
            if self.lose_reply {
                return Err(RunnerError::Transport);
            }
            Ok(
                json!({"run_id":"recovered-run","leases":[{"run_id":"recovered-run","work_id":"work-1","token":"private-lease-token","pin":{"path":"/x.md","revision":1},"reasons":["changed"],"expires_at":"2026-11-07T12:00:00Z"}]}),
            )
        }
    }

    async fn replay(store: &JournalStore, dir: &Path, stop_after_remote: bool) {
        let mut state = store.load().unwrap().unwrap();
        let pending = state.pending.as_ref().unwrap();
        let response = FakeClient {
            dir,
            lose_reply: stop_after_remote,
        }
        .call(pending.request.clone())
        .await;
        if stop_after_remote {
            assert_eq!(response, Err(RunnerError::Transport));
            assert!(store.load().unwrap().unwrap().pending.is_some());
            return;
        }
        let result: ClaimResult = serde_json::from_value(response.unwrap()).unwrap();
        state.run_id = Some(result.run_id);
        state.work = result
            .leases
            .iter()
            .map(|lease| WorkState {
                work_id: lease.work_id.clone(),
                phase: Phase::Claimed,
                packet: None,
                proposal: None,
                usage: None,
                failure: None,
            })
            .collect();
        state.leases = result.leases;
        state.pending = None;
        store.save(&state).unwrap();
    }

    #[tokio::test]
    async fn investigator_journal_child() {
        let Some(dir) = std::env::var_os("JOURNAL_TEST_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let action = std::env::var("JOURNAL_TEST_ACTION").unwrap();
        if action == "probe" {
            assert!(matches!(
                JournalStore::open(&dir, &binding()),
                Err(RunnerError::Busy)
            ));
            return;
        }
        let store = JournalStore::open(&dir, &binding()).unwrap();
        if action == "hold" {
            for _ in 0..4 {
                store.save(&Journal::new(binding())).unwrap();
            }
        } else if action != "before-remote" {
            replay(&store, &dir, action == "remote-success-before-save").await;
        }
        println!("journal-ready");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn investigator_journal_lock_survives_atomic_save() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let store = JournalStore::open(dir.path(), &binding()).unwrap();
        let inode = std::fs::metadata(dir.path().join("lock")).unwrap().ino();
        for _ in 0..4 {
            store.save(&Journal::new(binding())).unwrap();
            assert_eq!(
                std::fs::metadata(dir.path().join("lock")).unwrap().ino(),
                inode
            );
            assert!(child(dir.path(), "probe")
                .wait_with_output()
                .unwrap()
                .status
                .success());
        }
        drop(store);
        let mut owner = child(dir.path(), "hold");
        ready(&mut owner);
        let contention = JournalStore::open(dir.path(), &binding());
        owner.kill().unwrap();
        owner.wait().unwrap();
        assert!(matches!(contention, Err(RunnerError::Busy)));
        let store = JournalStore::open(dir.path(), &binding()).unwrap();
        assert!(store.load().unwrap().is_some());
        assert_eq!(
            std::fs::metadata(dir.path().join("lock")).unwrap().ino(),
            inode
        );
    }

    #[tokio::test]
    async fn investigator_journal_replays_lost_claim() {
        for boundary in ["before-remote", "remote-success-before-save", "after-save"] {
            let dir = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in(std::env::current_dir().unwrap())
                .unwrap();
            let key = new_id().unwrap();
            let original_args;
            {
                let store = JournalStore::open(dir.path(), &binding()).unwrap();
                let mut state = Journal::new(binding());
                state.pending = Some(
                    PendingMutation::new(
                        ToolRequest {
                            tool: AllowedTool::Claim,
                            args: json!({"batch_size":1,"idempotency_key":key}),
                        },
                        "pg",
                    )
                    .unwrap(),
                );
                original_args = state.pending.as_ref().unwrap().request.args.clone();
                store.save(&state).unwrap();
            }
            let mut runner = child(dir.path(), boundary);
            ready(&mut runner);
            runner.kill().unwrap();
            runner.wait().unwrap();
            let store = JournalStore::open(dir.path(), &binding()).unwrap();
            let state = store.load().unwrap().unwrap();
            if boundary == "after-save" {
                assert!(state.pending.is_none());
            } else {
                let pending = state.pending.unwrap();
                assert_eq!(pending.request_id, key);
                assert_eq!(pending.request.args, original_args);
                replay(&store, dir.path(), false).await;
            }
            let state = store.load().unwrap().unwrap();
            assert_eq!(state.run_id.as_deref(), Some("recovered-run"));
            assert_eq!(state.leases[0].work_id, "work-1");
            assert_eq!(state.leases[0].token, "private-lease-token");
            assert!(state.pending.is_none());
            let calls: Vec<Value> =
                serde_json::from_slice(&std::fs::read(dir.path().join("remote.json")).unwrap())
                    .unwrap();
            let keys: std::collections::HashSet<_> = calls
                .iter()
                .map(|c| c["idempotency_key"].as_str().unwrap())
                .collect();
            assert_eq!(keys.len(), 1);
            assert!(calls.iter().all(|args| args == &original_args));
            assert_eq!(
                calls.len(),
                if boundary == "remote-success-before-save" {
                    2
                } else {
                    1
                }
            );
            let raw = std::fs::read_to_string(dir.path().join("journal.json")).unwrap();
            assert!(!raw.contains("bearer-never-persist"));
        }
    }
}
