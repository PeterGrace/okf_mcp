//! Executable boundaries: only offline evaluation and fake readiness children.

use std::process::{Command, Output};

fn runner(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_okf-maintenance"))
        .args(args)
        .env_remove("OKF_MAINTENANCE_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("DATABASE_URL")
        .env("RUST_LOG", "trace,rmcp=trace,reqwest=trace")
        .output()
        .unwrap()
}

#[test]
fn investigator_cli_evaluate_is_offline() {
    let out = runner(&[
        "evaluate",
        "--cases",
        "tests/fixtures/investigator/corpus/evaluation.json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["cases"], 11);
    assert_eq!(value["true_positive"], 4);
    assert_eq!(value["false_positive"], 2);
    assert_eq!(value["false_negative"], 3);
    assert_eq!(value["false_verification"], 0);
    assert_eq!(value["rejected_verification"], 2);
    assert_eq!(value["excluded_unreviewed"], 2);
}

#[test]
fn investigator_cli_redacts_invalid_arguments_and_config() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("SOURCE_BODY_CANARY.toml");
    std::fs::write(
        &config,
        "endpoint = 'TOKEN_CANARY'\nunknown = 'BODY_CANARY'\n",
    )
    .unwrap();
    for args in [
        vec!["TOKEN_CANARY"],
        vec![
            "run-once",
            "--config",
            "SOURCE_BODY_CANARY",
            "--token",
            "TOKEN_CANARY",
        ],
        vec!["preflight", "--config", config.to_str().unwrap()],
        vec!["run-once", "--config", config.to_str().unwrap()],
    ] {
        let out = runner(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert_eq!(out.stderr, b"runner configuration is invalid\n");
    }
    let cases = dir.path().join("cases.json");
    std::fs::write(&cases, "{\"SOURCE_BODY_CANARY\":\"TOKEN_CANARY\"}").unwrap();
    let out = runner(&["evaluate", "--cases", cases.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(out.stderr, b"runner configuration is invalid\n");
}

#[test]
fn investigator_examples_are_disabled_and_redacted() {
    use okf_mcp::investigator::{config::RunnerConfig, error::RunnerError};
    let config =
        RunnerConfig::load(std::path::Path::new("examples/maintenance/config.toml")).unwrap();
    assert!(!config.allow_model_upload);
    assert!(!config.acknowledge_inflight_budget_overshoot);
    assert_eq!(config.validate_consent(), Err(RunnerError::Config));
    assert_eq!(config.max_run_seconds, 1740);
    assert_eq!(config.expected_claude_version, "2.1.319 (Claude Code)");
    assert_eq!(config.endpoint, "https://catalog.example.invalid/mcp");
    for command in ["preflight", "run-once"] {
        let out = runner(&[command, "--config", "examples/maintenance/config.toml"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert_eq!(out.stderr, b"runner configuration is invalid\n");
    }
    let service = std::fs::read_to_string("examples/maintenance/okf-maintenance.service").unwrap();
    let timer = std::fs::read_to_string("examples/maintenance/okf-maintenance.timer").unwrap();
    for setting in [
        "User=okf-maintenance",
        "UMask=0077",
        "StateDirectoryMode=0700",
        "ProtectHome=true",
        "PrivateTmp=true",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "MemoryMax=512M",
        "TimeoutStartSec=1800",
        "KillMode=control-group",
    ] {
        assert!(service.lines().any(|line| line == setting), "{setting}");
    }
    for setting in [
        "OnCalendar=daily",
        "RandomizedDelaySec=1h",
        "Persistent=true",
        "Unit=okf-maintenance.service",
    ] {
        assert!(timer.lines().any(|line| line == setting), "{setting}");
    }
    assert!(!service.contains("ExecStartPre="));
    assert!(!service.contains("RuntimeMaxSec="));
    assert!(!service.contains("[Install]"));
    assert!(!timer.contains("[Install]"));
}

#[test]
fn investigator_cli_static_help_and_version() {
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["preflight", "--help"],
    ] {
        let out = runner(&args);
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stderr.is_empty());
        assert!(!out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stdout).contains("not yet available"));
    }
}
