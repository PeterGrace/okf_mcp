use super::*;
use crate::investigator::types::{ModelResult, ModelUsage, Proposal};
use crate::maintenance::types::{Lane, RevisionPin};
use chrono::{DateTime, Utc};
use rmcp::model::Tool;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicI64, AtomicU32, Ordering},
};

struct TestClock {
    start: Instant,
    utc: DateTime<Utc>,
    jump: AtomicI64,
    /// When `FakeClient::list_tools` first froze tokio time. Everything before it
    /// (fixture setup, the real CLI readiness queued on the process lock) is wall time.
    paused_at: std::sync::OnceLock<Instant>,
}
impl TestClock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            utc: "2026-11-07T00:00:00Z".parse().unwrap(),
            jump: AtomicI64::new(0),
            paused_at: std::sync::OnceLock::new(),
        }
    }
    /// Virtual time since the clock froze, free of the load-dependent wall-time setup.
    fn virtual_elapsed(&self) -> Duration {
        self.paused_at
            .get()
            .expect("tokio time must be paused before measuring virtual time")
            .elapsed()
    }
}
impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        self.utc
            + chrono::Duration::from_std(self.start.elapsed()).unwrap()
            + chrono::Duration::seconds(self.jump.load(Ordering::SeqCst))
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    config: RunnerConfig,
    store: JournalStore,
    clock: Arc<TestClock>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")
            .unwrap();
        let cli = dir.path().join("ready");
        std::fs::write(&cli, "#!/bin/sh\ncase \"$1\" in\n--version) printf 'fixture\\n';;\n--help) printf '%s\\n' '--print --output-format --json-schema --tools --strict-mcp-config --mcp-config --bare --restricted --disable-slash-commands --no-session-persistence --no-chrome --setting-sources --model --max-budget-usd';;\n*) exit 99;;\nesac\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config: RunnerConfig = serde_json::from_value(json!({
            "endpoint":"https://example.com/mcp", "catalog":"synthetic", "claude_binary":cli,
            "expected_claude_version":"fixture", "model":"fake", "state_dir":dir.path().join("state"),
            "allow_model_upload":true, "acknowledge_inflight_budget_overshoot":true
        })).unwrap();
        let store = JournalStore::open(
            &config.state_dir,
            &config.binding("test-only-credential").unwrap(),
        )
        .unwrap();
        Self {
            _dir: dir,
            config,
            store,
            clock: Arc::new(TestClock::new()),
        }
    }
    fn client(&self, count: usize) -> FakeClient {
        FakeClient {
            clock: self.clock.clone(),
            count,
            calls: vec![],
            replay: HashMap::new(),
            lose: None,
            fail: None,
            read_delay: 0,
            renew_latency: Duration::ZERO,
            deadline: 3600,
            run_state: "running",
            close_state: None,
            response_patch: None,
            pause: true,
            bad_schema: false,
            large: false,
            aging: false,
            replace_lease: false,
            submit_receipt: None,
            cancel_on_get_run: None,
            fault_after_submit: None,
            state_path: self.config.state_dir.join("journal.json"),
            fault_after: None,
            list_failures: 0,
            lists: 0,
        }
    }
    fn seed(&self, phase: Phase) {
        let mut j = Journal::new(self.config.binding("test-only-credential").unwrap());
        j.run_id = Some("run".into());
        j.leases = vec![lease(0, self.clock.now() + chrono::Duration::seconds(900))];
        j.work = vec![WorkState {
            work_id: "w0".into(),
            phase: Phase::Claimed,
            packet: None,
            proposal: None,
            usage: None,
            failure: None,
        }];
        self.store.save(&j).unwrap();
        if phase == Phase::Claimed {
            return;
        }
        j.work[0].phase = Phase::PacketReady;
        j.work[0].packet = Some(crate::investigator::types::Packet {
            work_id: "w0".into(),
            reasons: vec![Lane::Changed],
            primary_id: "s0".into(),
            sources: vec![crate::investigator::types::Source {
                id: "s0".into(),
                pin: j.leases[0].pin.clone(),
                text: "Evidence".into(),
            }],
        });
        self.store.save(&j).unwrap();
        if phase == Phase::PacketReady {
            return;
        }
        j.work[0].phase = Phase::InvocationStarted;
        j.invocations = 1;
        j.reserved_microusd = 100_000;
        j.accounting_known = false;
        self.store.save(&j).unwrap();
        if phase == Phase::ProposalReady {
            j.work[0].phase = Phase::ProposalReady;
            j.work[0].proposal = Some(Proposal {
                summary: "Catalog only".into(),
                findings: vec![],
            });
            let usage = ModelUsage {
                input: 11,
                output: 7,
                cache_creation: 3,
                cache_read: 5,
                cost_microusd: 100_000,
            };
            j.work[0].usage = Some(usage.clone());
            j.usage = usage;
            j.accounting_known = true;
            self.store.save(&j).unwrap();
        }
    }
}
fn lease(i: usize, expiry: DateTime<Utc>) -> Lease {
    Lease {
        run_id: "run".into(),
        work_id: format!("w{i}"),
        token: format!("lease-{i}"),
        pin: RevisionPin {
            path: format!("/p{i}.md"),
            revision: 1,
        },
        reasons: vec![Lane::Changed],
        expires_at: expiry.to_rfc3339(),
    }
}
struct FakeClient {
    clock: Arc<TestClock>,
    count: usize,
    calls: Vec<(Instant, ToolRequest)>,
    replay: HashMap<String, Value>,
    lose: Option<AllowedTool>,
    fail: Option<(AllowedTool, RunnerError)>,
    read_delay: u64,
    renew_latency: Duration,
    deadline: i64,
    run_state: &'static str,
    close_state: Option<&'static str>,
    response_patch: Option<(AllowedTool, Value)>,
    pause: bool,
    bad_schema: bool,
    large: bool,
    aging: bool,
    replace_lease: bool,
    submit_receipt: Option<Receipt>,
    cancel_on_get_run: Option<CancellationToken>,
    fault_after_submit: Option<(usize, super::super::journal::Fault)>,
    state_path: std::path::PathBuf,
    fault_after: Option<usize>,
    list_failures: u32,
    lists: u32,
}
impl FakeClient {
    fn report(&self, state: &str) -> Value {
        json!({"id":"run","state":state,"actor":"fixture","created_at":self.clock.utc.to_rfc3339(),
            "deadline":(self.clock.utc + chrono::Duration::seconds(self.deadline)).to_rfc3339(),
            "ended_at":(state != "running").then(|| self.clock.now().to_rfc3339()),"reason":null,"primary_count":self.count,"completed_count":0,
            "failed_count":if state == "running" { 0 } else { self.count },"inconclusive_count":0,"superseded_count":0})
    }
}
impl CatalogClient for FakeClient {
    async fn list_tools(&mut self) -> Result<Vec<Tool>, RunnerError> {
        // Real fake-CLI readiness finishes before deterministic virtual-time work.
        if self.pause {
            tokio::time::pause();
            // Only the first pause anchors virtual time; later runs re-pause after resume.
            let _ = self.clock.paused_at.set(Instant::now());
            self.pause = false;
        }
        self.lists += 1;
        if self.list_failures > 0 {
            self.list_failures -= 1;
            return Err(RunnerError::Transport);
        }
        let mut tools: Vec<Tool> =
            serde_json::from_str(include_str!("contract_tools.json")).unwrap();
        if self.bad_schema {
            tools.clear();
        }
        if let Some(n) = self.fault_after.take() {
            super::super::journal::fail_save_after(n, super::super::journal::Fault::DirectorySync);
        }
        Ok(tools)
    }
    async fn call(&mut self, request: ToolRequest) -> Result<Value, RunnerError> {
        if matches!(
            request.tool,
            AllowedTool::Claim | AllowedTool::Renew | AllowedTool::Submit | AllowedTool::Close
        ) {
            let state: Journal =
                serde_json::from_slice(&std::fs::read(&self.state_path).unwrap()).unwrap();
            let p = state
                .pending
                .as_ref()
                .expect("mutation must be durable before dispatch");
            assert_eq!(p.request.tool, request.tool);
            assert_eq!(p.request.args, request.args);
        }
        self.calls.push((Instant::now(), request.clone()));
        if let Some((tool, error)) = self.fail {
            if tool == request.tool {
                return Err(error);
            }
        }
        let key = request.args["idempotency_key"].as_str().map(str::to_owned);
        if let Some(value) = key.as_ref().and_then(|k| self.replay.get(k)) {
            return Ok(value.clone());
        }
        let mut value = match request.tool {
            AllowedTool::Claim => {
                json!({"run_id":"run", "leases":(0..self.count).map(|i| { let mut l=lease(i,self.clock.now()+chrono::Duration::seconds(900)); if self.aging { l.reasons=vec![Lane::Aging]; } l }).collect::<Vec<_>>()})
            }
            AllowedTool::Renew => {
                tokio::time::sleep(self.renew_latency).await;
                let i = request.args["work_id"].as_str().unwrap()[1..]
                    .parse()
                    .unwrap();
                let mut l = lease(i, self.clock.now() + chrono::Duration::seconds(900));
                if self.aging {
                    l.reasons = vec![Lane::Aging];
                }
                if self.replace_lease {
                    l.token = "replacement".into();
                }
                serde_json::to_value(l).unwrap()
            }
            AllowedTool::Submit => {
                if let Some((saves, fault)) = self.fault_after_submit.take() {
                    super::super::journal::fail_save_after(saves, fault);
                }
                self.submit_receipt.as_ref().map(|r|serde_json::to_value(r).unwrap()).unwrap_or_else(||
                    json!({"run_id":"run","work_id":request.args["work_id"],"outcome":request.args["submission"]["outcome"],"finding_ids":[],"outdated":false}))
            }
            AllowedTool::Close => {
                // Slice 1 auto-completes after every work item has a receipt.
                let submitted = self
                    .replay
                    .values()
                    .filter(|v| v.get("finding_ids").is_some())
                    .count();
                if (submitted == self.count && request.args["state"] != "completed")
                    || (submitted < self.count && request.args["state"] == "completed")
                {
                    return Err(RunnerError::Permission);
                }
                self.report(
                    self.close_state
                        .unwrap_or(request.args["state"].as_str().unwrap()),
                )
            }
            AllowedTool::GetRun => {
                if let Some(cancel) = self.cancel_on_get_run.take() {
                    cancel.cancel();
                }
                let terminal = self.run_state != "running";
                let work: Vec<_> = (0..self.count).map(|index| {
                    let lease = lease(index, self.clock.now());
                    json!({"id":lease.work_id,"pin":lease.pin,"reasons":lease.reasons,
                        "outcome":terminal.then_some("failed"),"assessment":terminal.then_some("not_assessed"),
                        "summary":null,"evidence":[],"finding_ids":[]})
                }).collect();
                json!({"run":self.report(self.run_state),"work":work})
            }
            AllowedTool::GetConcept => {
                tokio::time::sleep(Duration::from_secs(self.read_delay)).await;
                json!({"catalog":"synthetic","path":request.args["path"],"title":"Fixture","frontmatter":{"aliases":["one","two","three","four"]},"frontmatter_error":null,"lines":1,"chars":8,"truncated":false,"body":"Evidence","raw":if self.large { "x".repeat(65_537) } else { "Evidence".into() },"links":[],"backlinks":[]})
            }
            AllowedTool::ReadRevision => {
                tokio::time::sleep(Duration::from_secs(self.read_delay)).await;
                json!({"pin":{"path":request.args["path"],"revision":1},"content":"Evidence","outdated":false})
            }
            AllowedTool::Search => {
                tokio::time::sleep(Duration::from_secs(self.read_delay)).await;
                json!({"hits":[],"skipped":[]})
            }
            _ => panic!("unexpected tool"),
        };
        if request.tool != AllowedTool::Search {
            value["catalog"] = json!("synthetic");
        }
        if let Some((tool, patch)) = &self.response_patch {
            if *tool == request.tool {
                for (name, field) in patch.as_object().unwrap() {
                    value[name] = field.clone();
                }
            }
        }
        if let Some(key) = key {
            self.replay.insert(key, value.clone());
        }
        if self.lose == Some(request.tool) {
            self.lose = None;
            return Err(RunnerError::Transport);
        }
        Ok(value)
    }
}
struct FakeInference {
    calls: AtomicU32,
    active: AtomicU32,
    maximum: AtomicU32,
    reaped: AtomicU32,
    delay: u64,
    error: Option<RunnerError>,
    invalid: bool,
    invalid_first: bool,
    invalid_second: bool,
    cost: u64,
    started: tokio::sync::Notify,
    input: u64,
    overflow_second: bool,
    path: std::sync::Mutex<Option<std::path::PathBuf>>,
}
impl FakeInference {
    fn new(delay: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicU32::new(0),
            active: AtomicU32::new(0),
            maximum: AtomicU32::new(0),
            reaped: AtomicU32::new(0),
            delay,
            error: None,
            invalid: false,
            invalid_first: false,
            invalid_second: false,
            cost: 100_000,
            started: tokio::sync::Notify::new(),
            input: 11,
            overflow_second: false,
            path: std::sync::Mutex::new(None),
        })
    }
}
impl Inference for FakeInference {
    async fn infer(
        &self,
        _: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelResult, RunnerError> {
        if let Some(path) = self.path.lock().unwrap().as_ref() {
            let j: Journal = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert!(j.pending.is_none());
            assert!(!j.accounting_known);
            assert_eq!(
                j.work
                    .iter()
                    .filter(|w| w.phase == Phase::InvocationStarted)
                    .count(),
                1
            );
            assert!(j.invocations > self.calls.load(Ordering::SeqCst));
            assert!(j.reserved_microusd >= 100_000);
        }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_waiters();
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        let cancelled = tokio::select! { biased; _ = cancel.cancelled() => true, _ = tokio::time::sleep(Duration::from_secs(self.delay)) => false };
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.reaped.fetch_add(1, Ordering::SeqCst);
        if cancelled {
            return Err(RunnerError::Cancelled);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(ModelResult {
            proposal: Proposal {
                summary: if self.invalid
                    || (self.invalid_first && call == 0)
                    || (self.invalid_second && call > 0)
                {
                    "x".repeat(262_145)
                } else {
                    "Catalog only".into()
                },
                findings: vec![],
            },
            usage: ModelUsage {
                input: if self.overflow_second && call > 0 {
                    u64::MAX
                } else {
                    self.input
                },
                output: 7,
                cache_creation: 3,
                cache_read: 5,
                cost_microusd: self.cost,
            },
        })
    }
}
async fn run(
    f: &Fixture,
    c: &mut FakeClient,
    i: Arc<FakeInference>,
) -> Result<RunSummary, RunnerError> {
    *i.path.lock().unwrap() = Some(f.config.state_dir.join("journal.json"));
    run_once(
        &f.config,
        c,
        i,
        &f.store,
        f.clock.as_ref(),
        CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn investigator_supervisor_renews_queued_work() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let i = FakeInference::new(70);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    assert_eq!(s.reported, 2);
    assert_eq!(i.maximum.load(Ordering::SeqCst), 1);
    let first_submit = c
        .calls
        .iter()
        .position(|(_, r)| r.tool == AllowedTool::Submit)
        .unwrap();
    for id in ["w0", "w1"] {
        assert!(c.calls[..first_submit]
            .iter()
            .any(|(_, r)| r.tool == AllowedTool::Renew && r.args["work_id"] == id));
    }
    let j = f.store.load().unwrap().unwrap();
    assert!(j.terminal);
    assert_eq!(j.invocations, 2);
    assert_eq!(j.reserved_microusd, 200_000);
}
#[tokio::test]
async fn investigator_supervisor_never_repays_ambiguous_inference() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut c = f.client(1);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(s.exit_code(), 4);
    assert!(!s.accounting_known);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.invocations, 1);
    assert_eq!(j.reserved_microusd, 100_000);
    assert!(!c.calls.iter().any(|(_, r)| r.tool == AllowedTool::Claim));
}
#[tokio::test]
async fn investigator_supervisor_replays_mutations_before_new_work() {
    let f = Fixture::new();
    let mut j = Journal::new(f.config.binding("test-only-credential").unwrap());
    j.pending = Some(
        PendingMutation::new(
            ToolRequest {
                tool: AllowedTool::Claim,
                args: json!({"batch_size":1,"idempotency_key":"original"}),
            },
            "synthetic",
        )
        .unwrap(),
    );
    let original = j.pending.as_ref().unwrap().request.args.clone();
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    c.lose = Some(AllowedTool::Claim);
    let s = run(&f, &mut c, FakeInference::new(0)).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    assert_eq!(c.calls[0].1.args, original);
    assert_eq!(c.calls[1].1.args, original);
    assert_eq!(
        c.calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::Claim)
            .count(),
        2
    );
}
#[tokio::test]
async fn investigator_supervisor_stops_on_accounting_or_clock_failure() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let mut i = FakeInference::new(0);
    Arc::get_mut(&mut i).unwrap().error = Some(RunnerError::InvalidProposal);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert!(!s.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.reserved_microusd, 100_000);
    assert_eq!(j.usage.cost_microusd, 0);
}
#[tokio::test]
async fn investigator_supervisor_known_invalid_proposal_preserves_usage() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let mut i = FakeInference::new(0);
    Arc::get_mut(&mut i).unwrap().invalid = true;
    let s = run(&f, &mut c, i).await.unwrap();
    // Each rejected proposal fails only its own item; the run still investigates both.
    assert_eq!(s.exit_code(), 4);
    assert!(s.accounting_known);
    assert_eq!(s.failed, 2);
    assert_eq!(s.reason, "completed");
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.usage.cost_microusd, 200_000);
    assert_eq!(j.usage.input, 22);
    assert_eq!(j.invocations, 2);
}
#[tokio::test]
async fn investigator_supervisor_invalid_proposal_fails_only_its_item() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let mut i = FakeInference::new(0);
    Arc::get_mut(&mut i).unwrap().invalid_first = true;
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(i.calls.load(Ordering::SeqCst), 2);
    assert_eq!(s.reported, 1);
    assert_eq!(s.failed, 1);
    assert_eq!(s.skipped, 0);
    assert_eq!(s.reason, "completed");
    assert_eq!(s.exit_code(), 4);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.work[0].failure.as_deref(), Some("invalid_proposal"));
    assert_eq!(j.work[1].failure, None);
    assert!(j.terminal);
    let outcomes: Vec<_> = c
        .calls
        .iter()
        .filter(|(_, r)| r.tool == AllowedTool::Submit)
        .map(|(_, r)| r.args["submission"]["outcome"].clone())
        .collect();
    assert_eq!(outcomes, [json!("failed"), json!("completed")]);
}
#[tokio::test]
async fn investigator_supervisor_recovered_invalid_proposal_is_not_a_run_stop() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut j = f.store.load().unwrap().unwrap();
    let usage = ModelUsage {
        input: 11,
        output: 7,
        cache_creation: 3,
        cache_read: 5,
        cost_microusd: 100_000,
    };
    j.work[0].phase = Phase::ProposalReady;
    j.work[0].proposal = Some(Proposal {
        summary: "x".repeat(262_145),
        findings: vec![],
    });
    j.work[0].usage = Some(usage.clone());
    j.work[0].failure = Some("invalid_proposal".into());
    j.usage = usage;
    j.accounting_known = true;
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(s.failed, 1);
    assert_eq!(s.reason, "completed");
    let submit = c
        .calls
        .iter()
        .find(|(_, r)| r.tool == AllowedTool::Submit)
        .unwrap();
    assert_eq!(submit.1.args["submission"]["outcome"], "failed");
    assert!(c
        .calls
        .iter()
        .any(|(_, r)| r.tool == AllowedTool::Close && r.args["rationale"] == "completed"));
}
#[tokio::test]
async fn investigator_supervisor_unknown_stop_closes_completed_server_run() {
    let f = Fixture::new();
    let mut c = f.client(1);
    let mut i = FakeInference::new(0);
    Arc::get_mut(&mut i).unwrap().error = Some(RunnerError::Child);
    let s = run(&f, &mut c, i).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert!(!s.accounting_known);
    assert!(f.store.load().unwrap().unwrap().terminal);
    assert!(c
        .calls
        .iter()
        .any(|(_, r)| r.tool == AllowedTool::Close && r.args["state"] == "completed"));
}
#[tokio::test]
async fn investigator_supervisor_revocation_during_inference_does_not_cleanup_remotely() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.fail = Some((AllowedTool::Renew, RunnerError::Permission));
    let i = FakeInference::new(80);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.reason, "permission");
    assert_eq!(s.exit_code(), 4);
    assert!(!s.accounting_known);
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert_eq!(i.active.load(Ordering::SeqCst), 0);
    assert!(!c
        .calls
        .iter()
        .any(|(_, r)| matches!(r.tool, AllowedTool::Submit | AllowedTool::Close)));
    assert_eq!(
        c.calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::Renew)
            .count(),
        1
    );
}

#[tokio::test]
async fn investigator_supervisor_persistence_failure_reaps_without_another_save() {
    let f = Fixture::new();
    let mut c = f.client(1);
    c.fault_after = Some(5);
    let i = FakeInference::new(80);
    assert_eq!(
        run(&f, &mut c, i.clone()).await.err(),
        Some(RunnerError::Io)
    );
    assert_eq!(i.active.load(Ordering::SeqCst), 0);
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.pending.as_ref().unwrap().request.tool, AllowedTool::Renew);
    assert!(
        j.work[0].failure.is_none(),
        "failed save must not be followed by another checkpoint"
    );
    assert_eq!(j.invocations, 1);
    assert_eq!(j.reserved_microusd, 100_000);
}
#[tokio::test]
async fn investigator_supervisor_clock_jump_cancels_and_retains_authority() {
    for jump in [-31, 31] {
        let f = Fixture::new();
        let mut c = f.client(2);
        let i = FakeInference::new(80);
        let model = i.clone();
        let clock = f.clock.clone();
        let jump_task = tokio::spawn(async move {
            while model.calls.load(Ordering::SeqCst) == 0 {
                model.started.notified().await;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            clock.jump.store(jump, Ordering::SeqCst);
        });
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        jump_task.await.unwrap();
        assert_eq!(s.reason, "contract");
        assert_eq!(s.exit_code(), 4);
        assert!(!s.accounting_known);
        assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
        assert!(!c
            .calls
            .iter()
            .any(|(_, r)| matches!(r.tool, AllowedTool::Submit | AllowedTool::Close)));
        assert!(!f.store.load().unwrap().unwrap().terminal);
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_cancel_reaps_owned_task() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let i = FakeInference::new(80);
    let token = CancellationToken::new();
    let signal = token.clone();
    let model = i.clone();
    let task = tokio::spawn(async move {
        while model.calls.load(Ordering::SeqCst) == 0 {
            model.started.notified().await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        signal.cancel();
    });
    let s = run_once(
        &f.config,
        &mut c,
        i.clone(),
        &f.store,
        f.clock.as_ref(),
        token,
    )
    .await
    .unwrap();
    task.await.unwrap();
    assert_eq!(s.reason, "cancelled");
    assert_eq!(s.exit_code(), 4);
    assert!(!s.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert!(f.store.load().unwrap().unwrap().terminal);
}
#[tokio::test]
async fn investigator_supervisor_lower_server_deadline_stops_child() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.deadline = 50;
    let i = FakeInference::new(80);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.reason, "budget");
    assert_eq!(s.exit_code(), 4);
    assert!(f.clock.virtual_elapsed() < Duration::from_secs(22));
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert!(!c
        .calls
        .iter()
        .any(|(_, r)| matches!(r.tool, AllowedTool::Submit | AllowedTool::Close)));
}
#[tokio::test]
async fn investigator_supervisor_local_deadline_is_monotonic() {
    let mut f = Fixture::new();
    f.config.max_run_seconds = 20;
    let mut c = f.client(2);
    let i = FakeInference::new(80);
    let model = i.clone();
    let clock = f.clock.clone();
    let task = tokio::spawn(async move {
        while model.calls.load(Ordering::SeqCst) == 0 {
            model.started.notified().await;
        }
        clock.jump.store(-29, Ordering::SeqCst);
    });
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    task.await.unwrap();
    assert_eq!(s.reason, "budget");
    assert!(!c
        .calls
        .iter()
        .any(|(_, r)| matches!(r.tool, AllowedTool::Submit | AllowedTool::Close)));
    assert!(!f.store.load().unwrap().unwrap().terminal);
    assert!(f.clock.virtual_elapsed() < Duration::from_secs(22));
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert_eq!(s.exit_code(), 4);
}
#[tokio::test]
async fn investigator_supervisor_slow_collection_yields_queued_heartbeats() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.read_delay = 9;
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    let start = c.calls[0].0;
    for id in ["w0", "w1"] {
        let at = c
            .calls
            .iter()
            .find(|(_, r)| r.tool == AllowedTool::Renew && r.args["work_id"] == id)
            .unwrap()
            .0;
        assert!(at.duration_since(start) <= Duration::from_secs(61));
    }
    assert_eq!(i.maximum.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn investigator_supervisor_historical_transport_reconciles_without_inference() {
    for failure in [
        None,
        Some(RunnerError::Permission),
        Some(RunnerError::Contract),
        Some(RunnerError::Transport),
    ] {
        let f = Fixture::new();
        let mut c = f.client(1);
        c.fail = Some((AllowedTool::Submit, RunnerError::Transport));
        let model = FakeInference::new(0);
        let first = run(&f, &mut c, model.clone()).await.unwrap();
        assert_eq!(first.reason, "transport");
        let before = f.store.load().unwrap().unwrap();
        assert!(before.pending.is_some());
        c.fail = failure.map(|e| (AllowedTool::GetRun, e));
        tokio::time::resume();
        c.pause = true;
        let resumed = run(&f, &mut c, model.clone()).await.unwrap();
        let after = f.store.load().unwrap().unwrap();
        assert_eq!(resumed.reason, "transport");
        assert_eq!(resumed.exit_code(), 4);
        assert_eq!(after.terminal, failure.is_none());
        assert_eq!(after.invocations, before.invocations);
        assert_eq!(after.reserved_microusd, before.reserved_microusd);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(after.receipts.len(), 1);
        assert_eq!(
            c.calls
                .iter()
                .filter(|(_, r)| r.tool == AllowedTool::Close)
                .count(),
            usize::from(failure.is_none())
        );
        tokio::time::resume();
    }
}

#[tokio::test]
async fn investigator_supervisor_unresolved_renewal_blocks_submissions_and_models() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.fail = Some((AllowedTool::Renew, RunnerError::Transport));
    let i = FakeInference::new(80);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.reason, "transport");
    assert_eq!(s.exit_code(), 4);
    let renewals: Vec<_> = c
        .calls
        .iter()
        .filter(|(_, r)| r.tool == AllowedTool::Renew)
        .collect();
    assert_eq!(renewals.len(), 3);
    for r in &renewals {
        assert_eq!(r.1.args, renewals[0].1.args);
    }
    for (pair, seconds) in renewals.windows(2).zip([1, 2]) {
        let delay = pair[1].0.duration_since(pair[0].0);
        assert!(
            delay >= Duration::from_secs(seconds)
                && delay < Duration::from_millis(seconds * 1000 + 10)
        );
    }
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert!(!c
        .calls
        .iter()
        .any(|(_, r)| matches!(r.tool, AllowedTool::Submit | AllowedTool::Close)));
    assert_eq!(
        f.store
            .load()
            .unwrap()
            .unwrap()
            .pending
            .unwrap()
            .request
            .tool,
        AllowedTool::Renew
    );
}
#[tokio::test]
async fn investigator_supervisor_lost_responses_keep_exact_keys_and_payloads() {
    for tool in [
        AllowedTool::Claim,
        AllowedTool::Renew,
        AllowedTool::Submit,
        AllowedTool::Close,
    ] {
        let f = Fixture::new();
        let mut c = f.client(1);
        c.lose = Some(tool);
        let i = FakeInference::new(if tool == AllowedTool::Renew { 70 } else { 0 });
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        assert_eq!(s.exit_code(), 0);
        let calls: Vec<_> = c.calls.iter().filter(|(_, r)| r.tool == tool).collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1.args, calls[1].1.args);
        assert_eq!(i.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.store.load().unwrap().unwrap().receipts.len(), 1);
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_queued_completion_during_renewal_records_usage() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.renew_latency = Duration::from_secs(9);
    let i = FakeInference::new(45);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    assert_eq!(i.maximum.load(Ordering::SeqCst), 1);
    assert!(s.accounting_known);
    assert_eq!(
        f.store.load().unwrap().unwrap().usage.cost_microusd,
        200_000
    );
}
#[tokio::test]
async fn investigator_supervisor_lease_replacement_is_not_authority() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.replace_lease = true;
    let i = FakeInference::new(70);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.reason, "lease_lost");
    assert_eq!(s.exit_code(), 4);
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert!(!c.calls.iter().any(|(_, r)| r.tool == AllowedTool::Submit));
}
#[tokio::test]
async fn investigator_supervisor_default_ten_and_exact_money_boundary() {
    let f = Fixture::new();
    let mut c = f.client(10);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    assert_eq!(s.invocations, 10);
    assert_eq!(i.calls.load(Ordering::SeqCst), 10);
    assert_eq!(i.maximum.load(Ordering::SeqCst), 1);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.reserved_microusd, 1_000_000);
    assert_eq!(j.usage.cost_microusd, 1_000_000);
}
#[tokio::test]
async fn investigator_supervisor_call_token_and_money_stops_remain_partial() {
    for control in ["calls", "tokens", "money", "overshoot"] {
        let mut f = Fixture::new();
        let mut i = FakeInference::new(0);
        match control {
            "calls" => f.config.max_invocations = 1,
            "tokens" => f.config.observed_token_stop = 26,
            "money" => f.config.run_microusd = 100_000,
            _ => Arc::get_mut(&mut i).unwrap().cost = 2_000_000,
        }
        let mut c = f.client(2);
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        assert_eq!(s.reason, "budget");
        assert_eq!(s.exit_code(), 4);
        assert!(s.accounting_known);
        assert_eq!(i.calls.load(Ordering::SeqCst), 1);
        assert_eq!(s.reported, 1);
        assert_eq!(s.failed, 1);
        let j = f.store.load().unwrap().unwrap();
        assert_eq!(j.reserved_microusd, 100_000);
        assert_eq!(
            j.usage.cost_microusd,
            if control == "overshoot" {
                2_000_000
            } else {
                100_000
            }
        );
        assert!(j.terminal);
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_empty_and_aging_are_acknowledged_success() {
    for n in [0, 1] {
        let f = Fixture::new();
        let mut c = f.client(n);
        c.aging = true;
        let i = FakeInference::new(0);
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        assert_eq!(s.exit_code(), 0);
        assert_eq!(s.inconclusive, n as u32);
        assert_eq!(s.reported, 0);
        assert_eq!(s.failed, 0);
        assert_eq!(i.calls.load(Ordering::SeqCst), n as u32);
        assert!(f.store.load().unwrap().unwrap().terminal);
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_large_primary_skips_without_inference() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.large = true;
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(s.failed, 2);
    assert_eq!(s.reason, "invalid_evidence");
    assert!(s.accounting_known);
}
#[tokio::test]
async fn investigator_supervisor_schemas_and_executable_preflight_fail_before_claim() {
    for bad in ["schema", "version", "help"] {
        let mut f = Fixture::new();
        let mut c = f.client(1);
        match bad {
            "schema" => c.bad_schema = true,
            "version" => f.config.expected_claude_version = "wrong".into(),
            _ => {
                std::fs::write(&f.config.claude_binary, "#!/bin/sh\nprintf 'fixture\\n'\n").unwrap()
            }
        }
        assert_eq!(
            run(&f, &mut c, FakeInference::new(0)).await.err(),
            Some(RunnerError::Contract)
        );
        assert!(c.calls.is_empty());
        assert!(f.store.load().unwrap().is_none());
        if bad == "schema" {
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_all_durable_boundaries_recover_without_repayment() {
    use super::super::journal::{fail_save_after, Fault};
    for fault in [Fault::FileSync, Fault::Rename, Fault::DirectorySync] {
        for boundary in 0..10 {
            let f = Fixture::new();
            let mut c = f.client(1);
            let i = FakeInference::new(0);
            fail_save_after(boundary, fault);
            assert_eq!(
                run(&f, &mut c, i.clone()).await.err(),
                Some(RunnerError::Io),
                "boundary {boundary}"
            );
            let saved = f.store.load().unwrap();
            let pending = saved.as_ref().and_then(|j| j.pending.clone());
            let reserved = saved.as_ref().map_or(0, |j| j.reserved_microusd);
            let invoked = saved.as_ref().is_some_and(|j| j.invocations > 0);
            let calls = i.calls.load(Ordering::SeqCst);
            assert_eq!(i.active.load(Ordering::SeqCst), 0);
            if !saved.as_ref().is_some_and(|j| j.terminal) {
                let before = c.calls.len();
                tokio::time::resume();
                c.pause = true;
                let s = run(&f, &mut c, i.clone()).await.unwrap();
                assert!(matches!(s.exit_code(), 0 | 4));
                if let Some(p) = pending {
                    assert_eq!(c.calls[before].1.tool, p.request.tool);
                    assert_eq!(c.calls[before].1.args, p.request.args);
                }
                if invoked {
                    assert_eq!(
                        i.calls.load(Ordering::SeqCst),
                        calls,
                        "boundary {boundary} repaid invocation"
                    );
                    assert_eq!(f.store.load().unwrap().unwrap().reserved_microusd, reserved);
                }
                assert!(
                    f.store.load().unwrap().unwrap().terminal,
                    "boundary {boundary}"
                );
            }
            assert!(i.calls.load(Ordering::SeqCst) <= 1);
            tokio::time::resume();
        }
    }
}
#[tokio::test]
async fn investigator_supervisor_reconciled_interruption_never_starts_queued_model() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut j = f.store.load().unwrap().unwrap();
    j.work[0].failure = Some("unknown_accounting".into());
    j.work[0].phase = Phase::Abandoned;
    f.store.save(&j).unwrap();
    let usage = ModelUsage {
        input: 11,
        output: 7,
        cache_creation: 3,
        cache_read: 5,
        cost_microusd: 100_000,
    };
    j.work[0].usage = Some(usage.clone());
    j.usage = usage;
    j.accounting_known = true;
    j.leases
        .push(lease(1, f.clock.now() + chrono::Duration::seconds(900)));
    j.work.push(WorkState {
        work_id: "w1".into(),
        phase: Phase::Claimed,
        packet: None,
        proposal: None,
        usage: None,
        failure: None,
    });
    f.store.save(&j).unwrap();
    let mut c = f.client(2);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert!(s.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.invocations, 1);
    assert_eq!(j.reserved_microusd, 100_000);
    assert_eq!(j.usage.input, 11);
    assert!(j.terminal);
}
#[tokio::test]
async fn investigator_supervisor_authoritative_terminal_reconciles_without_receipts() {
    for state in ["failed", "cancelled", "completed"] {
        for phase in [
            Phase::Claimed,
            Phase::InvocationStarted,
            Phase::ProposalReady,
        ] {
            let f = Fixture::new();
            f.seed(phase);
            let before = f.store.load().unwrap().unwrap();
            let mut c = f.client(1);
            c.run_state = state;
            c.deadline = -1;
            let i = FakeInference::new(0);
            let s = run(&f, &mut c, i.clone()).await.unwrap();
            assert_eq!(s.exit_code(), 4);
            let j = f.store.load().unwrap().unwrap();
            assert!(j.terminal, "authoritative {state} must release the journal");
            assert!(j.work[0].phase == Phase::Abandoned);
            assert!(j.receipts.is_empty());
            assert_eq!(j.invocations, before.invocations);
            assert_eq!(j.reserved_microusd, before.reserved_microusd);
            assert_eq!(j.accounting_known, before.accounting_known);
            assert_eq!(
                serde_json::to_value(&j.usage).unwrap(),
                serde_json::to_value(&before.usage).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&j.work[0].usage).unwrap(),
                serde_json::to_value(&before.work[0].usage).unwrap()
            );
            assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            assert!(c.calls.iter().all(|(_, r)| r.tool == AllowedTool::GetRun));
            // Reopen the lock/state as a restarted scheduler would. Only the next
            // authorized invocation may replace the durably reconciled journal.
            let config = f.config.clone();
            let mut next = f.client(0);
            drop(f.store);
            let store = JournalStore::open(
                &config.state_dir,
                &config.binding("test-only-credential").unwrap(),
            )
            .unwrap();
            let j = store.load().unwrap().unwrap();
            assert!(j.terminal);
            tokio::time::resume();
            next.run_state = "completed";
            let s = run_once(
                &config,
                &mut next,
                i.clone(),
                &store,
                f.clock.as_ref(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(s.exit_code(), 0);
            assert!(next.calls.iter().any(|(_, r)| r.tool == AllowedTool::Claim));
            assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_expired_running_recovery_closes_without_renewal() {
    for deadline in [false, true] {
        for close_state in ["failed", "completed"] {
            let f = Fixture::new();
            f.seed(Phase::InvocationStarted);
            let mut before = f.store.load().unwrap().unwrap();
            if !deadline {
                before.leases[0].expires_at =
                    (f.clock.now() - chrono::Duration::seconds(1)).to_rfc3339();
                f.store.save(&before).unwrap();
            }
            let mut c = f.client(1);
            c.close_state = Some(close_state);
            if deadline {
                c.deadline = -1;
            }
            let i = FakeInference::new(0);
            let s = run(&f, &mut c, i.clone()).await.unwrap();
            assert_eq!(s.exit_code(), 4);
            let j = f.store.load().unwrap().unwrap();
            assert!(j.terminal, "expired running run needs acknowledged close");
            assert!(j.pending.is_none());
            assert!(j.receipts.is_empty());
            assert!(j.work[0].phase == Phase::Abandoned);
            assert_eq!(j.work[0].failure.as_deref(), Some("unknown_accounting"));
            assert!(!j.accounting_known);
            assert_eq!(j.invocations, before.invocations);
            assert_eq!(j.reserved_microusd, before.reserved_microusd);
            assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            assert_eq!(c.calls.len(), 2);
            assert!(c.calls[0].1.tool == AllowedTool::GetRun);
            assert!(c.calls[1].1.tool == AllowedTool::Close);
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_terminal_read_follows_exact_submit_and_preserves_receipt() {
    let f = Fixture::new();
    f.seed(Phase::ProposalReady);
    let mut j = f.store.load().unwrap().unwrap();
    j.leases
        .push(lease(1, f.clock.now() + chrono::Duration::seconds(900)));
    j.work.push(WorkState {
        work_id: "w1".into(),
        phase: Phase::Claimed,
        packet: None,
        proposal: None,
        usage: None,
        failure: None,
    });
    let submission = to_submission(
        j.work[0].packet.as_ref().unwrap(),
        j.work[0].proposal.as_ref().unwrap(),
    )
    .unwrap();
    j.work[0].phase = Phase::SubmissionPending;
    j.pending = Some(PendingMutation::new(ToolRequest { tool: AllowedTool::Submit, args: json!({"work_id":"w0","token":"lease-0","submission":submission,"idempotency_key":"original-submit"}) }, "synthetic").unwrap());
    f.store.save(&j).unwrap();
    let mut c = f.client(2);
    c.run_state = "cancelled";
    let i = FakeInference::new(0);
    let result = run(&f, &mut c, i.clone()).await.unwrap();
    let after = f.store.load().unwrap().unwrap();
    assert_eq!(result.exit_code(), 4);
    assert_eq!(result.reported, 1);
    assert_eq!(result.skipped, 1);
    assert_eq!(c.calls.len(), 2);
    assert!(c.calls[0].1.tool == AllowedTool::Submit);
    assert_eq!(c.calls[0].1.args, j.pending.unwrap().request.args);
    assert!(c.calls[1].1.tool == AllowedTool::GetRun);
    assert!(after.work[0].phase == Phase::Reported);
    assert!(after.work[1].phase == Phase::Abandoned);
    assert!(after.terminal);
    assert_eq!(after.receipts.len(), 1);
    let mut acknowledged = c.replay["original-submit"].clone();
    acknowledged.as_object_mut().unwrap().remove("catalog");
    assert_eq!(
        serde_json::to_value(&after.receipts[0]).unwrap(),
        acknowledged
    );
    assert_eq!(after.invocations, j.invocations);
    assert_eq!(after.reserved_microusd, j.reserved_microusd);
    assert_eq!(
        serde_json::to_value(&after.usage).unwrap(),
        serde_json::to_value(&j.usage).unwrap()
    );
    assert!(after.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn investigator_supervisor_reconciliation_faults_reopen_without_repayment() {
    use super::super::journal::{fail_save_after, Fault};
    for state in ["failed", "cancelled", "completed", "running", "deadline"] {
        for fault in [Fault::FileSync, Fault::Rename, Fault::DirectorySync] {
            let boundaries = if matches!(state, "running" | "deadline") {
                4
            } else {
                3
            };
            for boundary in 0..boundaries {
                let f = Fixture::new();
                f.seed(Phase::InvocationStarted);
                let mut j = f.store.load().unwrap().unwrap();
                j.leases[0].expires_at =
                    (f.clock.now() - chrono::Duration::seconds(1)).to_rfc3339();
                f.store.save(&j).unwrap();
                let mut c = f.client(1);
                c.run_state = if state == "deadline" {
                    "running"
                } else {
                    state
                };
                if state == "deadline" {
                    c.deadline = -1;
                }
                c.close_state = Some("failed");
                fail_save_after(boundary, fault);
                let i = FakeInference::new(0);
                assert_eq!(
                    run(&f, &mut c, i.clone()).await.err(),
                    Some(RunnerError::Io)
                );
                let snapshot = f.store.load().unwrap().unwrap();
                let pending = snapshot.pending.clone();
                let calls = c.calls.len();
                let config = f.config.clone();
                drop(f.store);
                let store = JournalStore::open(
                    &config.state_dir,
                    &config.binding("test-only-credential").unwrap(),
                )
                .unwrap();
                tokio::time::resume();
                c.pause = true;
                if snapshot.terminal {
                    // Visible directory-sync failures must be confirmed durable,
                    // not turned into another recovery invocation in this test.
                    store.save(&snapshot).unwrap();
                } else {
                    let result = run_once(
                        &config,
                        &mut c,
                        i.clone(),
                        &store,
                        f.clock.as_ref(),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                    assert_eq!(result.exit_code(), 4);
                    if let Some(pending) = pending {
                        assert_eq!(c.calls[calls].1.args, pending.request.args);
                        assert!(c.calls[calls].1.tool == pending.request.tool);
                    }
                    tokio::time::resume();
                }
                let after = store.load().unwrap().unwrap();
                assert!(after.terminal, "{state} boundary {boundary}");
                assert!(after.receipts.is_empty());
                assert!(!after.accounting_known);
                assert_eq!(after.invocations, 1);
                assert_eq!(after.reserved_microusd, 100_000);
                assert_eq!(after.work[0].failure.as_deref(), Some("unknown_accounting"));
                assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_non_failure_close_still_rejects_mismatched_terminal_state() {
    for (requested, actual) in [("completed", "cancelled"), ("cancelled", "completed")] {
        let f = Fixture::new();
        f.seed(Phase::InvocationStarted);
        let mut j = f.store.load().unwrap().unwrap();
        j.pending = Some(PendingMutation::new(ToolRequest { tool: AllowedTool::Close, args: json!({"run_id":"run","state":requested,"rationale":"synthetic close","idempotency_key":"close-check"}) }, "synthetic").unwrap());
        f.store.save(&j).unwrap();
        let mut c = f.client(1);
        let mut response = c.report(actual);
        response["catalog"] = json!("synthetic");
        c.replay.insert("close-check".into(), response);
        let i = FakeInference::new(0);
        assert_eq!(run(&f, &mut c, i.clone()).await.unwrap().exit_code(), 4);
        let after = f.store.load().unwrap().unwrap();
        assert!(!after.terminal);
        assert_eq!(
            after.pending.unwrap().request.args,
            j.pending.unwrap().request.args
        );
        assert!(after.receipts.is_empty());
        assert_eq!(i.calls.load(Ordering::SeqCst), 0);
        assert_eq!(c.calls.len(), 1);
        tokio::time::resume();
    }
}

#[tokio::test]
async fn investigator_supervisor_terminal_responses_validate_id_catalog_state_and_deadline() {
    for tool in [AllowedTool::GetRun, AllowedTool::Close] {
        for field in ["id", "catalog", "state", "deadline"] {
            let f = Fixture::new();
            f.seed(Phase::InvocationStarted);
            let mut before = f.store.load().unwrap().unwrap();
            before.leases[0].expires_at =
                (f.clock.now() - chrono::Duration::seconds(1)).to_rfc3339();
            f.store.save(&before).unwrap();
            let mut c = f.client(1);
            c.run_state = if tool == AllowedTool::GetRun {
                "completed"
            } else {
                "running"
            };
            c.close_state = Some("cancelled");
            let invalid = match field {
                "id" => json!("wrong-run"),
                "catalog" => json!("foreign"),
                "state" => json!("unknown"),
                _ => json!("not-a-timestamp"),
            };
            let mut patch = json!({});
            if tool == AllowedTool::GetRun && field != "catalog" {
                let mut report = c.report("completed");
                report[field] = invalid;
                patch["run"] = report;
            } else {
                patch[field] = invalid;
            }
            c.response_patch = Some((tool, patch));
            let i = FakeInference::new(0);
            let result = run(&f, &mut c, i.clone()).await.unwrap();
            assert_eq!(result.exit_code(), 4);
            let after = f.store.load().unwrap().unwrap();
            assert!(!after.terminal, "{field}");
            assert!(after.work[0].phase == Phase::InvocationStarted);
            assert_eq!(after.pending.is_some(), tool == AllowedTool::Close);
            assert!(after.receipts.is_empty());
            assert_eq!(after.invocations, before.invocations);
            assert_eq!(after.reserved_microusd, before.reserved_microusd);
            assert!(!after.accounting_known);
            assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                c.calls.len(),
                if tool == AllowedTool::Close { 2 } else { 1 }
            );
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_reconciliation_errors_keep_pending_and_fail_closed() {
    for tool in [AllowedTool::GetRun, AllowedTool::Close] {
        for error in [
            RunnerError::Permission,
            RunnerError::Contract,
            RunnerError::Transport,
        ] {
            let f = Fixture::new();
            f.seed(Phase::InvocationStarted);
            let mut j = f.store.load().unwrap().unwrap();
            j.leases[0].expires_at = (f.clock.now() - chrono::Duration::seconds(1)).to_rfc3339();
            f.store.save(&j).unwrap();
            let mut c = f.client(1);
            c.fail = Some((tool, error));
            let i = FakeInference::new(0);
            let s = run(&f, &mut c, i.clone()).await.unwrap();
            let after = f.store.load().unwrap().unwrap();
            assert_eq!(s.exit_code(), 4);
            assert!(!after.terminal);
            assert!(after.receipts.is_empty());
            assert_eq!(after.pending.is_some(), tool == AllowedTool::Close);
            assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            assert!(c
                .calls
                .iter()
                .all(|(_, r)| matches!(r.tool, AllowedTool::GetRun | AllowedTool::Close)));
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_expired_renewal_replay_never_authorizes_work() {
    let f = Fixture::new();
    f.seed(Phase::Claimed);
    let mut j = f.store.load().unwrap().unwrap();
    j.leases[0].expires_at = (f.clock.now() + chrono::Duration::seconds(20)).to_rfc3339();
    j.pending = Some(
        PendingMutation::new(
            ToolRequest {
                tool: AllowedTool::Renew,
                args: json!({"work_id":"w0","token":"lease-0","idempotency_key":"old-renewal"}),
            },
            "synthetic",
        )
        .unwrap(),
    );
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    let mut old = serde_json::to_value(&j.leases[0]).unwrap();
    old["catalog"] = json!("synthetic");
    c.replay.insert("old-renewal".into(), old);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.reason, "lease_lost");
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(c.calls[0].1.args, j.pending.unwrap().request.args);
    assert!(!c
        .calls
        .iter()
        .any(|(_, r)| matches!(r.tool, AllowedTool::Claim | AllowedTool::Submit)));
}
#[tokio::test]
async fn investigator_supervisor_reissued_credential_cannot_open_old_journal() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let path = f.config.state_dir.clone();
    let binding = f
        .config
        .binding("replacement-credential-same-actor")
        .unwrap();
    drop(f.store);
    assert_eq!(
        JournalStore::open(&path, &binding).err(),
        Some(RunnerError::Journal)
    );
}
#[tokio::test]
async fn investigator_supervisor_child_timeout_is_not_a_model_retry() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let i = FakeInference::new(200);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.reason, "child");
    assert_eq!(s.exit_code(), 4);
    assert!(!s.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
    assert!(f.clock.virtual_elapsed() < Duration::from_secs(92));
    assert_eq!(f.store.load().unwrap().unwrap().reserved_microusd, 100_000);
}

#[tokio::test]
async fn investigator_supervisor_observed_invocation_overshoot_stops_further_inference() {
    let f = Fixture::new();
    let mut c = f.client(2);
    let mut i = FakeInference::new(0);
    Arc::get_mut(&mut i).unwrap().cost = 100_001;
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert_eq!(s.reason, "budget");
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.usage.cost_microusd, 100_001);
    assert_eq!(j.reserved_microusd, 100_000);
    assert!(s.accounting_known);
}
#[tokio::test]
async fn investigator_supervisor_overflow_retains_previous_totals_and_reservations() {
    for aggregate in [false, true] {
        let mut f = Fixture::new();
        f.config.observed_token_stop = u64::MAX;
        let mut c = f.client(3);
        let mut i = FakeInference::new(0);
        if aggregate {
            Arc::get_mut(&mut i).unwrap().overflow_second = true;
        } else {
            Arc::get_mut(&mut i).unwrap().input = u64::MAX;
        }
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        assert_eq!(s.exit_code(), 4);
        let j = f.store.load().unwrap().unwrap();
        assert_eq!(j.invocations, if aggregate { 2 } else { 1 });
        assert_eq!(
            j.reserved_microusd,
            if aggregate { 200_000 } else { 100_000 }
        );
        assert_eq!(j.usage.input, if aggregate { 11 } else { u64::MAX });
        assert_eq!(j.usage.cost_microusd, 100_000);
        assert_eq!(s.accounting_known, !aggregate);
        assert_eq!(i.calls.load(Ordering::SeqCst), j.invocations);
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_readiness_transports_have_bounded_retries() {
    for failures in [2, 3] {
        let f = Fixture::new();
        let mut c = f.client(0);
        c.list_failures = failures;
        let result = run(&f, &mut c, FakeInference::new(0)).await;
        assert_eq!(c.lists, 3);
        if failures == 2 {
            assert_eq!(result.unwrap().exit_code(), 0);
        } else {
            assert_eq!(result.err(), Some(RunnerError::Transport));
            assert!(c.calls.is_empty());
        }
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_recovered_budget_close_is_not_success() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut j = f.store.load().unwrap().unwrap();
    let usage = ModelUsage {
        input: 11,
        output: 7,
        cache_creation: 3,
        cache_read: 5,
        cost_microusd: 100_000,
    };
    j.work[0].phase = Phase::ProposalReady;
    j.work[0].proposal = Some(Proposal {
        summary: "Catalog only".into(),
        findings: vec![],
    });
    j.work[0].usage = Some(usage.clone());
    j.usage = usage;
    j.accounting_known = true;
    f.store.save(&j).unwrap();
    let submission = to_submission(
        j.work[0].packet.as_ref().unwrap(),
        j.work[0].proposal.as_ref().unwrap(),
    )
    .unwrap();
    j.work[0].phase = Phase::SubmissionPending;
    j.pending=Some(PendingMutation::new(ToolRequest {tool:AllowedTool::Submit,args:json!({"work_id":"w0","token":"lease-0","idempotency_key":"submitted","submission":submission})},"synthetic").unwrap());
    f.store.save(&j).unwrap();
    let receipt = Receipt {
        run_id: "run".into(),
        work_id: "w0".into(),
        outcome: WorkOutcome::Completed,
        finding_ids: vec![],
        outdated: false,
    };
    j.work[0].phase = Phase::Reported;
    j.receipts.push(receipt.clone());
    j.pending = None;
    f.store.save(&j).unwrap();
    j.pending=Some(PendingMutation::new(ToolRequest {tool:AllowedTool::Close,args:json!({"run_id":"run","state":"completed","rationale":"budget","idempotency_key":"original-close"})},"synthetic").unwrap());
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    c.replay
        .insert("submitted".into(), serde_json::to_value(receipt).unwrap());
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert_eq!(s.reason, "budget");
    assert!(s.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(c.calls.len(), 1);
    assert_eq!(c.calls[0].1.args, j.pending.unwrap().request.args);
}

#[tokio::test]
async fn investigator_supervisor_invalid_second_result_never_erases_prior_usage() {
    let f = Fixture::new();
    let mut c = f.client(3);
    let mut i = FakeInference::new(0);
    Arc::get_mut(&mut i).unwrap().invalid_second = true;
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert!(s.accounting_known);
    // Rejected proposals no longer stop the run, so all three items are invoked.
    assert_eq!(i.calls.load(Ordering::SeqCst), 3);
    assert_eq!(s.reported, 1);
    assert_eq!(s.failed, 2);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.invocations, 3);
    assert_eq!(j.reserved_microusd, 300_000);
    assert_eq!(j.usage.cost_microusd, 300_000);
    assert_eq!(j.usage.input, 33);
    assert_eq!(j.usage.output, 21);
    assert_eq!(j.work[1].failure.as_deref(), Some("invalid_proposal"));
    assert_eq!(j.work[2].failure.as_deref(), Some("invalid_proposal"));
}
#[tokio::test]
async fn investigator_supervisor_renewal_durable_faults_preserve_exact_replay() {
    use super::super::journal::{fail_save_after, Fault};
    for fault in [Fault::FileSync, Fault::Rename, Fault::DirectorySync] {
        for boundary in [5, 6] {
            let f = Fixture::new();
            let mut c = f.client(1);
            let i = FakeInference::new(80);
            fail_save_after(boundary, fault);
            assert_eq!(
                run(&f, &mut c, i.clone()).await.err(),
                Some(RunnerError::Io)
            );
            assert_eq!(i.calls.load(Ordering::SeqCst), 1);
            assert_eq!(i.reaped.load(Ordering::SeqCst), 1);
            let j = f.store.load().unwrap().unwrap();
            let pending = j.pending.clone();
            let before = c.calls.len();
            tokio::time::resume();
            c.pause = true;
            let s = run(&f, &mut c, i.clone()).await.unwrap();
            assert_eq!(s.exit_code(), 4);
            assert!(!s.accounting_known);
            assert_eq!(i.calls.load(Ordering::SeqCst), 1);
            if let Some(p) = pending {
                assert_eq!(
                    serde_json::to_vec(&c.calls[before].1.args).unwrap(),
                    serde_json::to_vec(&p.request.args).unwrap()
                );
            }
            let j = f.store.load().unwrap().unwrap();
            assert_eq!(j.reserved_microusd, 100_000);
            assert!(j.terminal);
            tokio::time::resume();
        }
    }
}
#[tokio::test]
async fn investigator_supervisor_runtime_protocol_and_permission_are_fatal_four() {
    for error in [RunnerError::Contract, RunnerError::Permission] {
        for tool in [
            AllowedTool::Claim,
            AllowedTool::GetRun,
            AllowedTool::GetConcept,
            AllowedTool::Submit,
        ] {
            let f = Fixture::new();
            let mut c = f.client(1);
            c.fail = Some((tool, error));
            let i = FakeInference::new(0);
            let s = run(&f, &mut c, i.clone()).await.unwrap();
            assert_eq!(s.exit_code(), 4);
            assert_eq!(
                s.reason,
                if error == RunnerError::Contract {
                    "contract"
                } else {
                    "permission"
                }
            );
            assert_eq!(c.calls.iter().filter(|(_, r)| r.tool == tool).count(), 1);
            assert!(!c.calls.iter().any(|(_, r)| r.tool == AllowedTool::Close));
            if tool != AllowedTool::Submit {
                assert_eq!(i.calls.load(Ordering::SeqCst), 0);
            }
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn investigator_supervisor_starts_slow_renewal_queue_before_sixty_seconds() {
    let f = Fixture::new();
    let mut c = f.client(2);
    c.renew_latency = Duration::from_secs(9);
    let s = run(&f, &mut c, FakeInference::new(65)).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    let start = c.calls[0].0;
    for id in ["w0", "w1"] {
        let at = c
            .calls
            .iter()
            .find(|(_, r)| r.tool == AllowedTool::Renew && r.args["work_id"] == id)
            .unwrap()
            .0;
        assert!(at.duration_since(start) < Duration::from_secs(51));
    }
}

#[tokio::test]
async fn investigator_supervisor_marks_interruption_before_recovery_rpc() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut c = f.client(1);
    c.fail = Some((AllowedTool::GetRun, RunnerError::Permission));
    let s = run(&f, &mut c, FakeInference::new(0)).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.work[0].failure.as_deref(), Some("unknown_accounting"));
    assert!(!j.accounting_known);
    assert_eq!(j.reserved_microusd, 100_000);
}
#[tokio::test]
async fn investigator_supervisor_confirms_terminal_durability_before_replacement() {
    let f = Fixture::new();
    let mut c = f.client(1);
    assert_eq!(
        run(&f, &mut c, FakeInference::new(0))
            .await
            .unwrap()
            .exit_code(),
        0
    );
    tokio::time::resume();
    c.pause = true;
    c.fault_after = Some(0);
    let before = c.calls.len();
    assert_eq!(
        run(&f, &mut c, FakeInference::new(0)).await.err(),
        Some(RunnerError::Io)
    );
    assert_eq!(c.calls.len(), before);
    assert!(f.store.load().unwrap().unwrap().terminal);
}

#[tokio::test]
async fn investigator_supervisor_reconciled_valid_proposal_is_reported_without_inference() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut j = f.store.load().unwrap().unwrap();
    let usage = ModelUsage {
        input: 11,
        output: 7,
        cache_creation: 3,
        cache_read: 5,
        cost_microusd: 100_000,
    };
    j.work[0].phase = Phase::ProposalReady;
    j.work[0].failure = Some("unknown_accounting".into());
    j.work[0].proposal = Some(Proposal {
        summary: "Catalog only".into(),
        findings: vec![],
    });
    j.work[0].usage = Some(usage.clone());
    j.usage = usage;
    j.accounting_known = true;
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert!(s.accounting_known);
    assert_eq!(s.reported, 1);
    assert_eq!(s.failed, 0);
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert!(f.store.load().unwrap().unwrap().terminal);
}

#[tokio::test]
async fn investigator_supervisor_readiness_honors_local_stop_without_claim() {
    for cancelled in [false, true] {
        let mut f = Fixture::new();
        f.config.max_run_seconds = 1;
        std::fs::write(&f.config.claude_binary, "#!/bin/sh\nexec /bin/sleep 2\n").unwrap();
        let mut c = f.client(1);
        let i = FakeInference::new(0);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let signal = tokio::spawn(async move {
            if cancelled {
                tokio::time::sleep(Duration::from_millis(20)).await;
                token.cancel();
            }
        });
        let error = run_once(
            &f.config,
            &mut c,
            i.clone(),
            &f.store,
            f.clock.as_ref(),
            cancel,
        )
        .await
        .err();
        signal.await.unwrap();
        assert_eq!(
            error,
            Some(if cancelled {
                RunnerError::Cancelled
            } else {
                RunnerError::Budget
            })
        );
        assert_eq!(i.calls.load(Ordering::SeqCst), 0);
        assert_eq!(c.lists, 0);
        assert!(c.calls.is_empty());
        assert!(f.store.load().unwrap().is_none());
    }
}

#[tokio::test]
async fn investigator_supervisor_upper_batch_renewal_sweeps_allow_work_progress() {
    let mut f = Fixture::new();
    f.config.batch_size = 50;
    f.config.max_invocations = 50;
    f.config.run_microusd = 5_000_000;
    f.config.max_run_seconds = 1000;
    let mut c = f.client(50);
    c.read_delay = 1;
    c.renew_latency = Duration::from_millis(250);
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    assert_eq!(s.reported, 50);
    assert_eq!(s.skipped, 0);
    assert_eq!(s.failed, 0);
    assert_eq!(i.calls.load(Ordering::SeqCst), 50);
    assert_eq!(i.maximum.load(Ordering::SeqCst), 1);
    assert!(
        c.calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::GetConcept)
            .count()
            >= 50
    );
    assert_eq!(
        c.calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::Submit)
            .count(),
        50
    );
    assert!(c.calls.iter().any(|(_, r)| r.tool == AllowedTool::Renew));
    let j = f.store.load().unwrap().unwrap();
    assert!(j.terminal);
    assert_eq!(j.receipts.len(), 50);
    assert!(s.accounting_known);
}

fn historical_receipt() -> Receipt {
    Receipt {
        run_id: "run".into(),
        work_id: "w0".into(),
        outcome: WorkOutcome::Superseded,
        finding_ids: vec![],
        outdated: true,
    }
}
#[tokio::test]
async fn investigator_supervisor_historical_receipt_and_lost_response_acknowledge_once() {
    for lost in [false, true] {
        let f = Fixture::new();
        let mut c = f.client(1);
        c.submit_receipt = Some(historical_receipt());
        if lost {
            c.lose = Some(AllowedTool::Submit);
        }
        let i = FakeInference::new(0);
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        assert_eq!(s.exit_code(), 0);
        assert_eq!(s.reported, 1);
        assert_eq!(s.inconclusive, 0);
        assert_eq!(s.failed, 0);
        assert_eq!(s.skipped, 0);
        assert!(s.accounting_known);
        assert_eq!(i.calls.load(Ordering::SeqCst), 1);
        let submits: Vec<_> = c
            .calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::Submit)
            .collect();
        assert_eq!(submits.len(), if lost { 2 } else { 1 });
        for (_, request) in &submits {
            assert_eq!(request.args["submission"]["outcome"], "completed");
            assert_eq!(
                serde_json::to_vec(&request.args).unwrap(),
                serde_json::to_vec(&submits[0].1.args).unwrap()
            );
        }
        let j = f.store.load().unwrap().unwrap();
        assert!(j.terminal);
        assert!(j.pending.is_none());
        assert_eq!(j.receipts, vec![historical_receipt()]);
        assert!(j.work[0].phase == Phase::Reported);
        tokio::time::resume();
    }
}
#[tokio::test]
async fn investigator_supervisor_rejects_malformed_historical_receipt() {
    for bad in ["outdated", "run", "work"] {
        let f = Fixture::new();
        let mut c = f.client(1);
        let mut receipt = historical_receipt();
        match bad {
            "outdated" => receipt.outdated = false,
            "run" => receipt.run_id = "foreign".into(),
            _ => receipt.work_id = "foreign".into(),
        }
        c.submit_receipt = Some(receipt);
        let i = FakeInference::new(0);
        let s = run(&f, &mut c, i.clone()).await.unwrap();
        assert_eq!(s.exit_code(), 4);
        assert_eq!(s.reason, "contract");
        assert_eq!(s.reported, 0);
        assert_eq!(s.skipped, 1);
        assert_eq!(i.calls.load(Ordering::SeqCst), 1);
        let j = f.store.load().unwrap().unwrap();
        assert!(j.receipts.is_empty());
        assert!(!j.terminal);
        assert_eq!(j.pending.unwrap().request.tool, AllowedTool::Submit);
        assert!(!c.calls.iter().any(|(_, r)| r.tool == AllowedTool::Close));
        tokio::time::resume();
    }
}

#[tokio::test]
async fn investigator_supervisor_cancelled_cleanup_receipt_crash_preserves_run_stop() {
    let f = Fixture::new();
    f.seed(Phase::InvocationStarted);
    let mut j = f.store.load().unwrap().unwrap();
    let usage = ModelUsage {
        input: 11,
        output: 7,
        cache_creation: 3,
        cache_read: 5,
        cost_microusd: 100_000,
    };
    j.work[0].phase = Phase::ProposalReady;
    j.work[0].proposal = Some(Proposal {
        summary: "Catalog only".into(),
        findings: vec![],
    });
    j.work[0].usage = Some(usage.clone());
    j.usage = usage.clone();
    j.accounting_known = true;
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    let cancel = CancellationToken::new();
    c.cancel_on_get_run = Some(cancel.clone());
    c.fault_after_submit = Some((1, super::super::journal::Fault::FileSync));
    let i = FakeInference::new(0);
    assert_eq!(
        run_once(
            &f.config,
            &mut c,
            i.clone(),
            &f.store,
            f.clock.as_ref(),
            cancel
        )
        .await
        .err(),
        Some(RunnerError::Io)
    );
    let saved = f.store.load().unwrap().unwrap();
    assert!(saved.work[0].phase == Phase::Reported);
    assert!(saved.pending.is_none());
    assert!(!saved.terminal);
    assert_eq!(saved.receipts.len(), 1);
    assert!(saved.work[0].failure.is_none());
    assert_eq!(saved.stop_reason.as_deref(), Some("cancelled"));
    assert_eq!(
        saved.work[0].proposal.as_ref().unwrap().summary,
        "Catalog only"
    );
    assert_eq!(saved.usage, usage);
    assert!(saved.accounting_known);
    tokio::time::resume();
    c.pause = true;
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert_eq!(s.reason, "cancelled");
    assert_eq!(s.reported, 1);
    assert!(s.accounting_known);
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(s.invocations, 1);
    assert_eq!(
        c.calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::Submit)
            .count(),
        1
    );
    let j = f.store.load().unwrap().unwrap();
    assert!(j.terminal);
    assert_eq!(j.reserved_microusd, 100_000);
}

#[test]
fn investigator_journal_stop_reason_is_validated_and_first_cause_is_immutable() {
    for replacement in [None, Some("budget"), Some("cancelled source text")] {
        let f = Fixture::new();
        let mut j = f.store.fresh_journal().unwrap();
        f.store.save(&j).unwrap();
        j.stop_reason = Some("cancelled".into());
        f.store.save(&j).unwrap();
        let mut changed = j.clone();
        changed.stop_reason = replacement.map(str::to_owned);
        assert_eq!(f.store.save(&changed), Err(RunnerError::Journal));
        assert_eq!(
            f.store.load().unwrap().unwrap().stop_reason.as_deref(),
            Some("cancelled")
        );
        f.store.save(&j).unwrap();
    }
    for invalid in ["", "completed", "empty", "arbitrary remote text"] {
        let f = Fixture::new();
        let mut j = f.store.fresh_journal().unwrap();
        j.stop_reason = Some(invalid.into());
        assert_eq!(f.store.save(&j), Err(RunnerError::Journal));
        assert!(f.store.load().unwrap().is_none());
    }
}
#[test]
fn investigator_journal_stop_reason_resets_only_with_terminal_fresh_state() {
    let f = Fixture::new();
    let mut j = f.store.fresh_journal().unwrap();
    j.stop_reason = Some("cancelled".into());
    j.terminal = true;
    f.store.save(&j).unwrap();
    let mut cleared = j.clone();
    cleared.stop_reason = None;
    assert_eq!(f.store.save(&cleared), Err(RunnerError::Journal));
    f.store.save(&f.store.fresh_journal().unwrap()).unwrap();
    assert!(f.store.load().unwrap().unwrap().stop_reason.is_none());
    let mut old_wire = serde_json::to_value(f.store.fresh_journal().unwrap()).unwrap();
    old_wire.as_object_mut().unwrap().remove("stop_reason");
    let old: Journal = serde_json::from_value(old_wire).unwrap();
    assert!(old.stop_reason.is_none());
}

#[tokio::test]
async fn investigator_supervisor_historical_receipt_checkpoint_loss_replays_exactly() {
    let f = Fixture::new();
    let mut c = f.client(1);
    c.submit_receipt = Some(historical_receipt());
    c.fault_after_submit = Some((0, super::super::journal::Fault::FileSync));
    let i = FakeInference::new(0);
    assert_eq!(
        run(&f, &mut c, i.clone()).await.err(),
        Some(RunnerError::Io)
    );
    let j = f.store.load().unwrap().unwrap();
    assert!(j.receipts.is_empty());
    assert!(j.work[0].phase == Phase::SubmissionPending);
    let original = j.pending.unwrap().request;
    let before = c.calls.len();
    tokio::time::resume();
    c.pause = true;
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 0);
    assert_eq!(s.reported, 1);
    assert_eq!(s.skipped, 0);
    assert_eq!(i.calls.load(Ordering::SeqCst), 1);
    assert_eq!(c.calls[before].1.tool, AllowedTool::Submit);
    assert_eq!(
        serde_json::to_vec(&c.calls[before].1.args).unwrap(),
        serde_json::to_vec(&original.args).unwrap()
    );
    assert_eq!(
        c.calls
            .iter()
            .filter(|(_, r)| r.tool == AllowedTool::Submit)
            .count(),
        2
    );
    let j = f.store.load().unwrap().unwrap();
    assert!(j.terminal);
    assert_eq!(j.receipts, vec![historical_receipt()]);
}
#[tokio::test]
async fn investigator_supervisor_first_run_stop_survives_later_permission_failure() {
    let f = Fixture::new();
    f.seed(Phase::Claimed);
    let mut j = f.store.load().unwrap().unwrap();
    j.stop_reason = Some("cancelled".into());
    f.store.save(&j).unwrap();
    let mut c = f.client(1);
    c.fail = Some((AllowedTool::GetRun, RunnerError::Permission));
    let i = FakeInference::new(0);
    let s = run(&f, &mut c, i.clone()).await.unwrap();
    assert_eq!(s.exit_code(), 4);
    assert_eq!(s.reason, "cancelled");
    assert_eq!(i.calls.load(Ordering::SeqCst), 0);
    assert_eq!(c.calls.len(), 1);
    let j = f.store.load().unwrap().unwrap();
    assert_eq!(j.stop_reason.as_deref(), Some("cancelled"));
    assert!(!j.terminal);
}

#[tokio::test]
async fn investigator_supervisor_readiness_precedes_claim() {
    let mut f = Fixture::new();
    f.config.allow_model_upload = false;
    let mut c = f.client(1);
    assert_eq!(
        run(&f, &mut c, FakeInference::new(0)).await.err(),
        Some(RunnerError::Config)
    );
    assert!(c.calls.is_empty());
    assert!(f.store.load().unwrap().is_none());
}
