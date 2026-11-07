use super::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/investigator/claude/success.json");
// Short test deadlines must not be consumed by another test holding the global process gate.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const HELP: &str = "--print --output-format --json-schema --tools --strict-mcp-config --mcp-config --bare --restricted --disable-slash-commands --no-session-persistence --no-chrome --setting-sources --model --max-budget-usd";

fn envelope(cost: &str) -> Vec<u8> {
    String::from_utf8(FIXTURE.to_vec())
        .unwrap()
        .replace("0.1,", &format!("{cost},"))
        .into_bytes()
}

#[test]
fn investigator_claude_envelope_requires_accounting() {
    let parsed = parse_envelope(FIXTURE).unwrap();
    assert_eq!(
        parsed.usage,
        ModelUsage {
            input: 11,
            output: 7,
            cache_creation: 3,
            cache_read: 5,
            cost_microusd: 100_000
        }
    );
    for key in [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ] {
        for bad in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!("1"),
            json!(18446744073709551616_u128.to_string()),
        ] {
            let mut value: Value = serde_json::from_slice(FIXTURE).unwrap();
            value["usage"][key] = bad;
            assert_eq!(
                parse_envelope(&serde_json::to_vec(&value).unwrap()).err(),
                Some(RunnerError::UnknownAccounting)
            );
        }
        let mut value: Value = serde_json::from_slice(FIXTURE).unwrap();
        value["usage"].as_object_mut().unwrap().remove(key);
        assert_eq!(
            parse_envelope(&serde_json::to_vec(&value).unwrap()).err(),
            Some(RunnerError::UnknownAccounting)
        );
    }
    let mut value: Value = serde_json::from_slice(FIXTURE).unwrap();
    value["usage"]["input_tokens"] = json!(u64::MAX);
    assert_eq!(
        parse_envelope(&serde_json::to_vec(&value).unwrap()).err(),
        Some(RunnerError::UnknownAccounting)
    );
    for (cost, expected) in [
        ("0", 0),
        ("0.0000001", 1),
        ("1e-7", 1),
        ("1.00000000000000000001", 1_000_001),
        ("18446744073709.551615", u64::MAX),
        ("0.1", 100_000),
        ("1e1", 10_000_000),
    ] {
        assert_eq!(
            parse_envelope(&envelope(cost)).unwrap().usage.cost_microusd,
            expected,
            "{cost}"
        );
    }
    for cost in ["-0", "-1", "null", "\"0.1\"", "1e10000", "1e-10000", "18446744073709.5516151", "NaN", "Infinity", "0.000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001"] {
        assert!(parse_envelope(&envelope(cost)).is_err(), "{cost}");
    }
    for key in [
        "type",
        "subtype",
        "is_error",
        "structured_output",
        "total_cost_usd",
        "usage",
    ] {
        let mut value: Value = serde_json::from_slice(FIXTURE).unwrap();
        value.as_object_mut().unwrap().remove(key);
        assert!(
            parse_envelope(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{key}"
        );
    }
    for (key, bad) in [
        ("type", json!("message")),
        ("subtype", json!("error")),
        ("is_error", json!(true)),
        ("structured_output", json!("plain text")),
        (
            "structured_output",
            json!({"summary":"s","findings":[],"extra":true}),
        ),
    ] {
        let mut value: Value = serde_json::from_slice(FIXTURE).unwrap();
        value[key] = bad;
        assert!(parse_envelope(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    assert!(parse_envelope(b"private source text").is_err());
}

const OBJECT_FIXTURE: &str =
    include_str!("../../tests/fixtures/investigator/claude/object-findings.json");
const OBJECT_USAGE: &str = r#"{"input_tokens":11,"output_tokens":7,"cache_creation_input_tokens":3,"cache_read_input_tokens":5}"#;
const OBJECT_EVIDENCE: &str = r#"{"source_id":"source-1","excerpt":"Synthetic excerpt"}"#;
const OBJECT_FINDING: &str = r#"{"category":"suggested_link","assertion":"Synthetic assertion","follow_up":"Synthetic follow-up","uncertainty":"Synthetic uncertainty","relationship":"inference","evidence":[{"source_id":"source-1","excerpt":"Synthetic excerpt"}]}"#;

#[test]
fn investigator_claude_rejects_object_encoded_proposal_enums() {
    for (field, variant) in [
        ("category", "suggested_link"),
        ("relationship", "inference"),
    ] {
        let old = format!(r#""{field}":"{variant}""#);
        let new = format!(r#""{field}":{{"{variant}":null}}"#);
        let bytes = OBJECT_FIXTURE.replace(&old, &new);
        assert_ne!(bytes, OBJECT_FIXTURE);
        assert_eq!(
            parse_envelope(bytes.as_bytes()).err(),
            Some(RunnerError::InvalidProposal)
        );
    }
}

#[test]
fn investigator_claude_rejects_positional_envelope() {
    let bytes =
        format!(r#"["result","success",false,{{"summary":"s","findings":[]}},0.1,{OBJECT_USAGE}]"#);
    assert_eq!(
        parse_envelope(bytes.as_bytes()).err(),
        Some(RunnerError::UnknownAccounting)
    );
}

#[test]
fn investigator_claude_rejects_positional_usage() {
    let bytes = OBJECT_FIXTURE.replace(OBJECT_USAGE, "[11,7,3,5]");
    assert_eq!(
        parse_envelope(bytes.as_bytes()).err(),
        Some(RunnerError::UnknownAccounting)
    );
}

#[test]
fn investigator_claude_rejects_positional_proposal() {
    let bytes = String::from_utf8(FIXTURE.to_vec()).unwrap().replace(
        r#"{"summary":"Synthetic test proposal, not captured from Claude","findings":[]}"#,
        r#"["summary",[]]"#,
    );
    assert_eq!(
        parse_envelope(bytes.as_bytes()).err(),
        Some(RunnerError::InvalidProposal)
    );
}

#[test]
fn investigator_claude_rejects_positional_finding() {
    let bytes = OBJECT_FIXTURE.replace(OBJECT_FINDING, &format!(
        r#"["suggested_link","Synthetic assertion","Synthetic follow-up","Synthetic uncertainty","inference",[{OBJECT_EVIDENCE}]]"#,
    ));
    assert_eq!(
        parse_envelope(bytes.as_bytes()).err(),
        Some(RunnerError::InvalidProposal)
    );
}

#[test]
fn investigator_claude_rejects_positional_evidence() {
    let bytes = OBJECT_FIXTURE.replace(OBJECT_EVIDENCE, r#"["source-1","Synthetic excerpt"]"#);
    assert_eq!(
        parse_envelope(bytes.as_bytes()).err(),
        Some(RunnerError::InvalidProposal)
    );
}

#[test]
fn investigator_claude_object_fixture_preserves_exact_cost_and_lists() {
    // All mutations use raw JSON text: fixture precision never passes through Value.
    for (cost, expected) in [
        ("1.00000000000000000001", 1_000_001),
        ("0.00000000000000000001", 1),
        ("18446744073709.551615", u64::MAX),
    ] {
        let bytes = OBJECT_FIXTURE.replace("1.00000000000000000001", cost);
        let result = parse_envelope(bytes.as_bytes()).unwrap();
        assert_eq!(result.usage.cost_microusd, expected);
        assert_eq!(result.proposal.findings.len(), 1);
        assert_eq!(result.proposal.findings[0].evidence.len(), 1);
        assert_eq!(
            result.proposal.findings[0].evidence[0].source_id,
            "source-1"
        );
    }
    let two_evidence = OBJECT_FINDING.replace(
        OBJECT_EVIDENCE,
        &format!("{OBJECT_EVIDENCE},{OBJECT_EVIDENCE}"),
    );
    let bytes = OBJECT_FIXTURE.replace(OBJECT_FINDING, &format!("{two_evidence},{two_evidence}"));
    let result = parse_envelope(bytes.as_bytes()).unwrap();
    assert_eq!(result.proposal.findings.len(), 2);
    assert!(result
        .proposal
        .findings
        .iter()
        .all(|finding| finding.evidence.len() == 2));
    assert_eq!(result.usage.cost_microusd, 1_000_001);
    assert_eq!(
        parse_envelope(
            OBJECT_FIXTURE
                .replace("1.00000000000000000001", "18446744073709.5516151")
                .as_bytes()
        )
        .err(),
        Some(RunnerError::UnknownAccounting)
    );
}

#[test]
fn investigator_claude_object_parser_rejects_duplicate_fields() {
    // Map conversion must not collapse duplicates, even equal exact-cost lexemes.
    for replacement in [
        r#""total_cost_usd":1.00000000000000000001,"total_cost_usd":1.00000000000000000001"#,
        r#""total_cost_usd":0,"total_cost_usd":1.00000000000000000001"#,
        r#""total_cost_usd":1.00000000000000000001,"total_cost_usd":0"#,
    ] {
        let bytes =
            OBJECT_FIXTURE.replace(r#""total_cost_usd":1.00000000000000000001"#, replacement);
        assert_eq!(
            parse_envelope(bytes.as_bytes()).err(),
            Some(RunnerError::UnknownAccounting)
        );
    }
    for (old, new, expected) in [
        (
            r#""input_tokens":11"#,
            r#""input_tokens":11,"input_tokens":11"#,
            RunnerError::UnknownAccounting,
        ),
        (
            r#""summary":"Synthetic object proposal""#,
            r#""summary":"Synthetic object proposal","summary":"Synthetic object proposal""#,
            RunnerError::InvalidProposal,
        ),
        (
            r#""assertion":"Synthetic assertion""#,
            r#""assertion":"Synthetic assertion","assertion":"Synthetic assertion""#,
            RunnerError::InvalidProposal,
        ),
        (
            r#""source_id":"source-1""#,
            r#""source_id":"source-1","source_id":"source-1""#,
            RunnerError::InvalidProposal,
        ),
    ] {
        assert_eq!(
            parse_envelope(OBJECT_FIXTURE.replace(old, new).as_bytes()).err(),
            Some(expected)
        );
    }
}

struct Fake {
    dir: tempfile::TempDir,
    config: RunnerConfig,
}
impl Fake {
    fn new(mut scenario: Value) -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let binary = dir.path().join("fake-child");
        std::fs::write(
            &binary,
            include_bytes!("../../tests/fixtures/investigator/claude/fake_child.py"),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.path().join("success.json"), FIXTURE).unwrap();
        if scenario.get("help").is_none() {
            scenario["help"] = json!(HELP);
        }
        std::fs::write(
            dir.path().join("scenario.json"),
            serde_json::to_vec(&scenario).unwrap(),
        )
        .unwrap();
        let config = serde_json::from_value(json!({
            "endpoint":"https://example.invalid/mcp", "catalog":"synthetic", "claude_binary":binary,
            "expected_claude_version":"2.1.319 (Claude Code)", "model":"synthetic-model", "state_dir":dir.path(),
            "allow_model_upload":true, "acknowledge_inflight_budget_overshoot":true
        })).unwrap();
        Self { dir, config }
    }
    fn adapter(&self) -> ClaudeAdapter {
        ClaudeAdapter {
            binary: self.config.claude_binary.clone(),
            model: self.config.model.clone(),
            reservation: self.config.invocation_microusd,
            provider_key: "synthetic-not-a-key".into(),
        }
    }
    fn record(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.path().join("record.json")).unwrap())
            .unwrap()
    }
    async fn started(&self) {
        for _ in 0..1000 {
            if self.dir.path().join("record.json").exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("fake did not start");
    }
    fn reaped(&self) {
        let pid = self.record()["pid"].as_u64().unwrap();
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "owned child not reaped"
        );
    }
}
fn request() -> ModelRequest {
    ModelRequest {
        prompt: "PRIVATE prompt --tools shell --model attacker".into(),
        model: "synthetic-model".into(),
        budget_usd: "0.100000".into(),
    }
}

#[tokio::test]
async fn investigator_claude_has_no_tools_or_mcp_secret() {
    let _test = TEST_LOCK.lock().await;
    let fake = Fake::new(json!({}));
    preflight_cli(&fake.config).await.unwrap();
    assert_eq!(fake.record()["argv"], json!(["--help"]));
    assert!(fake.record()["env"].get("ANTHROPIC_API_KEY").is_none());
    let adapter = fake.adapter();
    adapter
        .infer(request(), CancellationToken::new())
        .await
        .unwrap();
    let record = fake.record();
    let args = record["argv"].as_array().unwrap();
    let args: Vec<&str> = args.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(args.len(), 21);
    assert_eq!(&args[..3], &["--print", "--output-format", "json"]);
    assert_eq!(args[3], "--json-schema");
    assert_eq!(
        serde_json::from_str::<Value>(args[4]).unwrap(),
        super::super::proposal::proposal_schema()
    );
    assert_eq!(
        &args[5..],
        &[
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--bare",
            "--restricted",
            "--disable-slash-commands",
            "--no-session-persistence",
            "--no-chrome",
            "--setting-sources",
            "",
            "--model",
            "synthetic-model",
            "--max-budget-usd",
            "0.100000"
        ]
    );
    let env = record["env"].as_object().unwrap();
    for key in env.keys() {
        assert!(
            [
                "PATH",
                "HOME",
                "TMPDIR",
                "LANG",
                "ANTHROPIC_API_KEY",
                "LC_CTYPE"
            ]
            .contains(&key.as_str()),
            "unexpected env name {key}"
        );
    }
    assert_eq!(env["ANTHROPIC_API_KEY"], "synthetic-not-a-key");
    assert_eq!(env["HOME"], record["cwd"]);
    assert_eq!(env["TMPDIR"], record["cwd"]);
    assert_eq!(record["mode"], 448);
    assert!(!std::path::Path::new(record["cwd"].as_str().unwrap()).exists());
    assert_eq!(
        std::fs::read(fake.dir.path().join("stdin")).unwrap(),
        request().prompt.as_bytes()
    );
    fake.reaped();
    for (model, budget) in [
        ("attacker", "0.1"),
        ("synthetic-model", "0.100001"),
        ("synthetic-model", "0.099999"),
        ("synthetic-model", "0.1000000001"),
        ("synthetic-model", "0.0999999999"),
    ] {
        let mut req = request();
        req.model = model.into();
        req.budget_usd = budget.into();
        assert_eq!(
            adapter.infer(req, CancellationToken::new()).await.err(),
            Some(RunnerError::Config)
        );
    }
    let mut req = request();
    req.prompt = "é".repeat(65_536);
    adapter
        .infer(req.clone(), CancellationToken::new())
        .await
        .unwrap();
    req.prompt.push('x');
    assert_eq!(
        adapter.infer(req, CancellationToken::new()).await.err(),
        Some(RunnerError::Config)
    );
}

#[tokio::test]
async fn investigator_claude_preflight_exact_and_bounded() {
    let _test = TEST_LOCK.lock().await;
    for flag in HELP.split_whitespace() {
        let help = HELP
            .split_whitespace()
            .filter(|token| *token != flag)
            .collect::<Vec<_>>()
            .join(" ");
        let fake = Fake::new(json!({"help":help}));
        assert_eq!(
            preflight_cli(&fake.config).await.err(),
            Some(RunnerError::Contract),
            "missing {flag}"
        );
        fake.reaped();
    }
    for scenario in [
        json!({"version":"2.1.319 (Claude Code) extra"}),
        json!({"help":HELP.replace("--bare", "--barely")}),
        json!({"version":"X".repeat(1_048_577)}),
        json!({"help":"X".repeat(1_048_577)}),
    ] {
        let fake = Fake::new(scenario);
        assert_eq!(
            preflight_cli(&fake.config).await.err(),
            Some(RunnerError::Contract)
        );
        assert!(fake.record()["env"].get("ANTHROPIC_API_KEY").is_none());
        fake.reaped();
    }
}

#[tokio::test]
async fn investigator_claude_bounds_pipes_and_reaps() {
    let _test = TEST_LOCK.lock().await;
    for (stdout, stderr, valid) in [
        (1_048_576, 65_536, true),
        (1_048_577, 0, false),
        (373, 65_537, false),
        (2_000_000, 2_000_000, false),
    ] {
        let fake = Fake::new(json!({"stdout":stdout,"stderr":stderr}));
        let result = fake
            .adapter()
            .infer(request(), CancellationToken::new())
            .await;
        assert_eq!(result.is_ok(), valid);
        fake.reaped();
    }
    let fake = Fake::new(json!({"exit":7,"stderr":1024}));
    let err = fake
        .adapter()
        .infer(request(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert_eq!(err, RunnerError::Child);
    assert!(!err.to_string().contains("SSSS"));
    fake.reaped();
}

#[tokio::test]
async fn investigator_claude_timeout_includes_pipe_eof() {
    let _test = TEST_LOCK.lock().await;
    let fake = Fake::new(json!({"hold_pipes":true}));
    let begin = std::time::Instant::now();
    let result = run_process(
        fake.config.claude_binary.clone(),
        vec![],
        None,
        vec![],
        CancellationToken::new(),
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(result.err(), Some(RunnerError::Child));
    assert!(begin.elapsed() < Duration::from_millis(2900));
    fake.reaped();
    // Allow the deliberately short-lived fake pipe holder to exit naturally.
    tokio::time::sleep(Duration::from_secs(3)).await;
}

#[tokio::test]
async fn investigator_claude_drop_and_serialization() {
    let _test = TEST_LOCK.lock().await;
    let first = Fake::new(json!({"stall":true}));
    let second = Fake::new(json!({}));
    let adapter = first.adapter();
    let running =
        tokio::spawn(async move { adapter.infer(request(), CancellationToken::new()).await });
    first.started().await;
    let cancelled = CancellationToken::new();
    let signal = cancelled.clone();
    let adapter = second.adapter();
    let queued = tokio::spawn(async move { adapter.infer(request(), signal).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !second.dir.path().join("record.json").exists(),
        "two concurrent children"
    );
    cancelled.cancel();
    assert_eq!(queued.await.unwrap().err(), Some(RunnerError::Cancelled));
    running.abort();
    let _ = running.await;
    let pid = first.record()["pid"].as_u64().unwrap();
    for _ in 0..1000 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    first.reaped();
    second
        .adapter()
        .infer(request(), CancellationToken::new())
        .await
        .unwrap();
}

#[test]
fn investigator_claude_constructor_environment_probe() {
    // Only the explicitly env-cleared subprocess below loads Secrets::from_env.
    if std::env::var("TASK5_SYNTHETIC_PROBE").as_deref() != Ok("yes") {
        return;
    }
    let fake = Fake::new(json!({}));
    let secrets = Secrets::from_env().unwrap();
    let adapter = ClaudeAdapter::new(&fake.config, &secrets).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(adapter.infer(request(), CancellationToken::new()))
        .unwrap();
    let env = fake.record()["env"].clone();
    for key in [
        "OKF_MAINTENANCE_TOKEN",
        "DATABASE_URL",
        "HTTPS_PROXY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "NODE_OPTIONS",
        "TASK5_SYNTHETIC_PROBE",
    ] {
        assert!(
            env.get(key).is_none(),
            "inherited forbidden environment name {key}"
        );
    }
    assert_eq!(env["PATH"], "/usr/local/bin:/usr/bin:/bin");
    assert_eq!(env["LANG"], "C.UTF-8");
    let mut config = fake.config.clone();
    config.allow_model_upload = false;
    assert_eq!(
        ClaudeAdapter::new(&config, &secrets).err().unwrap(),
        RunnerError::Config
    );
    config.allow_model_upload = true;
    config.acknowledge_inflight_budget_overshoot = false;
    assert_eq!(
        ClaudeAdapter::new(&config, &secrets).err().unwrap(),
        RunnerError::Config
    );
    // This process is single-test/single-owner; no concurrent env access remains.
    std::env::remove_var("ANTHROPIC_API_KEY");
    assert_eq!(Secrets::from_env().err().unwrap(), RunnerError::Config);
}

#[test]
fn investigator_claude_constructor_dedicated_key_only() {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "investigator::claude::tests::investigator_claude_constructor_environment_probe",
        "--nocapture",
    ])
    .env_clear()
    .env("TASK5_SYNTHETIC_PROBE", "yes")
    .env("ANTHROPIC_API_KEY", "synthetic-not-a-key");
    for name in [
        "OKF_MAINTENANCE_TOKEN",
        "DATABASE_URL",
        "HTTPS_PROXY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "NODE_OPTIONS",
    ] {
        cmd.env(name, "synthetic-forbidden-sentinel");
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "synthetic constructor probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn investigator_claude_stdin_timeout_cancel_reap() {
    let _test = TEST_LOCK.lock().await;
    for cancel_now in [false, true] {
        let fake = Fake::new(json!({"stall":true}));
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        let binary = fake.config.claude_binary.clone();
        let task = tokio::spawn(async move {
            run_process(
                binary,
                vec!["--print".into()],
                Some("synthetic".into()),
                vec![b'x'; 131_072],
                child_cancel,
                Duration::from_millis(200),
            )
            .await
        });
        fake.started().await;
        if cancel_now {
            cancel.cancel();
        }
        assert_eq!(
            task.await.unwrap().err(),
            Some(if cancel_now {
                RunnerError::Cancelled
            } else {
                RunnerError::Child
            })
        );
        fake.reaped();
    }
    let fake = Fake::new(json!({"after_output_stall":true}));
    assert_eq!(
        run_process(
            fake.config.claude_binary.clone(),
            vec![],
            None,
            vec![],
            CancellationToken::new(),
            Duration::from_millis(200)
        )
        .await
        .err(),
        Some(RunnerError::Child)
    );
    fake.reaped();
}
