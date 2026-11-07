# Controlled External Investigator: Slice 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an externally scheduled, report-only investigator that consumes slice 1's MCP workflow without giving a model catalog-write authority or falsely refreshing aging facts.

**Architecture:** A separate Rust controller owns MCP credentials, evidence pins, leases, durable requests and report submission. A tool-free Claude Code child receives only a bounded evidence packet and returns a validated proposal. External scheduling remains disabled until a separately authorized live smoke test and deployment review.

**Tech Stack:** Existing Rust 2021, clap, rmcp 3.8.0, reqwest 0.13.5, tokio 1.54, serde/schemars, chrono, tempfile, sha2 and getrandom. Promote existing client/HTTP dependencies/features as specified below; no model SDK, locking crate or direct SQL client path in the runner.

**Spec:** `docs/superpowers/specs/2026-11-06-okf-maintenance-slice-2-design.md`.

## Global Constraints

- "No canonical edits or publication; no database credentials; no new provider SDK; no autonomous shell/browser/MCP access for the model; no external URL fetching."
- "It never submits `Assessment::SupportedCurrent`."
- "Live operation requires both `allow_model_upload = true` and `acknowledge_inflight_budget_overshoot = true`, default false."
- "Automatic model retries | 0"; never repair model output by invoking another model.
- Primary/neighbor/prompt hard byte limits: 65,536 / 16,384 / 131,072. Include at most four neighbors from at most 20 discovered paths.
- Child stdout/stderr caps: 1,048,576 / 65,536 bytes. Per-child timeout: 90 seconds. Exactly one model process at a time.
- Batch default 10, accepted 1..50. Model-call default 10, maximum batch size and 50. Local run default 1,800 seconds, maximum 3,000. Heartbeat every 60 seconds, 30-second lease/deadline safety margin, RPC timeout 10 seconds.
- Spend reservation defaults USD 0.10 per invocation and USD 1.00 per run. Observed token stopping threshold 100,000. These are stopping thresholds, not hard in-flight token/billing guarantees.
- At most three attempts for a transient MCP call, delayed 1 then 2 seconds; mutation retries retain exact normalized payload and idempotency key. No retries after 401/403, protocol/schema errors, invalid leases or permission failures.
- No real provider calls in tests; no installs, enabled timers, production credentials, commits, pushes or deployments are authorized by this plan. Implementation is a separate decision.
- Slice 1 must be implemented first. Do not silently change its approved API to make this runner easier to build.

## Review Focus

1. A crash after a successful claim/submission but before local receipt persistence must replay the same request, not claim again or pay twice (Tasks 3, 6).
2. A queued lease or unresolved renewal near expiry must stop model work rather than permit submission with guessed authority (Tasks 5, 6).
3. Instruction-like prose, malicious citations and secret-bearing sources must remain data and never change subprocess arguments, tools, pins or credentials (Tasks 4, 5).
4. Missing cost/cache-token fields, oversized pipe output and CLI/version drift must fail closed, not appear as zero spending or valid proposals (Tasks 1, 5, 6).
5. Internally consistent but unverifiable old information must yield an inconclusive aging report, never SupportedCurrent or a canonical verification stamp (Tasks 4, 7).

## Dependency and file map

Start from the delivered implementation of `docs/superpowers/plans/2026-11-06-okf-maintenance-slice-1.md`, not the current planned-only server. Reuse its `RevisionPin`, `Lease`, `ClaimResult`, `Submission`, `Receipt`, finding/enumeration types and validators from `maintenance::types`/`maintenance::validate`. Use its exact MCP result wrappers with catalog fields. Keep controller code out of `App::poll` and server write paths.

| Files | Responsibility |
|---|---|
| `src/bin/okf-maintenance.rs` | Minimal async CLI entry, exit-code mapping; no server startup/migration |
| `src/investigator/{mod,types,config,error}.rs` | Controller-only contracts, config, consent and redacted failures |
| `src/investigator/{client,contract}.rs` | Allowlisted MCP, schemas, TLS/redirect policy, result parsing |
| `src/investigator/journal.rs` | Private state, file lock, atomic persistence, request recovery |
| `src/investigator/{collect,proposal}.rs` | Bounded pinned evidence and conservative report conversion |
| `src/investigator/claude.rs` | Tool-free subprocess, bounded pipes, envelope/accounting parser |
| `src/investigator/supervisor.rs` | Serial mutations, heartbeat, reservations, cancellation and resume |
| `src/investigator/evaluate.rs` | Offline fixture metrics and false-verification rejection |
| `tests/investigator.rs`, `tests/fixtures/investigator/` | Synthetic end-to-end and recorded-format tests, no paid inference |
| `examples/maintenance/{config.toml,okf-maintenance.service,okf-maintenance.timer}` | Disabled deployment examples only |
| `README.md`, `docs/operations.md` | CLI, privacy, permissions, smoke/schedule gates and limitations |
| `Cargo.toml`, `src/lib.rs` | Register runner module/binary and existing dependency features |

Order: Tasks 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8. This is intentionally sequential initially: safety policies cross the client, journal and supervisor interfaces.

## Shared runner contracts

All new persisted/wire DTOs derive Serialize/Deserialize as needed; all inbound config/proposal/journal structs use `#[serde(deny_unknown_fields)]`. Do not derive `Debug` for secrets, raw prompts, journals or raw process output. Failures have a safe, fixed kind plus optional IDs/counts; no child stderr, remote body, credentials or excerpts in Display/log output.

`types.rs` defines:

```rust
pub struct Binding {
    pub origin: String, pub endpoint: String, pub catalog: String,
    pub credential_sha256: String,
}
pub enum AllowedTool {
    Search, GetConcept, History, Claim, Renew, ReadRevision,
    Submit, Close, GetRun,
}
pub struct ToolRequest { pub tool: AllowedTool, pub args: serde_json::Value }
pub struct Source { pub id: String, pub pin: RevisionPin, pub text: String }
pub struct Packet {
    pub work_id: String, pub reasons: Vec<Lane>,
    pub primary_id: String, pub sources: Vec<Source>,
}
pub struct ModelRequest { pub prompt: String, pub model: String, pub budget_usd: String }
pub struct ModelUsage {
    pub input: u64, pub output: u64,
    pub cache_creation: u64, pub cache_read: u64,
    pub cost_microusd: u64,
}
pub struct ModelResult { pub proposal: Proposal, pub usage: ModelUsage }
pub struct RunSummary {
    pub run_id: Option<String>, pub reported: u32, pub inconclusive: u32,
    pub failed: u32, pub skipped: u32, pub invocations: u32,
    pub accounting_known: bool, pub reason: String,
}
```

Use `Binding.endpoint` as the full normalized HTTPS path/query, not origin alone: two MCP catalogs/services can share an origin. Fingerprint the token without storing the token. `AllowedTool::name(&self) -> &'static str` is a fixed mapping to the nine spec-approved tools; no arbitrary string-to-tool fallback.

`error.rs` defines `RunnerError` with redacted variants `Config`, `Busy`, `Permission`, `Contract`, `Transport`, `LeaseLost`, `Budget`, `InvalidEvidence`, `InvalidProposal`, `UnknownAccounting`, `Child`, `Journal`, `Cancelled`, `Io`. Exit status: Config/Contract preflight failures 2, local lock Busy 3, runtime failures 4, fully acknowledged run or empty batch 0. Inconclusive aging is not by itself an operational error. Never declare sources current from exit status.

## Task 1: Configuration, explicit consent and CLI preflight inputs

**Files:** Create `src/bin/okf-maintenance.rs`, `src/investigator/{mod,types,config,error}.rs`; modify `src/lib.rs`, `Cargo.toml`.

**Interfaces:** Define clap subcommands `preflight --config PATH`, `run-once --config PATH`, `evaluate --cases PATH`. Define `Cli { command: Command }` and `Command::{Preflight { config: PathBuf }, RunOnce { config: PathBuf }, Evaluate { cases: PathBuf }}` with clap derives in `src/bin/okf-maintenance.rs`. Main owns parsing and calls the corresponding public investigator functions; final dispatch is completed in Task 8. Do not make the library depend on binary-local CLI types. Pure config/parser tests can run before orchestration exists.

`RunnerConfig` fields: `endpoint: String`, `catalog: String`, `claude_binary: PathBuf`, `expected_claude_version: String`, `model: String`, `state_dir: PathBuf`, `allow_model_upload: bool`, `acknowledge_inflight_budget_overshoot: bool`, `require_hard_billing_cap: bool`, `batch_size: u32`, `max_invocations: u32`, `max_run_seconds: u64`, `invocation_microusd: u64`, `run_microusd: u64`, `observed_token_stop: u64`. Consent flags default false; hard-cap false is a statement of available controls, not consent. Model, endpoint, catalog, absolute binary/state paths and expected version are required; do not silently choose an alias/fallback/provider.

Defaults: batch 10, invocations 10 (clamped default to batch when omitted), time 1,800, invocation 100,000 microUSD, run 1,000,000 microUSD, token stop 100,000. Require all numerical budgets positive; batch 1..50; invocations ≤batch and ≤50; time 1..3,000; invocation reservation ≤run reservation. A hard-cap request is unsupported. Public run-once requires both consent flags; preflight may diagnose missing consent without claiming/inference. `preflight` must exit 2 if not ready, not falsely say a disabled configuration is runnable.

Define `Secrets` with redacted/no Debug and private strings loaded only from `OKF_MAINTENANCE_TOKEN` and `ANTHROPIC_API_KEY`. Required only for network preflight/live use, not offline evaluation. Never accept credential values in TOML or flags. Add `RunnerConfig::load(path: &Path) -> Result<Self, RunnerError>`, `validate(&self) -> Result<(), RunnerError>`, `Secrets::from_env() -> Result<Self, RunnerError>` and `binding(&self, token: &str) -> Result<Binding, RunnerError>`.

Promote existing reqwest 0.13.5 to regular dependencies with `default-features = false`, `features = ["json", "rustls"]`; its TLS feature was confirmed in the local registry manifest. Add rmcp `client` and `transport-streamable-http-client-reqwest` features to regular dependencies, keeping server features. Add tokio `process`, `io-util`, `fs` features. Retain dev-only test features. Register the second binary without changing the existing `okf-mcp` binary or Rust edition. Use the inspected standard library file-lock API; no new locking crate. Check lockfile changes from feature resolution, not an unrelated dependency update.

- [ ] **1.1 Write tests** `investigator_config_requires_explicit_consent`, `investigator_config_bounds_are_exact`, `investigator_binding_includes_full_endpoint`. Parse synthetic TOML and environment fixtures in isolated subprocesses rather than mutating shared test environment. Assertions:

```rust
assert!(!defaults.allow_model_upload);
assert!(!defaults.acknowledge_inflight_budget_overshoot);
assert_eq!(defaults.batch_size, 10);
assert_eq!(defaults.max_run_seconds, 1_800);
assert_eq!(defaults.invocation_microusd, 100_000);
assert_eq!(defaults.run_microusd, 1_000_000);
assert!(hard_cap_config.validate().is_err());
assert_ne!(binding_for_mcp_path, binding_for_other_path);
```

Check 0/51 batch, 3,001 seconds, unknown config/credential fields, relative executable paths, missing model/version, >batch invocations and a 401-safe token wrapper that cannot format its secret.
- [ ] **1.2 RED:** `cargo test investigator_config -- --nocapture`; expect missing parser/types before implementation.
- [ ] **1.3 Implement** the signatures and dependency feature promotion. Keep `run-once` unavailable until supervisor wiring is complete; no temporary live inference behavior.
- [ ] **1.4 GREEN:** repeat 1.2, `cargo test investigator_binding -- --nocapture`, `cargo check --all-targets`.
- [ ] **1.5 Review checkpoint:** confirm existing server startup and key handling are unchanged. Commit only if separately authorized.

## Task 2: Fail-closed MCP client and schema preflight

**Files:** Create `src/investigator/{client,contract}.rs`; add test modules and fixtures under `tests/fixtures/investigator/contract/`.

**Interfaces:** For controller/fake interchange, define:

```rust
pub trait CatalogClient: Send {
    fn list_tools(&mut self) -> impl Future<Output = Result<Vec<rmcp::model::Tool>, RunnerError>> + Send;
    fn call(&mut self, request: ToolRequest)
        -> impl Future<Output = Result<serde_json::Value, RunnerError>> + Send;
}
pub async fn connect(config: &RunnerConfig, secrets: &Secrets)
    -> Result<McpClient, RunnerError>;
pub fn validate_tools(tools: &[rmcp::model::Tool]) -> Result<(), RunnerError>;
```

`McpClient` implements this trait using the existing `ServiceExt::serve` and `StreamableHttpClientTransport::with_client` patterns. Supply a reqwest client with certificate verification, `redirect(Policy::none())`, `no_proxy()`, 10-second RPC deadlines and no body/error logging. Configure rmcp raw `max_sse_event_size` to 1,048,576. Disable SDK reconnection retries where its config allows, so outer retry policy owns the 1/2-second attempt budget; never let an inner reconnect silently reinvoke a mutation under a new ID. Preserve the documented limitation that JSON responses may buffer before size checking; this slice does not rewrite the transport.

Require HTTPS and no URL userinfo/fragment for real config. A private test constructor permits only HTTP loopback. Verify the actual final response origin if transport metadata exposes it; redirects remain rejected regardless. Do not inherit credentials from `.netrc`, proxies or unrelated MCP config.

`contract.rs` checks the nine allowed tools against actual slice-1 request/output schemas: catalog required; correct work/token/idempotency fields; enum spellings; no caller-provided actors/timestamps; structured result wrapper matches catalog and expected DTO. Do not compare an invented version/hash. Use actual schema fixtures captured from the slice-1 test server, plus explicit mismatch fixtures; resolve local JSON-schema refs when checking. Handle paginated list-tools with a bounded 10-page preflight and reject cyclic cursors. Extra server tools may exist but never enter AllowedTool.

Each call injects exactly one configured catalog (`catalogs: [catalog]` only for Search), rejects conflicting nested catalog fields, checks `isError` before decoding structured results, enforces a 1,048,576-byte serialized payload cap before application traversal, and validates pin revision >0 and returned catalog identity. Do not parse unstructured tool text as a successful fallback. Runtime auth failures stop; inaccessible IDs retain generic failure kinds.

- [ ] **2.1 Write tests** `investigator_contract_rejects_missing_or_changed_tools`, `investigator_client_rejects_redirects_and_cross_catalog`, `investigator_client_has_no_write_dispatch`. Use a loopback axum fake and the real slice-1 tool list; assertions:

```rust
assert!(validate_tools(&valid_slice1_tools).is_ok());
assert!(validate_tools(&tools_missing_required_token).is_err());
assert!(redirect_result.is_err());
assert_eq!(foreign_origin_received_credentials, 0);
assert!(wrong_catalog_result.is_err());
assert_eq!(allowed_tool_names.len(), 9);
assert!(!allowed_tool_names.contains(&"publish"));
assert!(!allowed_tool_names.contains(&"maintenance_dispose"));
```

Test HTTPS failure, 401/403, malformed structured result, schema refs, paginated/cyclic tool lists, oversized SSE and decoded JSON, and timeouts. Do not label decoded JSON checks as preallocation memory bounds.
- [ ] **2.2 RED:** `cargo test investigator_contract -- --nocapture`, `cargo test investigator_client -- --nocapture`.
- [ ] **2.3 Implement** the client/contract interfaces; use `tests/support/server.rs` as construction precedent, with stricter TLS/redirect policy. Do not introduce direct SQL reads into controller code.
- [ ] **2.4 GREEN:** repeat 2.2 against fake transport and actual slice-1 server.
- [ ] **2.5 Review checkpoint:** compare all parameter/result fields with the delivered slice-1 API; stop for a design revision on incompatibility rather than changing server semantics unilaterally.

## Task 3: Private locked journal and exact mutation replay

**Files:** Create `src/investigator/journal.rs`; tests in the same module and `tests/investigator.rs`.

**Interfaces:** Define `Journal { schema_version: u32, binding: Binding, run_id: Option<String>, leases: Vec<Lease>, work: Vec<WorkState>, pending: Option<PendingMutation>, receipts: Vec<Receipt>, invocations: u32, reserved_microusd: u64, usage: ModelUsage, accounting_known: bool, terminal: bool }`, `PendingMutation { request_id: String, request: ToolRequest }`, and `WorkState { work_id: String, phase: Phase, packet: Option<Packet>, proposal: Option<Proposal>, usage: Option<ModelUsage>, failure: Option<String> }`. Initialize schema_version 1, counters/usage zero, accounting_known true and terminal false; failure strings are fixed safe tags only, never raw errors. PendingMutation.request_id must equal its request's idempotency_key. Durable work phases: claimed, packet_ready, invocation_started, proposal_ready, submission_pending, reported, abandoned. `RunSummary` is public-safe; Journal is not.

Produce `JournalStore::open(state_dir: &Path, binding: &Binding) -> Result<JournalStore, RunnerError>`, `load(&self) -> Result<Option<Journal>, RunnerError>`, `save(&self, state: &Journal) -> Result<(), RunnerError>`. Keep a stable 0600 lock file open and exclusively locked with `File::try_lock` until drop; never rename/delete that inode. Reject symlinked/nonprivate state or journal paths and wrong owner on Linux. Obtain the process filesystem/effective identity from kernel `/proc/self/status`'s `Uid:` fields and require all four IDs equal; reject setuid/mixed-identity execution. Compare with `MetadataExt::uid`, not a user-controlled UID environment variable or shell helper. This runner is Linux-only initially; fail explicitly on unsupported platforms. Atomically save via a 0600 temporary file in the same directory, fsync it, rename, then sync the parent directory. Do not reuse the lock inode as the journal inode.

The controller generates request keys with slice 1's `maintenance::types::new_id`. Save PendingMutation before calling MCP. On success, commit receipt/lease updates and clear pending in one journal save. A transport ambiguity leaves pending intact; only replay the same request/key. Recovery of a lost claim reply uses the persisted claim, recovering run ID/leases from idempotent replay. Keep at most one outstanding mutation; the supervisor arbitrates renew/submit/close through this slot.

Binding mismatches, corrupt/unknown journal versions and unsafe permissions fail closed. Terminal journals remain until atomically replaced at the start of the next authorized run; a completed run is not accidentally resumed as a new claim. Do not silently clean up another process's lock or discard unresolved work.

- [ ] **3.1 Write tests** `investigator_journal_lock_survives_atomic_save`, `investigator_journal_replays_lost_claim`, `investigator_journal_rejects_corruption_and_rebinding`. Use two processes for locking and a fault-injected fake client/store. Assertions:

```rust
assert!(second_process_open_is_busy);
assert_eq!(recovered_request.request_id, original_request.request_id);
assert_eq!(recovered_request.request.args, original_request.request.args);
assert_eq!(claim_call_count_with_distinct_keys, 1);
assert!(changed_catalog_or_token_or_endpoint_is_rejected);
assert_eq!(journal_mode & 0o777, 0o600);
assert_eq!(state_mode & 0o777, 0o700);
```

Crash at before-remote, remote-success-before-save, and after-save boundaries. Inject rename/fsync failure; prove no replacement claim is sent and no partially written JSON is treated as empty state. Test lock release after process death, corrupt JSON, secret-free public summaries and terminal journal replacement.
- [ ] **3.2 RED:** `cargo test investigator_journal -- --nocapture`.
- [ ] **3.3 Implement** journal/lock contracts without a new locking dependency; use the repository's inspected standard library/toolchain. Keep the Linux/Unix scope explicit rather than adding unsupported platform shims.
- [ ] **3.4 GREEN:** repeat 3.2 including two-process exclusion and crash tests.
- [ ] **3.5 Review checkpoint:** ensure no API token is saved and no sensitive journal field has automatic log/debug output.

## Task 4: Bounded evidence collection and conservative report conversion

**Files:** Create `src/investigator/{collect,proposal}.rs`, synthetic corpus under `tests/fixtures/investigator/corpus/`.

**Interfaces:**

```rust
pub enum CollectStep { Request(ToolRequest), Ready(Packet) }
// Collector has private discovery/pinning state; one request awaits a reply.
impl Collector {
    pub fn new(lease: &Lease) -> Result<Self, RunnerError>;
    pub fn next(&mut self) -> Result<CollectStep, RunnerError>;
    pub fn accept(&mut self, response: serde_json::Value) -> Result<(), RunnerError>;
}
pub fn render_prompt(packet: &Packet) -> Result<String, RunnerError>;
pub fn to_submission(packet: &Packet, proposal: &Proposal)
    -> Result<Submission, RunnerError>;
```

Define `Proposal { summary: String, findings: Vec<ProposedFinding> }`, `ProposedFinding { category: Category, assertion: String, follow_up: String, uncertainty: String, relationship: Relationship, evidence: Vec<ProposedEvidence> }`, `ProposedEvidence { source_id: String, excerpt: String }`. No model-provided outcome, assessment, actor, path, revision, URL observations or run/lease fields. `Relationship::ExternalObservation` is rejected in this catalog-only version. Define `proposal_schema() -> serde_json::Value` for this exact JSON shape. Proposal size/finding/evidence/excerpt limits reuse slice-1 validators after ID mapping.

Drive Collector one read at a time through the supervisor; between requests it must service due renewals and cancellation. Do not let a monolithic collection future monopolize the mutable MCP client across dozens of calls. `next` cannot advance an unanswered request; `accept` validates and advances that request only.

Primary must match the claim's exact current pin; outdated/oversized/secret-bearing primary returns a typed error for a generic, evidence-free inconclusive/failed outcome, never a silently truncated packet. Neighbor discovery uses resolved outbound/backlink paths first, then lexical searches for title, aliases, basename tokens (deduplicate terms, at most 5 queries, each query at most 128 UTF-8 bytes). Responses contribute paths only. Bound considered paths to 20, include at most four complete neighbors ≤16,384 bytes. Sort linked paths before lexical hits, then lexically by normalized path. Pin through history/read-revision, rejecting stale or missing replies. Sources get deterministic IDs `s0` primary, `s1`… neighbors; never take IDs from catalog prose.

Raw primary ≤65,536 bytes; whole rendered prompt ≤131,072 bytes including instructions/escaping. Drop lowest-ranked neighbors until it fits; if primary alone cannot fit, return inconclusive before model upload. Preserve exact bytes/CRLF; scan each raw text before JSON escaping. Prompts explicitly separate instructions from quoted evidence and request proposals only, no current-world verification. Model cannot cause another discovery round.

Convert all valid aging work to Inconclusive/Unverifiable even if it contains useful connection findings; changed-only valid work completes NotAssessed. Include primary evidence with a short exact excerpt when available, respecting slice-1 ≤2,048-byte/20-evidence limits. A failed/inconclusive read without usable primary evidence uses empty assessment evidence, permitted by slice 1. Never produce SupportedCurrent or Outdated assessments from this version. Comparisons require two supplied distinct concept paths. Unknown IDs, invented excerpts, secret-bearing output, unsupported categories/relationships and external observations reject the proposal, not repair it.

- [ ] **4.1 Write tests** `investigator_collection_bounds_and_pins`, `investigator_proposal_cannot_refresh_aging`, `investigator_prompt_injection_remains_data`. Assert:

```rust
assert_eq!(packet.sources[0].pin, lease.pin);
assert!(packet.sources.len() <= 5);
assert!(rendered_prompt.len() <= 131_072);
assert_eq!(aging_submission.outcome, WorkOutcome::Inconclusive);
assert_eq!(aging_submission.assessment, Assessment::Unverifiable);
assert_eq!(changed_submission.assessment, Assessment::NotAssessed);
assert!(invented_source_or_excerpt.is_err());
assert_eq!(model_requested_additional_reads, 0);
```

Test 65,536/65,537 primary bytes, 16,384/16,385 neighbor bytes, escaped JSON inflation, UTF-8 boundaries, exact CRLF excerpts, empty body, nonexistent history, source edited during collection, 20-path cap, linked-vs-lexical ranking, duplicated sources, copied assertions and distinct similarly named entities. Use fake secrets assembled at test runtime; do not commit realistic credentials.
- [ ] **4.2 RED:** `cargo test investigator_collection -- --nocapture`, `cargo test investigator_proposal -- --nocapture`, `cargo test investigator_prompt -- --nocapture`.
- [ ] **4.3 Implement** collection and conversion with slice-1 validators and immutable packet ID mapping. Never upload source until all collection and secret/size checks succeed.
- [ ] **4.4 GREEN:** repeat 4.2; verify no write tool or external URL fetch appears in captured client requests.
- [ ] **4.5 Review checkpoint:** explicitly inspect no-verification behavior for work carrying both Changed and Aging reasons.

## Task 5: Tool-free Claude subprocess with bounded pipes and accounting

**Files:** Create `src/investigator/claude.rs`; add `tests/fixtures/investigator/claude/` synthetic envelope/usage fixtures and a fake-child test executable.

**Interfaces:**

```rust
pub trait Inference: Send + Sync {
    fn infer(&self, request: ModelRequest, cancel: CancellationToken)
        -> impl Future<Output = Result<ModelResult, RunnerError>> + Send;
}
pub async fn preflight_cli(config: &RunnerConfig) -> Result<(), RunnerError>;
pub fn parse_envelope(bytes: &[u8]) -> Result<ModelResult, RunnerError>;
```

Define `ClaudeAdapter` with private trusted config/provider-secret fields and `ClaudeAdapter::new(config: &RunnerConfig, secrets: &Secrets) -> Result<Self, RunnerError>`; implement Inference for it. Preflight runs only `--version` and `--help`, with bounded capture; exact version must match configured expected version, all required flags must be present. The inspected version is `2.1.319 (Claude Code)`, not proof its paid envelope matches. Do not call a model to discover support during preflight.

Construct argv directly: `--print --output-format json --json-schema <proposal_schema> --tools "" --strict-mcp-config --mcp-config {"mcpServers":{}} --bare --restricted --disable-slash-commands --no-session-persistence --no-chrome --setting-sources "" --model <configured-id> --max-budget-usd <reserved-decimal>`. No shell, dynamic flags from sources, plugins, bypass permissions, resume or fallback. Prompt travels only on stdin. Use a fresh TempDir (0700) for cwd/HOME; clear environment and add only trusted PATH, HOME, TMPDIR, LANG and the dedicated `ANTHROPIC_API_KEY`. MCP key/database/user-config/proxy/other-provider variables must not be inherited. Binary path resolves before clearing env.

Spawn pipe-draining tasks so inference output cannot block heartbeats. Hard cap stdout 1,048,576 bytes, stderr 65,536 bytes, timeout 90 seconds. Timeout/cancel/overflow kills and reaps the owned child; readers do not continue unbounded allocation. Keep raw stderr/output out of errors and logs. It is process/tool restriction, not an OS sandbox or egress firewall.

Only accept the spec's target envelope: success type/subtype, is_error false, object `structured_output` matching Proposal, finite nonnegative `total_cost_usd`, and all four explicit nonnegative integer usage counts. Reject overflow when summing counts. Parse decimal dollars into microUSD rounding upward; reject negative, NaN/infinity, overflow and absurd precision rather than undercounting. Unknown accounting is a fatal safe error, not a zero-cost successful ModelResult. No plain-text or alternate-envelope fallback. Synthetic fixtures must say they are synthetic; sanitized live fixture capture needs separate authorization and review.

- [ ] **5.1 Write tests** `investigator_claude_has_no_tools_or_mcp_secret`, `investigator_claude_bounds_pipes_and_reaps`, `investigator_claude_envelope_requires_accounting`. Fake child records argv/environment to a private test channel, emits fixtures, stalls, or floods pipes. Assertions:

```rust
assert_eq!(child_tool_list, "");
assert_eq!(child_mcp_servers, serde_json::json!({}));
assert!(!child_env.contains_key("OKF_MAINTENANCE_TOKEN"));
assert!(!child_env.contains_key("DATABASE_URL"));
assert!(prompt_absent_from_argv);
assert!(missing_cache_counter_is_unknown_accounting);
assert_eq!(parsed_usage.cost_microusd, 100_000);
assert!(owned_child_reaped_after_timeout);
```

Test exact pipe boundaries/one byte over, stderr source leakage, malformed JSON, negative/overflow usage and costs, missing structured output, model instruction trying to change argv, wrong CLI version, missing flags, cancellation and simultaneous stdout/stderr output. Fake process has no provider credentials or network access.
- [ ] **5.2 RED:** `cargo test investigator_claude -- --nocapture`.
- [ ] **5.3 Implement** the adapter/parser with tokio process/IO features and separate redacted provider secret access. Do not substitute personal interactive Claude settings or OAuth when the provider key is missing.
- [ ] **5.4 GREEN:** repeat 5.2 and confirm tests never execute the real configured Claude binary or use a provider key.
- [ ] **5.5 Review checkpoint:** verify the documented hard-limit versus stop-threshold distinction and the remaining non-OS-sandbox trust boundary.

## Task 6: Supervisor, heartbeat, budget reservations and crash recovery

**Files:** Create `src/investigator/supervisor.rs`; extend journal/controller test support.

**Interfaces:**

```rust
pub async fn run_once<C: CatalogClient, I: Inference + 'static>(
    config: &RunnerConfig, client: &mut C, inference: Arc<I>,
    store: &JournalStore, clock: &dyn Clock, cancel: CancellationToken,
) -> Result<RunSummary, RunnerError>;
```

Controller owns one mutation slot and all idempotency keys. `Arc<I>` gives the spawned inference task owned lifetime without borrowing a stack-local adapter; clone the Arc into that task, not the MCP client or journal. Client receives no concurrent mutable access; an independent inference task drains/processes child output while the supervisor services renewals. Persist every started invocation and reservation before spawn. Use integer microUSD, checked token sums, monotonic elapsed time for local budget, and server UTC lease/deadline timestamps for authority with a 30-second safety margin. A system-clock jump must stop rather than extend a local run or assume an expired lease is valid.

New-run sequence: CLI readiness/consent and schemas → acquire/load journal → recover pending requests → preflight version/help → durable claim request → retrieve run deadline → collect each packet → reserve/persist invocation_started → spawn at most one child → process renewals while waiting → validate/store proposal → submit with durable request key → acknowledge receipts → final close/terminal journal. An empty batch records completion without inference. A terminal journal can be replaced only after its terminal state is durable.

For recovery, perform the same schema/auth checks first and replay the pending mutation before fresh work. Recover missing claim receipts using original key. Never invoke a model again for a work item marked invocation_started after an unclean exit; mark accounting unknown, perform only recovery/report/close operations, and stop further inference for that run. Preserve any valid proposal already durably stored and submit it only if ownership remains valid. Interrupted reservations are not refunded. A later new scheduler run is a new spending window, subject to server cooldown; do not pretend the per-run threshold is a daily cap.

Renew every unsubmitted lease, including queued items, every 60 seconds during discovery, pin reads, inference, validation and submission preparation. The supervisor drives Task 4's Collector requests individually so collection cannot starve heartbeats. Each renewal is journaled and serialized, with priority over submission. Drain a pending renewal before submitting; a lost renewal response blocks submission/inference until identical replay acknowledges expiry at least 30 seconds ahead. At most three transport attempts with 1/2-second delays, all within RPC/run/lease margins. Loss of lease/authority, repeated transport failure, corrupt state, unknown accounting or cancellation cancels/reaps the child and stops new inference. Do not retry a model.

End local work by the earlier of local 1,800-second deadline or server deadline minus 30 seconds. Budget/time limits can leave unused leases. When reachable and still authorized, submit generic failure/inconclusive results for remaining owned work and close with reason; pending mutations must first resolve. If unavailable/expired/revoked, retain the journal and let server expiry recover authority. A clean transport with acknowledged inconclusive aging outcomes may exit 0. Runtime/protocol/budget interruption yields exit 4 even when partial findings were saved.

- [ ] **6.1 Write tests** `investigator_supervisor_renews_queued_work`, `investigator_supervisor_never_repays_ambiguous_inference`, `investigator_supervisor_replays_mutations_before_new_work`, `investigator_supervisor_stops_on_accounting_or_clock_failure`. Use fixed/injectable Clock and paused Tokio time, fake MCP, fake Inference and a fault-injected journal. Assertions:

```rust
assert_eq!(maximum_simultaneous_models, 1);
assert!(all_unsubmitted_leases_renewed_at_60_seconds);
assert_eq!(invocations_after_recovery, invocations_before_crash);
assert_eq!(replayed_mutation_payload, original_mutation_payload);
assert_eq!(reserved_microusd_after_crash, reserved_microusd_before_crash);
assert!(no_submission_passed_ambiguous_renewal);
assert_eq!(next_model_calls_after_unknown_accounting, 0);
assert_eq!(max_default_invocation_count, 10);
```

Exercise lease expiry/replacement, reissued same-name credential, 401/403 after a poll, crash at each durable phase, malformed output, zero-neighbor/large-primary skip, explicit cancel, queued child completion during a renewal, slow multi-request collection that crosses heartbeat deadlines, lower server deadline, exact budget boundary, accounting >reservation, overflow and system clock jumps. Document that billed overshoot cannot be retroactively prevented. Test no lost-response case generates another request key.
- [ ] **6.2 RED:** `cargo test investigator_supervisor -- --nocapture`.
- [ ] **6.3 Implement** the supervisor state machine and exact failure/recovery policy. Counter/receipt changes and phase changes share one atomic journal save; do not scatter independent checkpoint files.
- [ ] **6.4 GREEN:** repeat 6.2; run all journal/process tests together to expose cancellation/persistence interactions.
- [ ] **6.5 Review checkpoint:** manually trace crash after remote claim, remote submit, invocation start and unknown renewal. A token, paid call or completed report must not be silently duplicated.

## Task 7: Real-server acceptance and honest offline evaluation

**Files:** Create `tests/investigator.rs`, `src/investigator/evaluate.rs`; extend synthetic fixtures under `tests/fixtures/investigator/`.

**Interfaces:** `evaluate::run(cases: &Path) -> Result<EvaluationSummary, RunnerError>`, with `EvaluationSummary { cases: u32, true_positive: u32, false_positive: u32, false_negative: u32, rejected_citations: u32, false_verification: u32, rejected_verification: u32, excluded_unreviewed: u32, precision: Option<f64>, recall: Option<f64> }`.

Cases pair a synthetic Packet/recorded Proposal with human-authored expected relation keys `(category, sorted distinct evidence paths)` and human admissibility annotations for ambiguous assertions. Compute exact key matches only for admissible, citation-valid proposals; retain ambiguous/unreviewed cases as excluded from precision/recall with explicit counts in the output. `false_verification` counts accepted proposals that wrongly certify currency; `rejected_verification` counts blocked verification attempts. The former must remain zero; negative fixtures must make the latter positive. Undefined denominator is null, not a perfect score. Do not use another model as ground truth or call a provider during evaluate. Report fixture structural metrics, not proof of real-world truth or live-model quality. Fixture metadata labels synthetic versus reviewed captured output and contains no secrets/private catalog data.

Run supervisor against actual slice-1 TestServer with fake inference. Compare all canonical document/revision rows and unpacked archive entries/bytes/modes before/after successful changed reports, inconclusive aging, cancellation, failed validation and replay. Capture client tool names and prove they belong to the nine-tool allowlist. Use distinct catalog fixtures to test guessed IDs and response mismatch handling.

- [ ] **7.1 Write tests** `investigator_end_to_end_is_report_only`, `investigator_evaluation_reports_false_verification`, `investigator_evaluation_does_not_call_a_model`. Include correct/incorrect links, similar-name distinct entities, copied claims, historical changes, contradictions, aging unknowns and injected instructions. Assertions:

```rust
assert_eq!(canonical_rows_before, canonical_rows_after);
assert_eq!(canonical_archive_before, canonical_archive_after);
assert_eq!(canonical_search_before, canonical_search_after);
assert!(all_reported_aging_assessments_are_unverifiable);
assert_eq!(provider_calls_during_tests, 0);
assert_eq!(evaluation.false_verification, 0);
assert!(evaluation.rejected_verification > 0);
assert_eq!(empty_denominator_precision, None);
```

Add negative fixtures that attempt SupportedCurrent/outcome injection and assert rejection is counted. Force a lost successful submission response and check one stored report. Check false-positive/negative counts against hand-computed fixture values; do not invent a live quality score.
- [ ] **7.2 RED:** `cargo test --test investigator -- --nocapture`, `cargo test investigator_evaluation -- --nocapture`.
- [ ] **7.3 Implement** evaluator and integration fixture adapters; do not add a model/provider dependency or bypass the MCP transport with SQL in controller production code. SQL is only for test assertions.
- [ ] **7.4 GREEN:** repeat 7.2 and relevant slice-1 `tests/maintenance.rs` regressions.
- [ ] **7.5 Review checkpoint:** distinguish operational success, reported findings, structural evidence validity and actual factual verification in CLI output and evaluator documentation.

## Task 8: Operator CLI, disabled scheduling examples and full gates

**Files:** Complete `src/bin/okf-maintenance.rs` and module dispatch; create `examples/maintenance/{config.toml,okf-maintenance.service,okf-maintenance.timer}`; update `README.md`, `docs/operations.md`. No CI or live infrastructure changes.

**Interfaces:** Complete binary-local `async fn dispatch(cli: Cli) -> Result<i32, RunnerError>`, calling `client::connect`, `claude::preflight_cli`, `JournalStore::open`, `ClaudeAdapter::new`, `supervisor::run_once`, and `evaluate::run` as appropriate; keep the public library interfaces defined in their owning tasks. `preflight` reads config/credential presence/version/help and MCP schemas without claims/mutations/inference; `run-once` performs one bounded batch; `evaluate` is offline. Write RunSummary/evaluation JSON to stdout and redacted fixed errors to stderr; diagnostics must not corrupt machine-readable stdout. Document exit 0/2/3/4 precisely; no inference from exit 0 to factual freshness.

Config example includes placeholder HTTPS endpoint/catalog/model, absolute binary/state paths, exact expected version and explicit false consent flags. Examples contain no real internal addresses, API values or copied workstation paths. Document separate API authentication despite an existing interactive Claude subscription, and upload of catalog text to the configured provider. Explain 0.10/1.00 USD reservations, unknown-cost behavior, and no hard in-flight billing/token guarantee. Explain decoded-JSON versus raw-SSE limits and recommend controller `MemoryMax=512M`; an OOM kill is a recoverable crash, not a clean report.

Use a disabled systemd oneshot service with dedicated User, protected EnvironmentFile paths, UMask=0077, StateDirectory, PrivateTmp, NoNewPrivileges, ProtectSystem=strict, ProtectHome, explicit writable private state and a pinned executable outside a user home. Timer: daily, randomized delay, persistent catch-up, same service unit. Because Type=oneshot does not give the desired execution cap via RuntimeMaxSec, use `TimeoutStartSec=1800` and `KillMode=control-group`, with the service example's `max_run_seconds = 1740` leaving 60 seconds before the unit's 1,800-second cap for orderly shutdown. The standalone default remains 1,800 seconds. Do not add `[Install]` enabling actions to development scripts; documentation may show commands an operator can choose after authorization, not execute them here. Do not promise systemd filesystem restrictions are a network egress allowlist.

Before live scheduling, require a separately authorized one-primary smoke run with upload/budget acknowledgement and sanitized envelope capture against the exact configured CLI/model. Help/version and synthetic parser tests do not satisfy this live gate. The default batch may be configured to 1, with one invocation and a 0.10 USD reservation threshold; review resulting evidence, accounting and absence of canonical writes. Timer remains disabled on failed or absent smoke evidence. Provisioning keys/accounts/provider quotas remains an operator action outside this plan.

- [ ] **8.1 Add CLI tests** `investigator_cli_preflight_is_nonmutating`, `investigator_cli_evaluate_is_offline`, `investigator_examples_are_disabled_and_redacted`. Assert no claim/model process during preflight, no credentials/network during evaluation, missing consent exits 2, local lock contention exits 3, partial recovery exits 4, and acknowledged empty/inconclusive runs exit 0. Assert examples have both consent flags false and no enabling side effects.
- [ ] **8.2 Complete wiring/docs** with exact commands and prerequisites; keep credentials in environment/protected files, never flags. Document state backup/privacy, crash recovery, SDK buffering limits, provider accounting uncertainty, and how to diagnose fixed error codes without enabling prompt/secret logs.
- [ ] **8.3 Run full gates:**

```sh
just fmt
just lint
just test
git diff --check
```

Expect formatting, warning-free clippy/rustdoc and all existing plus new tests to pass. Docker is required for actual slice-1/server tests. Report failures; do not fabricate a green result by skipping containers or paid-output verification. No live model calls are part of these commands.
- [ ] **8.4 Check spec coverage:** config/consent (1), transport/contracts (2), crash/exclusion (3,6), evidence/no false freshness (4), process/accounting (5), budgets/leases/recovery (6), canonical preservation/evaluation (7), scheduling/operator/live gate (8). All five Review Focus risks must have the named tests above. Confirm no server schema/API changes were smuggled into slice 2.
- [ ] **8.5 Final review checkpoint:** fresh whole-change review in the separately selected execution workflow. No release, commit, paid smoke, install, timer enablement or deployment without current authorization.

## Execution handoff

Slice 1 implementation and contract validation come first. Human review of this plan is required before implementation. Recommend subagent-driven execution because client/journal/process/supervisor failures interact and may leak private evidence or spend money even without canonical writes. Native execution is cheaper, but independent review happens only after the whole implementation. Neither method authorizes paid inference or an enabled schedule.
