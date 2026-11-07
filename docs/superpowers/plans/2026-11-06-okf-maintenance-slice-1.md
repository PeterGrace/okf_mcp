# Report-only Catalog Maintenance: Slice 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give humans and external investigators a safe, durable MCP workflow for reporting catalog connections and aging information without modifying canonical content.

**Architecture:** Postgres owns maintenance state and revision pins; a pure selector allocates work fairly between changed and aging concepts. MCP authorizes each operation and records reports separately from documents. A future external runner consumes these interfaces; this plan neither builds nor deploys that runner.

**Tech Stack:** Existing Rust 2021, axum/rmcp, sqlx/Postgres 17, chrono, serde/schemars, getrandom, and sha2. No new dependency or model provider is needed.

**Spec:** `docs/superpowers/specs/2026-11-06-okf-maintenance-design.md`.

## Global Constraints

- "No automatic canonical edits, merges, splits, deletions, verification stamps, or changeset publication."
- "Keep existing tool contracts unchanged."
- "Default batch size is 10, maximum 50."
- "Initial review interval is 30 days, catalog-configurable with a minimum of one day."
- "Failed or inconclusive investigations become eligible after a 24-hour retry cooldown."
- "Work is claimed transactionally with a 15-minute lease and an opaque lease token."
- "Cap run duration at one hour and primary concepts at 50."
- "Default limits: 20 findings per primary work item, 20 evidence references per finding, 2 KiB per excerpt, and 256 KiB per submission."
- Every operation requires an explicit catalog. All timestamps come from `App::clock()`/the caller's `Clock`, never MCP parameters or SQL `now()` defaults.
- No scheduler, inference, external fetching, embeddings, cross-catalog synthesis, new UI, or generated-report search indexing.
- The design and this plan remain uncommitted until the user authorizes git actions. Commit checkpoints below apply only to an implementation session separately authorized to commit; otherwise leave changes local.
- Planning does not authorize implementation. Preserve the existing untracked design file.

## Review Focus

Each risk is pinned to a test below, not merely left for a reviewer:

1. Revocation, capability removal, and name reuse during a lease must not transfer ownership or retain access after the existing auth refresh (Tasks 2, 8).
2. UTF-8, CRLF source bytes, and malicious source prose must not corrupt excerpt matching or execute instructions (Tasks 5, 8).
3. New source revisions, deletion/recreation of a path, and deletion/recreation of a catalog must not inherit successful work or authorize old findings (Tasks 4, 7).
4. A lost response, expired lease, competing claim, or retry at a time boundary must not duplicate results, starve aging work, or complete another worker's attempt (Tasks 3, 4, 6).
5. Huge inputs, future/invalid legacy stamps, and unavailable external sources must not falsely renew verification, leak secrets, or silently trigger network requests (Tasks 3, 5, 8).

## Repository map and dependencies

Baseline: `b070e6a`. Follow runtime-checked SQL and adjacent module tests. `src/db.rs::test_db()` and `tests/support/pg.rs::TestDb` provide migrated, disposable Postgres; Docker is required. `src/mcp/testing.rs::app_at` accepts an injected clock. `tests/support/server.rs::{TestServer,call,structured}` exercise real MCP HTTP.

| Files | Responsibility |
|---|---|
| `migrations/0006_maintenance.sql` | Capability and maintenance-only tables, constraints, indexes |
| `src/maintenance/{mod,types,select,validate}.rs` | Shared contracts, deterministic scheduling, shape/secret checks |
| `src/store/maintenance/{mod,candidates,runs,evidence,reports}.rs` | Short transactions, authoritative revision reads, findings and dispositions |
| `src/mcp/maintenance/{mod,work,reports}.rs` | Parameters, authorization, result schemas, tool routing |
| `src/{lib,config,auth,cli}.rs`, `src/store/{mod,keys}.rs` | Register modules, settings, reporting capability and CLI |
| `src/mcp/mod.rs`, `src/mcp/testing.rs`, `tests/support/server.rs` | Register tools and extend existing fixtures |
| `tests/maintenance.rs` | Real HTTP isolation and lifecycle acceptance tests |
| `README.md`, `docs/operations.md` | Workflow, CLI, permissions, limits and export boundaries |

Dependency order: 1 → 2 and 3; 1+3 → 4; 1+4 → 5; 4+5 → 6; 6 → 7; 2+4+5+6+7 → 8; all → 9. Execute sequentially initially: the contracts cross task boundaries, and schema/capability literals affect existing tests.

## Shared contracts: decisions implementers must not reinvent

All new public DTOs derive `Debug`, `Clone`, `PartialEq`, `Serialize`, `Deserialize`, and `JsonSchema` where representable. Use snake_case serialized enum values and `#[serde(deny_unknown_fields)]` on new request structs. Database paths have no leading slash; MCP paths use full `/...md` form. Reject traversal, non-positive revisions, blank IDs, and invalid enum values. Normalization must use existing path helpers, not filesystem resolution.

`src/maintenance/types.rs` defines these types (all fields public):

```rust
pub struct Actor { pub key_id: String, pub actor: String }
pub struct RevisionPin { pub path: String, pub revision: i32 }
pub enum Lane { Changed, Aging }
pub enum AgingGroup { Expiry, Verification }
pub enum WorkOutcome { Completed, Inconclusive, Failed, Superseded }
pub enum Assessment { SupportedCurrent, Outdated, Unverifiable, NotAssessed }
pub enum RunState { Running, Completed, Failed, Cancelled }
pub enum Category {
    SuggestedLink, PossibleDuplicate, ConflictingAssertions,
    PotentiallyOutdatedInformation, UnverifiableClaim,
}
pub enum Relationship { DirectStatement, Inference, ExternalObservation }
pub enum Disposition { Open, AcceptedForFollowUp, Dismissed, NeedsInput }
pub struct Evidence { pub pin: RevisionPin, pub excerpt: String }
pub struct ExternalObservation {
    pub resource: String, pub excerpt: String, pub attribution: String,
}
pub struct FindingInput {
    pub category: Category, pub assertion: String, pub follow_up: String,
    pub uncertainty: String, pub relationship: Relationship,
    pub evidence: Vec<Evidence>, pub external: Vec<ExternalObservation>,
}
pub struct Submission {
    pub outcome: WorkOutcome, pub assessment: Assessment,
    pub summary: String, pub evidence: Vec<Evidence>,
    pub findings: Vec<FindingInput>, pub reported_tokens: Option<u64>,
}
pub struct Lease {
    pub run_id: String, pub work_id: String, pub token: String,
    pub pin: RevisionPin, pub reasons: Vec<Lane>,
    pub expires_at: String,
}
pub struct ClaimResult { pub run_id: String, pub leases: Vec<Lease> }
pub struct Receipt {
    pub run_id: String, pub work_id: String, pub outcome: WorkOutcome,
    pub finding_ids: Vec<String>, pub outdated: bool,
}
```

Further result DTOs are defined in the task that produces them. They use string-formatted UTC timestamps at the MCP boundary; internal scheduling uses `DateTime<Utc>`.

### Run and idempotency semantics

One `maintenance_claim` opens one run with one batch, not an unbounded session. Omitted batch size is 10; accept 1..=50, reject 0 or >50. A run with no candidates is immediately completed. All its primary concepts count toward the 50-concept cap, including failed attempts. A subsequent batch is a new run. This keeps slice 1 usable without introducing another queue API.

A run's deadline is creation + 3,600 seconds. Initial leases are creation + 900 seconds; renew to `min(now + 900 seconds, deadline)`. A lease is expired at `now >= expires_at`; deadline exhaustion occurs at `now >= deadline`. Budget exhaustion closes the run as failed with reason `budget_exhausted` while retaining completed work. Each work row is either leased (`outcome IS NULL`) or terminal. Expiry records a failed attempt with reason `lease_expired`, eligible for immediate reassignment; explicit failed/inconclusive submissions have the 86,400-second cooldown. Keep crash recovery distinct from an investigator reporting failure.

A new claim reclaims expired work as a new attempt in a new run with a new token; old receipts and rows survive. Closing a run as failed/cancelled invalidates its remaining leases and releases that work for later runs. Explicit close-as-completed requires every item terminal. Completing the last item automatically completes a running run. Operators can cancel another credential's run only with ordinary `Write`; reporter keys may close their own runs only.

All mutations accept `idempotency_key` (1..=128 ASCII characters, no whitespace). Scope keys by `(catalog_id, credential_id, operation, idempotency_key)`; scan keys with the existing secret scanner before persistence. Store normalized request JSON and the response in the same transaction as the mutation. Comparison is semantic DTO equality, not raw JSON key order. Identical retry returns the response without touching timestamps, leases, cursors, or counts; conflicting payload returns `Refused`. Always authorize before replay. Never expose an old lease token to another key. Idempotency lookup precedes lease expiry checks after authorization: a retry of an already successful submission returns its original receipt even after expiry.

### Evidence and freshness semantics

Every pin implicitly belongs to the request's one catalog; callers cannot attach another catalog to a nested pin. Every completed item must include primary-pin evidence in `Submission.evidence`; failed/inconclusive attempts can have none. `SupportedCurrent` requires `Completed` plus evidence and is an attributed investigator assessment, not server/human verification. `Outdated` requires `Completed` and supporting evidence. `Unverifiable` requires `Inconclusive`; failures use `NotAssessed`. Changed-only analysis can complete with `NotAssessed`. If aging is among the reasons, completed work requires `SupportedCurrent` or `Outdated`.

A completed changed analysis covers only its exact revision. A completed aging investigation schedules another attempt after the review interval whether it found current or outdated data; only `SupportedCurrent` records a reported-supported pin/time. Never write canonical `verified`, `reviewed`, or `generated`. A newer revision is uncovered and invalidates any effective current-support signal. Legacy `verified` stamps are scheduling hints only; ignore malformed and future stamps. Absence of evidence never means current.

Use exact persisted UTF-8 revision bytes, not snapshot universal-newline text, for excerpt membership. Require nonempty excerpts, at most 2,048 UTF-8 bytes each. A comparison finding requires at least two distinct concept paths; all findings require at least one pin, and completed work requires its primary pin even if the individual finding concerns a neighbor. External observations supplement, never replace, catalog evidence. Bound combined local/external evidence entries per finding to 20. Reports never become evidence inputs.

## Task 1: Add maintenance contracts and schema

**Files:** Create `src/maintenance/{mod,types}.rs`, `migrations/0006_maintenance.sql`; modify `src/lib.rs`, `src/db.rs` tests.

**Interfaces:** Produces the shared DTOs above and `maintenance::types::new_id() -> Result<String, OkfError>`. Generate 128 random bits with existing `getrandom`, formatted as 32 lowercase hexadecimal characters; no bearer-key prefix, no new UUID dependency. The same generator creates run/work/finding IDs and lease tokens, but tokens are never included in list APIs or logs.

Schema contract:

- Add `api_keys.maintenance_report boolean NOT NULL DEFAULT false`.
- `maintenance_state`: PK/FK `catalog_id`; `changed_cursor`, `expiry_cursor`, `verification_cursor` text default empty; `next_aging_group` (`expiry`/`verification`), and server timestamp `updated_at`.
- `maintenance_runs`: text ID PK, catalog FK, nullable key FK `ON DELETE SET NULL`, immutable `credential_id` and actor snapshot, state, deadline/creation/end timestamps, end reason, primary count constrained 0..50.
- `maintenance_work`: text ID PK, run/catalog FKs, path/revision FK to published revisions, nonempty lane array, nullable assigned aging group (`expiry`/`verification`), nullable terminal outcome, nullable assessment, reason, summary, lease token/expiry, attempt/end timestamps, and optional reported tokens. Only rows with `outcome IS NULL` participate in a unique index on `(catalog_id,path,revision)`.
- `maintenance_findings`: text ID PK, run/work/catalog FKs, category/assertion/follow-up/uncertainty/relationship, external observations JSONB, creation timestamp.
- `maintenance_evidence`: catalog, owning work ID, nullable finding ID (null for assessment evidence), ordinal, path/revision, excerpt; FK to `revisions(catalog_id,path,revision)`. Unique per owner/ordinal; finding ownership must be enforced by compound FK, not just client validation.
- `maintenance_dispositions`: append-only rows with identity sequence PK, finding/catalog FK, disposition, rationale, immutable credential/actor snapshots and timestamp. A finding without rows is open.
- `maintenance_requests`: catalog, credential snapshot, operation, idempotency key composite PK; normalized request and response JSONB. These may contain a lease token; never expose via list APIs.
- Every maintenance row cascades on catalog deletion; use compound unique keys/FKs to prevent attaching a work, finding, run or evidence row from another catalog. Required timestamps have no database clock defaults.
- Index runs by `(catalog_id,created_at,id)`, work by `(catalog_id,path,revision)` and lease expiry, findings by `(catalog_id,created_at,id)`, dispositions by `(catalog_id,finding_id,id)`.

- [ ] **1.1 Write schema tests** in `src/db.rs`: migration from the five existing migrations preserves credentials, documents and history; existing key capability is false; rerunning migration is a no-op. Add a new `maintenance_schema_rejects_cross_catalog_and_duplicate_live_claims` test. Its assertion contracts are:

```rust
assert!(!existing_key_maintenance_report);
assert_eq!(documents_before, documents_after);
assert_eq!(revisions_before, revisions_after);
assert!(duplicate_active_claim_insert.is_err());
assert!(cross_catalog_evidence_insert.is_err());
assert!(second_attempt_after_terminal_insert.is_ok());
```

The before/after values are ordered full SQL rows, not only row counts. Use two synthetic catalogs and actual revision pins. Extend the existing exact table-list assertion for all seven new tables.

- [ ] **1.2 Verify RED:** `cargo test maintenance_schema -- --nocapture` and `cargo test key_name_migration -- --nocapture`; the new maintenance assertion must fail before adding the migration, while the existing migration regression remains green.
- [ ] **1.3 Implement** the SQL constraints and shared DTOs; register `pub mod maintenance` in `src/lib.rs`. Add `new_id_is_random_hex` asserting length 32, ASCII lowercase hexadecimal, and unequal successive IDs.
- [ ] **1.4 Verify GREEN:** `cargo test maintenance_schema -- --nocapture`, `cargo test migrations_create_the_schema -- --nocapture`, `cargo test new_id_is_random_hex`. Expect all selected tests pass.
- [ ] **1.5 Review/commit checkpoint:** schema and contracts only, message explaining preservation and isolation. Stage only this task's files if commits are authorized.

## Task 2: Add the reporting capability without widening access

**Files:** Modify `src/store/keys.rs`, `src/auth.rs`, `src/cli.rs`, `src/mcp/testing.rs`, `tests/support/server.rs`; update affected `NewKey`, `KeyRecord`, and `Principal` literals found by content search.

**Interfaces:** Add `maintenance_report: bool` to `NewKey`, `KeyRecord`, its private SQL row, and `Principal`. Produce:

```rust
// src/auth.rs; existing Principal::require remains unchanged.
pub fn require_maintenance_report(&self, catalog: &str) -> Result<(), OkfError>;
// src/store/keys.rs; use existing resolve_active and generation bookkeeping.
pub async fn set_maintenance_report_target(
    pool: &PgPool, target: &str, actor: Option<&str>, enabled: bool,
) -> Result<String, OkfError>;
```

The principal method first checks ordinary `Read` and catalog reach, then the flag. Neither Write nor Admin implicitly enables reporting. CLI `key create --maintenance-report` defaults false. Add `key maintenance-report ID --enabled true|false [--actor ACTOR]`; retain exact-ID/name/actor resolution rules. Append `maintenance_report true|false` to existing key-list output without reordering earlier columns. Test fixtures default false and opt in explicitly.

- [ ] **2.1 Write tests** `maintenance_capability_does_not_imply_write`, `maintenance_capability_change_refreshes_cache`, and `maintenance_capability_cli_resolves_actor_and_reissued_names`. Construct a read+report principal and a write-without-report principal, plus a second catalog; assert:

```rust
assert!(reporter.require_maintenance_report("pg").is_ok());
assert!(reporter.require(Access::Write, "pg").is_err());
assert!(reporter.require_maintenance_report("other").is_err());
assert!(writer_without_flag.require_maintenance_report("pg").is_err());
assert!(!principal_after_poll.maintenance_report);
assert_ne!(revoked_credential_id, replacement_credential_id);
```

Cache test: grant true, load key, set false, call the existing poll, then inspect new principal. CLI test toggles the intended actor only and checks preservation of earlier list columns. No-op setting must not bump `admin_generation`.

- [ ] **2.2 Verify RED:** `cargo test maintenance_capability -- --nocapture`; expect missing flag/method/parser coverage to fail before implementation.
- [ ] **2.3 Implement** the exact methods and CLI, propagate the flag through SQL selection and cache conversion, and update literal constructors with false unless the test requires a reporter. Do not add another ordinal access level.
- [ ] **2.4 Verify GREEN:** `cargo test maintenance_capability -- --nocapture`, `cargo test key_subcommands -- --nocapture`, `cargo test auth:: -- --nocapture`.
- [ ] **2.5 Review/commit checkpoint:** permissions and CLI as one independently testable change.

## Task 3: Implement pure, fair candidate selection

**Files:** Create `src/maintenance/select.rs`; modify `src/maintenance/mod.rs`, `src/config.rs`.

**Interfaces:** `select.rs` defines `Candidate { pin: RevisionPin, edited_at: DateTime<Utc>, stale_after: Option<DateTime<Utc>>, legacy_verified: Option<DateTime<Utc>>, changed_done: bool, last_attempt: Option<DateTime<Utc>>, last_aging_completed: Option<DateTime<Utc>>, reported_supported: Option<(i32, DateTime<Utc>)>, retry_at: Option<DateTime<Utc>>, claimed: bool }`, `Cursors { changed: String, expiry: String, verification: String, next_expiry: bool }`, `Selection { items: Vec<Selected>, cursors: Cursors }`, and `Selected { pin: RevisionPin, reasons: Vec<Lane>, aging_group: Option<AgingGroup> }`. Produce:

```rust
pub fn select(
    candidates: &[Candidate], cursors: &Cursors,
    now: DateTime<Utc>, review_interval: chrono::Duration, limit: usize,
) -> Result<Selection, OkfError>;
```

Config gains `maintenance_review_intervals: BTreeMap<String, std::time::Duration>` parsed with a small map deserializer that applies existing `humantime_serde` parsing to each value. Missing catalog defaults to 2,592,000 seconds. Validate names with `catalogs::is_valid_name`, and reject values <86,400 seconds. This is runtime configuration, not a change to concept metadata or catalog identity.

Algorithm decisions:

- Reject limit outside 1..=50. Filter claimed and cooldown (`now < retry_at`) candidates. Store supplies live concepts only; pure tests include expired/deprecated filtering in the store test task.
- Changed budget `limit/2`, aging budget `limit-limit/2`; round-robin changed paths after its persisted cursor, wrapping once.
- Aging eligible if explicitly expired, no usable verification/aging attempt exists, current revision is uncovered, or the last completed aging attempt is at least one review interval old. `last_aging_completed` covers only the candidate's current revision; honor that attempt's interval even when explicit expiry remains in the past. A new revision enters changed work and is uncovered for aging. Carry historical last-attempt rotation separately from revision-specific completion.
- Alternate expiry and verification groups, retaining `next_expiry` across claims. Sort eligible expiry work by `(last_attempt or epoch, path)`; verification work by `(last_attempt or epoch, legacy/current-reported verification or epoch, path)`. Ignore future/invalid legacy stamps; never use `generated`/`reviewed`. Persist a path cursor as the tie-break rotation among equal-ranked candidates, not a reason to skip higher-priority candidates indefinitely.
- Deduplicate pins across lanes but attach both eligible reasons. Backfill unused slots from the other lane until limit or candidates exhausted. A combined item counts once; completing it must satisfy both reasons. Retain its assigned aging group on the work row so completion can advance group rotation without guessing from the reasons.
- `Selection.cursors` is proposed state. Transaction code persists progress only with terminal completion, never merely on claiming. Re-read active claims before selecting another batch. Rotation plus active-claim exclusion prevents duplicate live assignment.

- [ ] **3.1 Write pure tests** `maintenance_selection_shares_lanes_and_wraps`, `maintenance_selection_retry_and_expiry_boundaries`, `maintenance_selection_ignores_edit_as_verification`, and config test `maintenance_review_interval_is_per_catalog`. Use a fixed `t0`, 12 changed-only paths and 12 aging-only paths; initial limit 10 must yield 5 each, limit 3 yields 1 changed and 2 aging. After recording completion and proposed cursors across finite rounds, every original aging path must appear despite newer edits to changed paths. Assertion contracts:

```rust
assert_eq!((changed_only_count, aging_only_count), (5, 5));
assert_eq!(selected_unique_pin_count, selected.items.len());
assert!(all_original_aging_paths_seen);
assert!(!eligible_at_t0_plus_86399);
assert!(eligible_at_t0_plus_86400);
assert_eq!(default_interval.num_seconds(), 2_592_000);
assert!(interval_of_86_399_seconds_is_rejected);
```

Also test one empty lane, both empty, overlap-only inputs, odd limits, exact stale timestamp, future/invalid stamps parsed by candidate loading, and last-attempt rotation with permanently expired content. A fresh `generated` stamp never removes an unverified candidate.

- [ ] **3.2 Verify RED:** `cargo test maintenance_selection -- --nocapture` and `cargo test maintenance_review_interval -- --nocapture`.
- [ ] **3.3 Implement** selector/config contracts without changing `okf::analysis::oldest`. Do not copy its touched-age logic into verification scheduling.
- [ ] **3.4 Verify GREEN:** repeat 3.2 and run `cargo test okf::analysis::oldest -- --nocapture` for compatibility.
- [ ] **3.5 Review/commit checkpoint:** pure selection and its configuration.

## Task 4: Add authoritative candidate loading and leased runs

**Files:** Create `src/store/maintenance/{mod,candidates,runs}.rs`; modify `src/store/mod.rs`.

**Interfaces:** Store functions take catalog database identity, never resolve by name after authorization. `candidates.rs` produces `pub async fn load(conn: &mut PgConnection, catalog_id: i64, now: DateTime<Utc>) -> Result<Vec<Candidate>, OkfError>` and `pub async fn cursors(conn: &mut PgConnection, catalog_id: i64) -> Result<Cursors, OkfError>`. Read published document rows and maintenance history in the same transaction. Parse concept frontmatter using `Doc::from_text(...).parse()`, timestamp helpers, and existing concept/machinery classification; invalid metadata cannot abort all other candidates. Use historical revision content where necessary to associate a legacy verification with actual source, never infer that current edits were verified.

`runs.rs` produces:

```rust
pub async fn claim(pool: &PgPool, catalog_id: i64, actor: &Actor,
    idempotency_key: &str, limit: usize, review_interval: chrono::Duration,
    now: DateTime<Utc>) -> Result<ClaimResult, OkfError>;
pub async fn renew(pool: &PgPool, catalog_id: i64, actor: &Actor,
    work_id: &str, token: &str, idempotency_key: &str,
    now: DateTime<Utc>) -> Result<Lease, OkfError>;
pub async fn close(pool: &PgPool, catalog_id: i64, actor: &Actor,
    run_id: &str, state: RunState, rationale: &str, may_close_others: bool,
    idempotency_key: &str, now: DateTime<Utc>) -> Result<RunSummary, OkfError>;
```

Define `RunSummary { id: String, state: RunState, actor: String, created_at: String, deadline: String, ended_at: Option<String>, reason: Option<String>, primary_count: u32, completed_count: u32, failed_count: u32, inconclusive_count: u32, superseded_count: u32 }`. It never contains tokens. Define shared private transaction helpers in `store/maintenance/mod.rs` for catalog lock/state initialization, request replay, deadline/lease recovery, and request insertion; signatures are local implementation details, not another public abstraction.

Lock order for every maintenance mutation: existing `changesets::lock_catalog` (`FOR KEY SHARE`), maintenance-state row `FOR UPDATE`, run, work, finding/disposition, request rows. Claim selection reads authoritative documents and rechecks their revisions under row `FOR SHARE` locks acquired in path order before inserting claims; rebuild selection if revisions changed. No network/model work while locked. Publication/import may wait briefly on those document locks; maintenance never writes the documents or bumps catalog generation. All maintenance operations in one catalog serialize only their short mutations on the state row.

Expire/release old work before selecting; preserve history and issue a new token for replacement attempts. Cancel/failure closes active work without advancing successful coverage. A source edited since the claimed revision is superseded when observed; mark old attempt terminal without marking the new revision processed. Existing SQL lock helpers map deadlocks to `Busy`; preserve that retry behavior.

- [ ] **4.1 Write store tests** `maintenance_claims_are_exclusive_and_recover`, `maintenance_claim_replay_does_not_extend_a_lease`, `maintenance_source_change_supersedes_only_old_revision`, and `maintenance_close_obeys_deadline_and_owner`. Use `db::test_db`, synthetic imported concepts, explicit `t0`, and `tokio::join!` competing claims. Assertions:

```rust
assert!(first_batch_pins.is_disjoint(&second_batch_pins));
assert_eq!(replayed_claim, original_claim);
assert_eq!(replayed_expiry, t0_plus_900_seconds);
assert_ne!(replacement.token, original.token);
assert!(old_token_renewal.is_err());
assert!(renew_at_exact_expiry.is_err());
assert!(new_revision_remains_eligible);
assert_eq!(deadline_expired_run.state, RunState::Failed);
assert_eq!(deadline_expired_run.reason.as_deref(), Some("budget_exhausted"));
```

Check exact 900/3,600-second boundaries, new key with reused name cannot renew, failure cooldown vs immediate crash reassignment, invalid limits, empty batches, catalogue deletion/recreation with same name, cancelled work, no-op cursor behavior, and rollback leaves no rows/request receipt. Read inputs from Postgres, not a stale `CatalogState` snapshot.

- [ ] **4.2 Verify RED:** `cargo test maintenance_claim -- --nocapture`, `cargo test maintenance_source_change -- --nocapture`, `cargo test maintenance_close -- --nocapture`.
- [ ] **4.3 Implement** loading, idempotency, claim/renew/close with the contracts and atomicity above. `may_close_others` comes only from MCP's ordinary Write authorization, never a user parameter.
- [ ] **4.4 Verify GREEN:** repeat 4.2; add candidate-loader tests excluding machinery/deprecated/tombstoned rows and tolerating invalid/future metadata.
- [ ] **4.5 Review/commit checkpoint:** independently testable storage workflow, no MCP surface yet.

## Task 5: Validate pinned evidence and untrusted report text

**Files:** Create `src/maintenance/validate.rs`, `src/store/maintenance/evidence.rs`; extend `src/store/maintenance/runs.rs` to validate close rationales with the new text validator; register modules.

**Interfaces:** `validate.rs` produces `pub fn submission(input: &Submission) -> Result<(), OkfError>` (shape, UTF-8-byte and secret limits) and `pub fn text(value: &str) -> Result<(), OkfError>` for free-text fields, including close/disposition rationales. Use `scan_secrets("maintenance/report.md", value)` and return masked `SecretDetected`; never log the rejected value.

`evidence.rs` produces `PinnedConcept { pin: RevisionPin, content: String, outdated: bool }` and:

```rust
pub async fn read(pool: &PgPool, catalog_id: i64,
    pin: &RevisionPin) -> Result<PinnedConcept, OkfError>;
pub async fn validate(conn: &mut PgConnection, catalog_id: i64,
    evidence: &[Evidence]) -> Result<bool, OkfError>;
```

Use `history::content_at` or equivalent transaction-scoped SQL for immutable bytes. Reject missing/deletion revisions, binary/invalid UTF-8, and nonconcept/machinery paths. `validate` returns whether any referenced revision is no longer current; exact excerpts must exist in cited bytes even when outdated. External resources are attribution strings, not fetch targets. Require literal `http://` or `https://`, a nonempty authority with no `@` or percent escapes, and no whitespace/control characters anywhere. These deliberately narrow lexical checks reject plain or encoded userinfo without adding a URL-fetching dependency. They do not certify public routability or reachability: perform no DNS lookup or fetch. Future runner network policy remains slice 2.

Enforce serialized DTO length ≤262,144 bytes, each finding count ≤20, combined finding evidence count ≤20, and excerpts ≤2,048 bytes. Also cap assessment evidence at 20 entries and 2,048 bytes each (same evidence policy). Empty evidence/excerpts and invalid assessment/outcome pairings are rejected according to shared contracts. Scan all user text, URI strings, excerpts, summaries, uncertainty, and external attribution before persistence; enforce 4,096-byte maximum on standalone rationale and 256-byte maximum on path. Pure validation does not accept caller-supplied timestamps or actor identity.

- [ ] **5.1 Write tests** `maintenance_evidence_uses_exact_revision_bytes`, `maintenance_submission_limits_and_masking`, `maintenance_external_sources_are_attributed_not_fetched`. Fixtures contain `é`, CRLF, two revisions, a deleted path, a machine instruction in body prose, and a secret assembled at runtime as adjacent tests do. Assertions:

```rust
assert!(exact_crlf_excerpt_matches);
assert!(!lf_only_excerpt_matches_crlf_source);
assert!(two_kib_utf8_excerpt_is_accepted);
assert!(two_kib_plus_one_byte_excerpt_is_rejected);
assert!(twenty_findings_are_accepted);
assert!(twenty_one_findings_are_rejected);
assert!(unsupported_current_without_primary_evidence_is_rejected);
assert!(secret_error_is_masked);
assert_eq!(source_bytes_after_validation, source_bytes_before_validation);
```

Test 20/21 evidence entries, 262,144/262,145 serialized bytes, excerpts from another revision, duplicate-path comparison pins, plain/percent-encoded URI userinfo, malformed URI schemes, and unknown nested catalog fields. Source prose remains a string; no instruction is acted on. A syntactically allowed but unavailable public URI is retained as unverified attribution, not fetched.

- [ ] **5.2 Verify RED:** `cargo test maintenance_evidence -- --nocapture`, `cargo test maintenance_submission -- --nocapture`, `cargo test maintenance_external -- --nocapture`.
- [ ] **5.3 Implement** validators using exact bytes and existing secret/timestamp/path helpers. Do not expose a capability to read arbitrary archive machinery through the new pin interface.
- [ ] **5.4 Verify GREEN:** repeat 5.2; all boundary cases pass without making any network request.
- [ ] **5.5 Review/commit checkpoint:** evidence safety, independent of MCP routing.

## Task 6: Submit findings atomically and maintain scheduling history

**Files:** Create `src/store/maintenance/reports.rs`; extend `runs.rs`, `candidates.rs` and module registration.

**Interfaces:** Produce:

```rust
pub async fn submit(pool: &PgPool, catalog_id: i64, actor: &Actor,
    work_id: &str, token: &str, idempotency_key: &str,
    input: &Submission, now: DateTime<Utc>) -> Result<Receipt, OkfError>;
```

Validate text before a transaction, then authorize ownership and replay, lock state/run/work, check deadline/lease, and validate revision evidence within the transaction. Insert findings/evidence, terminalize the work, update proposed selection cursors from current persisted state (not an old batch snapshot), update counters/aging history, and save receipt atomically. Zero findings is allowed: a successful investigation can find no issues but still needs assessment evidence. Terminal outcomes `Superseded` are server-assigned, not accepted as a normal caller outcome.

If a valid lease still exists but the primary revision changed, preserve the submission as a historical, outdated report and return `Superseded`; never cover the new revision or renew current-support metadata. If a later claim already superseded/replaced the old lease, reject a fresh submission using that token. Previously recorded idempotent results still replay after authorization. Non-primary evidence changing marks the report outdated; do not treat a supported-current assessment depending on outdated evidence as effective current support.

Record failed/inconclusive timestamps and retry eligibility separately from last successful aging completion and reported-supported pin. No canonical metadata changes, catalog generation bumps, or snapshot rebuilds.

- [ ] **6.1 Write tests** `maintenance_submit_is_atomic_and_idempotent`, `maintenance_inconclusive_never_refreshes_support`, `maintenance_historical_report_does_not_cover_new_revision`. Submit a supported report, retry identical and conflicting payloads, fail evidence validation after the first proposed finding, then inspect all tables. Assertions:

```rust
assert_eq!(first_receipt, retried_receipt);
assert_eq!(stored_findings_after_retry, stored_findings_after_first);
assert!(conflicting_retry.is_err());
assert_eq!(findings_after_rejected_submission, findings_before);
assert_eq!(support_after_inconclusive, support_before);
assert_eq!(inconclusive_retry_at, attempt_time_plus_86400_seconds);
assert!(historical_receipt.outdated);
assert_eq!(historical_receipt.outcome, WorkOutcome::Superseded);
assert!(new_revision_changed_lane_eligible);
```

Test completed/assessment pairings, zero findings with evidence, closed runs, old tokens, another credential, deadline expiry, last-item automatic run completion, and double completion. Outdated data findings never certify current support.

- [ ] **6.2 Verify RED:** `cargo test maintenance_submit -- --nocapture`, `cargo test maintenance_inconclusive -- --nocapture`, `cargo test maintenance_historical -- --nocapture`.
- [ ] **6.3 Implement** atomic submission and loader history aggregation. Return existing `Refused` for invalid state/input, `Busy` for a stale lease/state race, and `NotFound` for absent/inaccessible record IDs; do not add secret-bearing diagnostic strings.
- [ ] **6.4 Verify GREEN:** repeat 6.2 and Task 4 lifecycle tests.
- [ ] **6.5 Review/commit checkpoint:** durable reports plus correct freshness bookkeeping.

## Task 7: Retrieve reports and record dispositions without applying them

**Files:** Extend `src/store/maintenance/{runs,reports}.rs`.

**Interfaces:** Define `Page<T> { items: Vec<T>, next_after: Option<String> }`, `FindingRecord { id: String, run_id: String, work_id: String, input: FindingInput, actor: String, created_at: String, disposition: Disposition, dispositions: Vec<DispositionRecord>, outdated: bool }`, and `DispositionRecord { id: i64, disposition: Disposition, rationale: String, actor: String, at: String }`. Define `RunReport { run: RunSummary, work: Vec<WorkReport> }`, `WorkReport { id: String, pin: RevisionPin, reasons: Vec<Lane>, outcome: Option<WorkOutcome>, assessment: Option<Assessment>, summary: Option<String>, evidence: Vec<Evidence>, finding_ids: Vec<String> }`. None expose tokens. Produce:

```rust
pub async fn list_runs(pool: &PgPool, catalog_id: i64,
    after: Option<&str>, limit: u32) -> Result<Page<RunSummary>, OkfError>;
pub async fn get_run(pool: &PgPool, catalog_id: i64,
    run_id: &str) -> Result<RunReport, OkfError>;
pub async fn list_findings(pool: &PgPool, catalog_id: i64,
    after: Option<&str>, limit: u32) -> Result<Page<FindingRecord>, OkfError>;
pub async fn get_finding(pool: &PgPool, catalog_id: i64,
    finding_id: &str) -> Result<FindingRecord, OkfError>;
pub async fn dispose(pool: &PgPool, catalog_id: i64, actor: &Actor,
    finding_id: &str, disposition: Disposition, rationale: &str,
    idempotency_key: &str, now: DateTime<Utc>) -> Result<FindingRecord, OkfError>;
```

Lists sort ascending by `(created_at,id)` and use a catalog-scoped existing row ID as cursor; reject an absent/foreign cursor as NotFound. Default page 20, accepted 1..=200. Work is bounded to 50 per run; findings and evidence use pages and the fixed submission limits. `get_run` plus paged `list_findings` provide JSON export; do not add a tarball route. Compute outdated at read time against current database rows. Do not store a boolean that becomes permanently false after source edits.

- [ ] **7.1 Write tests** `maintenance_disposition_preserves_history_and_content`, `maintenance_report_outdated_tracks_all_evidence`, `maintenance_report_pages_are_catalog_scoped`. Accept, dismiss, and reopen a finding, replay disposition, edit/delete/recreate an evidence path, and use a cursor from another catalog. Assertions:

```rust
assert_eq!(history.len(), 3);
assert_eq!(current_disposition, Disposition::Open);
assert_eq!(documents_before, documents_after_dispositions);
assert_eq!(revisions_before, revisions_after_dispositions);
assert!(outdated_after_neighbor_edit);
assert!(outdated_after_primary_delete_and_recreate);
assert!(foreign_catalog_cursor.is_err());
assert!(!json_export_contains_lease_token);
```

Test page boundaries, identical timestamps, omitted/invalid limits and cursor, failed/inconclusive reports without findings, catalog deletion cascade, and a completed historical report remaining readable after source changes.

- [ ] **7.2 Verify RED:** `cargo test maintenance_disposition -- --nocapture`, `cargo test maintenance_report -- --nocapture`.
- [ ] **7.3 Implement** the exact query/disposition interfaces, scanning rationale with Task 5 validation. Dispositions are append-only; acceptance creates no changeset or document mutation.
- [ ] **7.4 Verify GREEN:** repeat 7.2 and inspect JSON results for absence of tokens.
- [ ] **7.5 Review/commit checkpoint:** human review and portable report retrieval.

## Task 8: Expose bounded, authorized MCP tools

**Files:** Create `src/mcp/maintenance/{mod,work,reports}.rs`, `tests/maintenance.rs`; modify `src/mcp/mod.rs`, test helpers only as needed.

**Interfaces:** Follow sibling `#[tool_router]`, `Parameters`, `Json`, `ToolContext`, `reply` patterns. Produce `maintenance_router()` and register it in `OkfServer::tools()`. Tool parameters below are exact new contracts; `?` denotes optional, not Rust syntax. All require `catalog: String`, no `changeset` input; every response repeats the authorized catalog. IDs/tokens are strings. Actors, timestamps, ownership overrides, and limits the server fixes are not caller inputs.

| Tool | Other parameters | Permission | Result |
|---|---|---|---|
| `maintenance_claim` | `idempotency_key`, `batch_size?: u32` | Read + reporting flag | ClaimResult |
| `maintenance_renew` | `work_id`, `token`, `idempotency_key` | Read + reporting flag, owner | Lease |
| `maintenance_read_revision` | `path`, `revision: i32` | Read | PinnedConcept |
| `maintenance_submit` | `work_id`, `token`, `idempotency_key`, `submission: Submission` | Read + reporting flag, owner | Receipt |
| `maintenance_close` | `run_id`, `state: RunState`, `rationale`, `idempotency_key` | Owner with reporting flag; or Write for another owner | RunSummary |
| `maintenance_list_runs` | `after?: String`, `limit?: u32` | Read | Page<RunSummary> |
| `maintenance_get_run` | `run_id` | Read | RunReport |
| `maintenance_list_findings` | `after?: String`, `limit?: u32` | Read | Page<FindingRecord> |
| `maintenance_get_finding` | `finding_id` | Read | FindingRecord |
| `maintenance_dispose` | `finding_id`, `disposition`, `rationale`, `idempotency_key` | Write | FindingRecord |

Resolve explicit catalog using `ctx.one(Some(&params.catalog))`, then check permission before any ID lookup or replay. Use the resulting database ID. Fetch authority from the current request principal; do not cache it in a maintenance run. Unknown and inaccessible record IDs use the same NotFound shape. Existing catalog selection errors remain consistent with sibling tools. Tool descriptions must say report-only and distinguish reported support from verified truth.

Define each tool's `pub(crate) async fn` handler as `(ctx: &ToolContext, params: <ToolName>Params) -> Result<<ResultType>, OkfError>` in its work/reports module. Public parameter structs are `<ToolName>Params` in PascalCase (for example `MaintenanceClaimParams`); result wrappers add `catalog: String` around the store DTO. No handler directly performs canonical store writes.

- [ ] **8.1 Write real HTTP tests** using existing `TestServer::{start,key,client}` and `call/structured`. Add a helper `TestServer::reporter_key(name: &str, catalogs: &[&str]) -> String` that creates ordinary Read, toggles the flag through Task 2's function, and calls `app.poll()`; existing `key()` retains its signature/default false. Tests: `maintenance_reporter_is_read_only`, `maintenance_http_lease_uses_credential_identity`, `maintenance_http_rejects_cross_catalog_and_unknown_fields`, `maintenance_http_preserves_canonical_export`. In assertions use JSON tool calls, including:

```rust
assert!(claim_result.is_error.is_none_or(|v| !v));
assert_eq!(claim_json["catalog"], "pg");
assert!(canonical_edit_result.is_error.unwrap_or(false));
assert!(dispose_as_reporter_result.is_error.unwrap_or(false));
assert!(foreign_evidence_request.is_error.unwrap_or(false));
assert_eq!(canonical_export_before, canonical_export_after);
assert_eq!(document_and_revision_rows_before, document_and_revision_rows_after);
```

Before export comparison, include successes, failures, cancellation, renewal, retries, dispositions by a writer, instruction-like text, and invalid secret submissions. Compare unpacked ordered archive entries/bytes/mode, not gzip headers. Compare complete ordered document/revision SQL rows too. Existing search, `oldest`, link report and history results must match before/after maintenance. Store-only metadata is absent from these outputs.

- [ ] **8.2 Verify RED:** `cargo test --test maintenance -- --nocapture`; new tools should be absent before implementation.
- [ ] **8.3 Implement** parameter validation, wrappers and router. Tests remove capability/revoke a key, poll, and prove existing clients cannot renew or submit afterward; replacement credentials with the same name cannot inherit leases. Add nested unknown catalog/timestamp/actor field rejections and inaccessible run/cursor ID cases. Keep the existing poll-interval revocation contract; do not promise instantaneous revocation.
- [ ] **8.4 Verify GREEN:** `cargo test --test maintenance -- --nocapture`, `cargo test --test server -- --nocapture`, `cargo test --test writes -- --nocapture`. Expect no authorization or legacy tool regressions.
- [ ] **8.5 Review/commit checkpoint:** complete MCP workflow behind explicit reporting permission.

## Task 9: Document the workflow and run release gates

**Files:** Modify `README.md`, `docs/operations.md`; add synthetic acceptance cases to `tests/maintenance.rs` where not already covered. No deployment manifests or CI changes.

**Interfaces:** Document the ten tools, JSON report pagination, CLI capability enable/disable, 10/50 batch boundaries, 900-second leases, 3,600-second deadline, 86,400-second retry, and `maintenance_review_intervals = { pg = "30d" }`. Explain that ordinary canonical exports omit reports while database backups include them, catalog deletion removes them under existing semantics, and the scanner is heuristic rather than a proof that text contains no secret.

- [ ] **9.1 Add documentation examples** using synthetic catalogs/actors and placeholder environment references, never private production details. Show `claim → read pinned revision → submit → get/list → writer disposition`. Explicitly say no scheduler/model service exists yet, external URLs are not fetched, and acceptance does not apply changes.
- [ ] **9.2 Check acceptance coverage** against this map. Every row must refer to passing named tests, not a prose claim:

| Spec acceptance | Owning task |
|---|---|
| Selection coverage/fairness and expiry | 3, 4 |
| Attempts distinct from verification | 3, 6 |
| Existing tools/history/export compatibility | 8 |
| Exclusive claims/recovery/idempotency | 4, 6 |
| Source changes/deletion during investigation | 4, 6, 7 |
| Credential/catalog isolation | 2, 7, 8 |
| Shape/size/secret rejection | 5, 8 |
| Canonical byte preservation on all outcomes | 1, 7, 8 |
| Reports outside search and bundle export | 8 |

The semantic fixture evaluation described in the design is a gate for slice 2, not evidence slice 1 can produce without an investigator. Do not add a model or invent quality scores here.

- [ ] **9.3 Run gates** from the workspace root, after checking Docker is available:

```sh
just fmt
just lint
just test
git diff --check
```

Expected: format check passes; clippy has no warnings; rustdoc with `RUSTDOCFLAGS="-D warnings"` succeeds; all tests pass including disposable-Postgres tests; no whitespace errors. Report actual failures. Do not clear wrappers, kill unrelated processes, change CI, or skip Docker tests to manufacture a green result.

- [ ] **9.4 Final self-review:** verify all public parameters have schemas/doc comments, all store writers use the injected clock and lock order, no model/network dependency was added, no report operation writes canonical tables or `catalogs.generation`, and every test above exists and is exercised. Review changes on a fresh capable model in the separately selected execution workflow.
- [ ] **9.5 Review/commit checkpoint:** documentation and acceptance tests; do not release, deploy, push or open issues/PRs without current authorization.

## Execution handoff

The plan implements slice 1 only. Read the design and this plan together; select an execution method after human plan review. Prefer subagent-driven execution for the independent permission, lease, and evidence review gates: mistakes could expose private material or misrepresent verification even though canonical text is not rewritten. Native execution is cheaper but has no independent task review before the end.
