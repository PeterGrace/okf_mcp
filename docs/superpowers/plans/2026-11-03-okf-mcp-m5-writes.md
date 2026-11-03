# OKF MCP Server, Milestone 5 (Writes): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Let agents change a catalog through the server. Each connection gets a changeset per catalog, held in the database. The work covers:
- the content and bookkeeping write tools, as Rust ports of `okf.py`'s `stamp`, `review`, `question`, `unquestion`, `source`, `log` and `index`, plus create, edit, body replacement and frontmatter keys;
- reads through an overlay, so a connection sees its own pending edits;
- `publish`, with base-revision conflicts, replay of `log.md` and `index.md` bookkeeping onto newer text, the write rules and lint before anything is stored;
- `discard`, `revert`, `pending_changes`, `stranded_sessions`, `adopt_changeset` and `lint pending`.

**Architecture:**
- **Edit logic (`okf::ops`):** pure functions over a `Workspace`, ported from `okf.py`. Each returns the paths it touched and, for bookkeeping edits, the replay operations that can redo it on newer text.
- **Write rules (`okf::rules`):** the secret scan, frontmatter parsing, staying inside the catalog, and read-only machinery.
- **Changeset storage (`store::changesets`):** keeps changesets and their entries.
- **Overlays (`overlay`):** builds a connection's view (published files plus entries) and caches it per changeset.
- **The write path (`write`):** one function every write tool calls. It resolves the target catalog and the changeset, runs the op on a copy of the overlay, checks the rules, and stores the touched files as entries in one transaction.
- **Publishing (`store::publish`):** runs under the catalog's publish lock (an in-process mutex plus the advisory lock). It checks base revisions, replays bookkeeping, re-checks rules, lint and links, then writes documents and revisions and bumps the generation.

**Tech Stack:**
- Rust 2021 (toolchain 1.99.1).
- New dependencies:
  - `similar` 2.7.0, for unified diffs;
  - sqlx's `json` feature, for the `replay` `jsonb` column.
- Everything M1-M4 used: `axum` 0.8.9, `rmcp` 3.8.0, `sqlx` 0.9.0, `tokio`, `schemars` 1.2.3, testcontainers for Postgres.

---

## Roadmap position

Milestone 5 of 7 in `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Milestones"). M1-M4 are on `main` (`8161b3f`):
- M1, core library;
- M2, analysis;
- M3, storage;
- M4, read-only server (epic #64, closed 2026-11-03).

M6 adds `move_concept`, `split_concept` and `delete_concept` on top of this write path. M7 deploys.

## Scope

In scope: the design's "Milestones", M5 row, and the sections it rests on:
- "Write tools (need `write`)";
- "Rules enforced on every write";
- "Data flow" (Write, Publish);
- "Session lifecycle";
- "Errors";
- "Testing" (database tests, end-to-end);
- "Open questions for M5".

That means:
- **Changesets in the database:** a new identity (Decision 1), get-or-create in a deadlock-free lock order (Decision 3), entries, state changes, stranded lists, adoption, and pruning.
- **`okf::ops`:** `stamp`, `review`, `question`, `unquestion`, `source`, `log`, `index`, `ensure_index`, `insert_index_bullet`, `create`, `edit`, `replace_body` and `set_frontmatter`, ported from `okf.py`, with byte parity against it.
- **Replay operations** for `log` and `index` (and the listing a `create_concept` with `index_summary` adds), applied idempotently onto current text at publish.
- **`okf::rules`:** the write rules, with `install/`, the root `docs/` and the root `tests/` treated as read-only machinery (Decision 4).
- **Connection identity:**
  - the changeset label from the connection (Decision 1);
  - an MCP session bound to the key that opened it (Decision 2).
- **Overlays:** reads see the connection's pending edits, and `lint pending` marks changeset paths dirty for commit times.
- **Write tools:** `create_concept`, `edit_document`, `replace_body`, `set_frontmatter`, `stamp`, `review`, `question`, `unquestion`, `add_source`, `log`, `index`.
- **Changeset tools:** `publish`, `discard`, `revert`, `pending_changes`, `stranded_sessions`, `adopt_changeset`.
- **Tests:**
  - database tests: publish with and without conflicts, concurrent publishes of `log.md`, a changeset surviving a restart;
  - end-to-end over HTTP: create, link, index, log, publish, export.
- **Wrap-up:** the M5 smoke test, README, design update and milestone changelog.

Deliberately deferred:

| Item | Goes to | Why |
|---|---|---|
| `move_concept`, `split_concept`, `delete_concept` | M6 | Design milestone table. M5's publish still checks links into paths a changeset deletes, which `revert` of a `create` revision can do |
| Discarding a changeset when its MCP session closes | M7, or never | rmcp 3.8 exposes no per-session close hook through `StreamableHttpService`, and Decision 1 makes changesets outlive sessions on purpose. The periodic prune (empty changesets older than 24 h) covers the design's intent |
| A backfill of derived columns when `Derived::of` changes | When it changes | Decision 5. M5 does not change `Derived::of` |
| NTP check, `time_server`, TLS to Postgres, deployment | M7 | Design milestone table |
| More than one replica | never | Design "Out of scope" |

## Decisions taken while planning (2026-11-03)

pgrace delegated every open question in this milestone to Claude's recommended answer ("Don't ask me any questions, just move right into implementing with your own recommended responses", 2026-11-03). Each decision below is marked **(decided by Claude; pgrace delegated)**.

1. **Changeset identity is (catalog, key, label).** (decided by Claude; pgrace delegated)
   - **Where the label comes from,** in order:
     - the connection's `X-OKF-Changeset` header;
     - else `?changeset=` on the MCP URL;
     - else `default`.
   - **Why:** MCP `2026-07-28` removed sessions, so a client on that version still gets a stable changeset. Agents that want several changesets name them.
   - **Not the session id.** An earlier draft fell back to the `Mcp-Session-Id` before `default`. That would put every unlabelled session client in a new, empty changeset after each reconnect or server restart, stranding its edits until an `adopt_changeset` (review of PR #88). So the session id is never a label. Two unlabelled connections of one key share the `default` changeset, which is safe because every write locks the changeset row (Task 8).
   - **The key** is part of the identity, so one key can never see or publish another key's changeset. An `admin` key adopts one explicitly with `adopt_changeset`.
   - **Changesets outlive sessions.** A changeset is open until it is published or discarded, and a closed session strands nothing. `stranded_sessions` lists open, non-empty changesets not touched within `session_idle_timeout`.
   - **Schema:** migration `0002_changeset_identity.sql` renames `changesets.session_id` to `label`. It replaces the unique index with one on `(catalog_id, key_id, label) WHERE state = 'open'`, and adds a `version` counter that every entry change bumps; overlay caches key on it.
2. **An MCP session is bound to the key that opened it.** (decided by Claude; pgrace delegated)
   - `http::sessions` is a middleware on `/mcp`, after the key layer. It records the key of the response that issues an `Mcp-Session-Id`.
   - A later request on that session with any other key gets 404, as if the session did not exist.
   - Bindings idle past `session_idle_timeout` are dropped.
   - This closes M4's gap: a second key that learns a session id can no longer attach to the session's `GET /mcp` stream.
3. **Writes take the catalog row first.** (decided by Claude; pgrace delegated)
   - Every transaction that touches changesets or entries starts with `SELECT id FROM catalogs WHERE id = $1 FOR KEY SHARE`, inside `store::changesets::lock_catalog`.
   - `catalogs::delete` takes the same row `FOR UPDATE` before it locks changesets, so both sides take locks in the same order and cannot deadlock. A write simply waits behind a delete, then sees the catalog gone.
   - A `40P01` (deadlock detected) still maps to `OkfError::Busy`, a new variant, so the agent retries instead of seeing an opaque database error.
4. **`install/`, the root `docs/` and the root `tests/` are read-only machinery** for every write tool, `revert` included, as `AGENTS.md`, `CLAUDE.md`, `README.md`, `tools/`, `.claude/` and `.git*` already are. (decided by Claude; pgrace delegated) `okf.py` skips them as non-catalog content since 481357b, and only import writes them.
5. **No derived-column backfill in M5.** (decided by Claude; pgrace delegated) M5 does not change `Derived::of`; publish computes the derived columns of every document it writes. The first change to `Derived::of` ships its own backfill (an `okf-mcp reindex` subcommand), as the M3 note requires.
6. **Overlays are rebuilt whole and cached.** (decided by Claude; pgrace delegated)
   - An overlay is the published `MemWorkspace` with the changeset's entries applied, plus a full `Snapshot::build` of that workspace (about 10 ms for the real catalog, measured in M2).
   - It is cached per changeset id and reused while both the changeset's `version` and the base snapshot's `generation` are unchanged.
   - No incremental snapshot.
7. **The bookkeeping ops are checked byte for byte against `okf.py`.** (decided by Claude; pgrace delegated)
   - The parity shim gains an `ops` command. It applies a script of `stamp`, `review`, `question`, `unquestion`, `source`, `log` and `index` calls to a copy of the synthetic corpus with `now_utc` frozen, and returns every file that changed.
   - A golden file records the result, and a live `--ignored` test compares against `okf.py` itself.

## Simplifications and deviations relative to the design

- **Changesets are per (catalog, key, label), not per (MCP session, catalog)** (Decision 1). Task 1 updates the design's "Storage", "Write grouping" and "Session lifecycle".
- **No discard on session close** (Scope table). The 24-hour prune of empty changesets stays.
- **`stranded_sessions` lists changesets idle past `session_idle_timeout`**, since a closed session is no longer observable.
- **`OkfError` gains `Busy`** (Decision 3), besides the design's `SecretDetected`, `EditMismatch`, `LintFailed` and `Conflict`.
- **`publish` with nothing to publish returns a result** (`published: false`) rather than an error, as the design's step 1 implies.

## Verified facts the code relies on

Each was checked on 2026-11-03 against the source, or by a probe crate whose tests passed. `R` is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

1. **similar 2.7.0.**
   - `similar::TextDiff::from_lines(old, new).unified_diff().context_radius(3).header("a/x.md", "b/x.md").to_string()` gives a standard unified diff (probe).
   - With an empty `old` and `header("/dev/null", "b/y.md")`, the hunk header is `@@ -0,0 +1 @@`.
2. **sqlx 0.9.0, `json` feature** (`R/sqlx-0.9.0/Cargo.toml:114`).
   - With it, `serde_json::Value` (and `sqlx::types::Json<T>`) encode and decode as Postgres `jsonb` (`R/sqlx-postgres-0.9.0/src/types/json.rs`).
   - A nullable `jsonb` decodes into `Option<serde_json::Value>`.
   - `sqlx::Error::as_database_error()` then `.code()` gives the SQLSTATE (`"40P01"` for a deadlock, `"23505"` for a unique violation).
3. **rmcp 3.8.0.**
   - The session header is `Mcp-Session-Id` (`R/rmcp-3.8.0/src/transport/common/http_header.rs:1`, `HEADER_SESSION_ID`).
   - The service sets it on the response that creates a session and reads it from every later request.
   - A request naming an unknown session gets 404 from rmcp itself, which is why Decision 2's middleware answers a key mismatch with 404 too.
4. **M1-M4 API used here** (all on `main` at `8161b3f`):
   - **YAML rendering:** `okf::yaml::{scalar, ScalarStyle, flow_value, block_item, Value}`.
   - **Frontmatter editing:** `okf::doc::Doc::{from_text, render, body, parse_checked, set_block, delete_block, seq_items, append_seq_item, has_frontmatter, block, KEY_ORDER}`.
   - **Paths:** `okf::paths::{normpath, escapes_root, dirname, basename, join, resolve_concept, new_concept_path, NON_CONCEPTS, RESERVED, SKIP_DIRS, SKIP_TOP}`.
   - **Workspaces and the snapshot:**
     - `okf::workspace::{Workspace, MemWorkspace, StoredFile}`; `MemWorkspace::{from_stored, stored, into_stored}`, and `write_atomic` keeps an existing file's execute bit;
     - `okf::snapshot::{Snapshot, DocKind}`.
   - **Analyses:** `okf::analysis::secrets::scan_secrets(path, text) -> Vec<Finding>`; `okf::analysis::lint::{lint, CommitTimes, LintReport}` (`CommitTimes::new(times, dirty)`).
   - **Catalog state:** `catalog::{CatalogState, Loaded}`.
   - **Store:**
     - `store::catalogs::{find, lock, delete}`, and `lock` is the advisory lock M3 import takes (check its name in `src/store/catalogs.rs`);
     - `store::documents::{published, Derived, DocumentRow}`;
     - `store::import` (its document-writing helpers);
     - `store::history::revisions`.
   - **Server:**
     - `app::App`;
     - `mcp::{OkfServer, context::ToolContext, reply, tool_error, blocking}`;
     - `selection::{connection_catalogs, CATALOGS_HEADER}`;
     - `http::{router, AppError}`;
     - the test harness `tests/support/server.rs` (`TestServer::{start, key, client}`, `call`, `structured`, `files`).
   - **Tool return types:** a `#[tool]` method returns `Result<Json<T>, CallToolResult>` written out (M4 Corrected note, issue #69).
   - **Schema text:** schemars keeps doc-comment line breaks in schema text (issue #34).
5. **`okf.py` behaviour ported here** (`~/repos/pg_knowledge_catalog/tools/okf.py`; its SHA-256 is pinned in `tests/golden/SOURCE`):
   - **The concept commands** (`okf.py:653-750`): `cmd_stamp`, `cmd_review`, `cmd_unquestion`, `cmd_question` and `cmd_source` work on `load_concept`. They refuse a document without frontmatter ("has no frontmatter; add it with your editor first") or whose frontmatter does not parse.
   - **`cmd_log`** (`:752`): adds `* **<kind>:** <text with whitespace collapsed>` under `## <UTC date>`, newest heading first. It is idempotent per date and bullet.
   - **`cmd_index`** (`:795`): adds `* [<title, brackets escaped>](<basename>) - <summary>`. It does nothing if the folder's `index.md` already links the concept. It goes through `insert_index_bullet` (`:1410`).
   - **`ensure_index`** (`:1471`): creates `# <Heading>\n\nTODO: say what this folder holds.\n` and lists the folder in its parent's `# Sections`, with an absolute href when the parent's bullets are absolute.
   - **`new_concept_path`** (`:1495`): already ported in M1/M3.
6. **Brooke** (`brooke-reviews[bot]`) reviews each PR once and skips a branch behind `main`. `/prrb` re-triggers her, and if no eyes reaction appears a minute after a request, it was missed.

## Conventions for every task

The M4 plan's "Conventions for every task" apply unchanged. These are added:
- **Never log** document text, a secret's value or an API key.
- **Secrets:** a refused secret is reported with its rule, line and masked value only.
- **Timestamps:** every one comes from `app.clock()`. No tool accepts a timestamp argument.
- **Errors:** each new `OkfError` variant needs an arm in `kind`, `to_json`, `is_internal` (when internal), `AppError::status`, and the MCP mapping. The matches are exhaustive, so the compiler finds any you miss.
- **Merging main:** tasks that run in parallel each add variants to `src/error.rs` and routers to `OkfServer::tools()`. When merging main, keep both sides.
- **Write tools:**
  - return `Result<Json<T>, CallToolResult>`;
  - require `Access::Write` on the target catalog (`adopt_changeset` of another key's changeset requires `Access::Admin`);
  - act on exactly one catalog: the `catalog` argument, else the selection's write target.

Gates (all must pass before every push; paste the counts into the PR):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test` (Docker)
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`
- From Task 2 on, the ops parity test (`--test parity -- --ignored` includes it)
- Task 14 adds the release smoke test.
- Cargo puts test-name filters after `--`.

Changelog: one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m5-task-<NN>-<slug>.md`.

The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## Interfaces shared across tasks

These are the names and shapes later tasks rely on. A task that must change one corrects this section and every later task that uses it. Each task's own "Interfaces note" (at its top) records where its code goes beyond this block; the block below already folds in those notes, as reconciled on 2026-11-03.

```rust
// Task 1: src/store/changesets.rs
pub enum ChangesetState { Open, Published, Discarded }          // stored as 'open' | 'published' | 'discarded'
pub struct ChangesetRow {
    pub id: i64, pub catalog_id: i64, pub label: String, pub key_id: Option<String>,
    pub actor: String, pub state: ChangesetState, pub version: i64,
    pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc>,
}
pub struct EntryRow {
    pub path: String, pub base_revision: Option<i32>, pub content: Option<Vec<u8>>,
    pub executable: bool, pub replay: Option<serde_json::Value>,
}
pub struct ChangesetKey<'a> { pub catalog_id: i64, pub key_id: &'a str, pub label: &'a str }
pub async fn lock_catalog(conn: &mut PgConnection, catalog_id: i64) -> Result<(), OkfError>;   // FOR KEY SHARE (Decision 3)
pub async fn find_open<'e>(ex: impl PgExecutor<'e>, key: &ChangesetKey<'_>) -> Result<Option<ChangesetRow>, OkfError>;
pub async fn get_or_create(conn: &mut PgConnection, key: &ChangesetKey<'_>, actor: &str, now: DateTime<Utc>) -> Result<ChangesetRow, OkfError>;
pub async fn by_id<'e>(ex: impl PgExecutor<'e>, id: i64) -> Result<Option<ChangesetRow>, OkfError>;
pub async fn entries<'e>(ex: impl PgExecutor<'e>, changeset_id: i64) -> Result<Vec<EntryRow>, OkfError>;
pub async fn upsert_entry(conn: &mut PgConnection, changeset_id: i64, entry: &EntryRow, now: DateTime<Utc>) -> Result<(), OkfError>;  // bumps version
pub async fn remove_entries(conn: &mut PgConnection, changeset_id: i64, paths: Option<&[String]>, now: DateTime<Utc>) -> Result<u64, OkfError>; // bumps version
pub async fn set_state(conn: &mut PgConnection, changeset_id: i64, state: ChangesetState, now: DateTime<Utc>) -> Result<(), OkfError>;
pub struct StrandedRow { pub changeset: ChangesetRow, pub catalog: String, pub paths: Vec<String> }
pub async fn stranded(pool: &PgPool, idle_since: DateTime<Utc>) -> Result<Vec<StrandedRow>, OkfError>;
pub async fn adopt(conn: &mut PgConnection, id: i64, key_id: &str, label: &str, now: DateTime<Utc>) -> Result<ChangesetRow, OkfError>;
pub async fn prune_empty(pool: &PgPool, older_than: DateTime<Utc>, now: DateTime<Utc>) -> Result<u64, OkfError>;

// Task 2 to Task 4: src/okf/ops/mod.rs (+ frontmatter.rs, log_index.rs, content.rs)
pub struct OpContext<'a> { pub now: DateTime<Utc>, pub by: &'a str }
pub struct OpOutcome {
    pub touched: Vec<String>,                          // catalog-relative paths written, in order, no duplicates
    pub replay: BTreeMap<String, Vec<ReplayOp>>,       // per touched path: the ops that redo it; a touched path absent here is a free edit
    pub message: String,                               // okf.py's printed line, e.g. "infra/x.md: generated { by: a, at: ... }"
    pub notes: Vec<String>,                            // okf.py's stderr notes (non-kebab name, created index)
}
pub enum StampField { Generated, Verified }
pub struct SourceEntry<'a> { pub id: &'a str, pub resource: &'a str, pub title: Option<&'a str>, pub author: Option<&'a str> }
pub enum LogKind { Update, Creation, Change, Import }  // rendered "Update" etc.
pub struct NewConcept<'a> {
    pub path: &'a str, pub frontmatter: &'a serde_json::Map<String, serde_json::Value>, pub body: &'a str,
    pub index_summary: Option<&'a str>, pub index_section: Option<&'a str>,
}
pub fn stamp(ws: &dyn Workspace, ctx: &OpContext<'_>, concept: &str, field: StampField) -> Result<OpOutcome, OkfError>;
pub fn review(ws: &dyn Workspace, ctx: &OpContext<'_>, concept: &str, outcome: &str, note: &str, questions: &[String]) -> Result<OpOutcome, OkfError>;
pub fn question(ws: &dyn Workspace, ctx: &OpContext<'_>, concept: &str, question: &str) -> Result<OpOutcome, OkfError>;
pub fn unquestion(ws: &dyn Workspace, ctx: &OpContext<'_>, concept: &str, question: &str) -> Result<OpOutcome, OkfError>;
pub fn source(ws: &dyn Workspace, ctx: &OpContext<'_>, concept: &str, entry: &SourceEntry<'_>) -> Result<OpOutcome, OkfError>;
pub fn log(ws: &dyn Workspace, ctx: &OpContext<'_>, kind: LogKind, text: &str) -> Result<OpOutcome, OkfError>;
pub fn index(ws: &dyn Workspace, ctx: &OpContext<'_>, concept: &str, summary: &str, section: Option<&str>, title: Option<&str>) -> Result<OpOutcome, OkfError>;
pub fn create_concept(ws: &dyn Workspace, ctx: &OpContext<'_>, new: &NewConcept<'_>) -> Result<OpOutcome, OkfError>;
pub fn edit_document(ws: &dyn Workspace, path: &str, old: &str, new: &str, replace_all: bool) -> Result<OpOutcome, OkfError>;
pub fn replace_body(ws: &dyn Workspace, concept: &str, body: &str) -> Result<OpOutcome, OkfError>;
pub fn set_frontmatter(ws: &dyn Workspace, concept: &str, key: &str, value: Option<&serde_json::Value>) -> Result<OpOutcome, OkfError>;

// Task 3: src/okf/ops/replay.rs
#[derive(Serialize, Deserialize)] #[serde(tag = "op", rename_all = "snake_case")]
pub enum ReplayOp {
    Log { date: String, bullet: String },                                   // date "2026-11-03"
    ListBullet { target: String, bullet: String, section: Option<String>, create_section: bool },
    CreateIndex { folder: String },
}
// Task 2 creates replay.rs with the enum only (OpOutcome needs it); Task 3 adds the functions.
pub fn replay(path: &str, current: Option<&str>, ops: &[ReplayOp]) -> Result<String, OkfError>;   // idempotent; current None = file missing
pub fn log_text(text: &str, date: &str, bullet: &str) -> Option<String>;                         // None = already logged
pub fn insert_index_bullet_text(text: &str, bullet: &str, section: Option<&str>, create_section: bool) -> Option<String>; // None = section missing
pub fn ensure_index(ws: &dyn Workspace, folder: &str, outcome: &mut OpOutcome) -> Result<(), OkfError>;           // pub (log_index.rs)

// Task 5: src/okf/rules.rs
pub fn is_machinery(path: &str) -> bool;                                 // normalized catalog-relative path
pub fn check_path(path: &str) -> Result<String, OkfError>;               // normalized, inside, not machinery → InvalidPath
pub fn check_content(path: &str, text: &str) -> Result<(), OkfError>;    // SecretDetected; FrontmatterParse for a .md

// src/error.rs additions (Tasks 4, 5, 1, 10)
OkfError::EditMismatch { path: String, matches: usize }                 // Task 4
OkfError::SecretDetected { path: String, line: usize, rule: String, message: String }  // Task 5 (the Finding's message, already masked)
OkfError::Busy { reason: String }                                       // Task 1: a hand-written From<sqlx::Error> maps 40P01 and 40001; HTTP 503
OkfError::LintFailed { errors: Vec<Finding>, warnings: Vec<Finding> }   // Task 10; HTTP 422
// okf.py's die(msg) becomes OkfError::Refused { reason: msg } with okf.py's exact text (Tasks 2-4)
OkfError::Conflict { files: Vec<ConflictFile> }                         // Task 10
pub struct ConflictFile { pub path: String, pub base_revision: Option<i32>, pub current_revision: Option<i32> }

// Task 6: src/selection.rs, src/mcp/context.rs, src/http/sessions.rs
pub const CHANGESET_HEADER: &str = "x-okf-changeset";
pub const CHANGESET_QUERY: &str = "changeset";
pub fn connection_label(parts: &Parts) -> String;                       // header, query, else "default" (never the session id)
pub const SESSION_HEADER: &str = "mcp-session-id"; pub const DEFAULT_LABEL: &str = "default";
impl ToolContext { pub fn label(&self) -> &str; pub fn with_label(self, label: &str) -> Self; }   // set in from_parts; DEFAULT_LABEL in ToolContext::new
pub struct SessionKeys { /* Mutex<HashMap<String, (String, Instant)>> */ }
pub async fn bind_session(State(app): State<Arc<App>>, request: Request, next: Next) -> Response;  // route_layer after require_key on /mcp
impl App { pub fn sessions(&self) -> &SessionKeys; }

// Task 7: src/overlay.rs, src/catalog.rs, src/okf/workspace.rs, src/okf/analysis/lint.rs
impl MemWorkspace { pub fn with_changes(&self, changes: impl IntoIterator<Item = (String, Option<StoredFile>)>) -> Result<MemWorkspace, OkfError>; } // a copy with entries applied; None deletes
impl CommitTimes { pub fn with_dirty(&self, dirty: impl IntoIterator<Item = String>) -> CommitTimes; }
impl Loaded { pub fn workspace(&self) -> &MemWorkspace; pub fn revision(&self, path: &str) -> Option<i32>; }
pub struct Overlay { /* base Arc<Loaded>, changeset id + version, MemWorkspace, Snapshot, dirty paths */ }
impl Overlay {
    pub fn snapshot(&self) -> &Snapshot; pub fn workspace(&self) -> &MemWorkspace;
    pub fn base(&self) -> &Arc<Loaded>; pub fn dirty(&self) -> &BTreeSet<String>; pub fn lint(&self, now: DateTime<Utc>) -> LintReport;
}
pub async fn build_overlay(base: Arc<Loaded>, changeset: &ChangesetRow, entries: Vec<EntryRow>) -> Result<Overlay, OkfError>;
pub struct Overlays { /* Mutex<HashMap<i64, Arc<Overlay>>> */ }
impl Overlays { pub fn new(idle: Duration) -> Self; pub async fn get(&self, pool: &PgPool, state: &CatalogState, changeset: &ChangesetRow) -> Result<Arc<Overlay>, OkfError>; pub fn forget(&self, changeset_id: i64); }
impl App { pub fn overlays(&self) -> &Overlays; }
pub enum View { Published(Arc<Loaded>), Pending(Arc<Overlay>) }
impl View { pub fn snapshot(&self) -> &Snapshot; pub fn lint(&self, now: DateTime<Utc>) -> LintReport; pub fn pending(&self) -> bool; }
impl ToolContext { pub async fn view(&self, state: &CatalogState) -> Result<View, OkfError>; }   // Pending iff this key+label has an open changeset there

// Task 8: src/write.rs, and src/store/changesets.rs (adds `lock`)
pub async fn lock(conn: &mut PgConnection, changeset_id: i64) -> Result<ChangesetRow, OkfError>; // store::changesets: FOR UPDATE, re-read; write::apply takes it before reading the overlay
pub struct WriteReport { pub catalog: String, pub changeset: i64, pub label: String, pub touched: Vec<String>, pub message: String, pub notes: Vec<String> }
#[derive(Serialize, JsonSchema)] (paths in WriteReport start with "/")
pub async fn apply<F>(ctx: &ToolContext, catalog: Option<&str>, by: Option<&str>, op: F) -> Result<WriteReport, OkfError>
where F: FnOnce(&MemWorkspace, &OpContext<'_>) -> Result<OpOutcome, OkfError> + Send + 'static;
// a touched path that is no longer a file after the op is stored as a deletion entry (revert; M6's delete)
impl MemWorkspace { pub fn remove(&self, path: &str) -> Result<bool, OkfError>; }    // Task 11
// target = catalog arg or selection write target; require Access::Write; tx: lock_catalog, get_or_create, changesets::lock;
// op runs (blocking) on a clone of the overlay workspace; each touched path: rules::check_path + check_content;
// base_revision = existing entry's base, else Loaded::revision(path); replay = existing entry's replay ++ new ops when
// both are Some (or the entry is new and the path has ops), else None; upsert_entry; commit; overlays.forget(id).

// Task 10: src/store/publish.rs
#[derive(Serialize, JsonSchema)]
pub struct PublishReport { pub published: bool, pub revisions: Vec<PublishedRevision>, pub warnings: Vec<Finding>, pub replayed: Vec<String> }
pub struct PublishedRevision { pub path: String, pub revision: i32, pub op: String }  // create | update | delete
pub async fn publish(app: &App, state: &CatalogState, changeset: &ChangesetRow, actor: &str, message: &str) -> Result<PublishReport, OkfError>; // actor = the publishing key's
// src/store/documents.rs gains DocumentWrite, RevisionOp, RevisionRecord, write_document, delete_document, record_revision; import uses them
// src/store/history.rs gains content_at (Task 11)
impl CatalogState { pub fn publish_lock(&self) -> &tokio::sync::Mutex<()>; }
```

## File map

| File | Created/modified by | Responsibility |
|---|---|---|
| `Cargo.toml` | T1 (sqlx `json`), T11 (`similar`) | Dependencies (keep both sides) |
| `migrations/0002_changeset_identity.sql` | T1 (create) | `label`, the key-bound unique index, `version` |
| `src/store/changesets.rs`, `src/store/mod.rs` | T1 (create; `pub mod changesets;`), T10 (`pub mod publish;`) | Changeset and entry rows |
| `src/store/catalogs.rs` | T1 (modify) | `delete`'s changeset query uses the renamed column; lock order comment |
| `src/error.rs` | T1 (`Busy`), T4 (`EditMismatch`), T5 (`SecretDetected`), T10 (`LintFailed`, `Conflict`) | Variants (keep both sides) |
| `src/http/mod.rs` | T1 (`Busy` status), T4, T5, T10 (statuses), T6 (`bind_session` layer) | Router, `AppError::status` |
| `src/okf/ops/mod.rs` | T2 (create), T3, T4 (add `mod` lines) | Shared op types |
| `src/okf/ops/frontmatter.rs` | T2 (create) | `stamp`, `review`, `question`, `unquestion`, `source` |
| `src/okf/ops/log_index.rs` | T3 (create) | `log`, `index`, `ensure_index` |
| `src/okf/ops/replay.rs` | T2 (create, the `ReplayOp` enum only), T3 (the functions) | `ReplayOp`, `replay`, `log_text`, `insert_index_bullet_text` |
| `src/okf/ops/content.rs` | T4 (create) | `create_concept`, `edit_document`, `replace_body`, `set_frontmatter` |
| `src/okf/mod.rs` | T2 (`pub mod ops;`), T5 (`pub mod rules;`) | Module tree |
| `tests/parity/okf_shim.py`, `tests/parity.rs`, `tests/golden/ops.json` | T2 (create the `ops` command and golden), T3 (extends the script with `log` and `index`) | Ops parity |
| `src/okf/rules.rs` | T5 (create) | Write rules |
| `src/okf/doc.rs` | T2 (`Doc::remove_lines`), T4 (`is_top_key`) | Line removal for `unquestion`; top-level key test for `edit_document` |
| `src/okf/analysis/links.rs` | T3 (`INDEX_HEADING` becomes `pub(crate)`) | Index headings shared with `insert_index_bullet` |
| `src/okf/workspace.rs` | T7 (`MemWorkspace::with_changes`), T11 (`MemWorkspace::remove`) | Overlay copies, deletions |
| `src/okf/analysis/lint.rs` | T7 (`CommitTimes::with_dirty`) | Dirty paths for `lint pending` |
| `src/mcp/testing.rs` | T6 (`principal_as`, `store_key`, `PendingEdit`, `pending_entry`), T7 (`app_at`), T8 (`writer`) | Test support |
| `tests/support/server.rs` | T13 (`restart`, `client_with`) | Harness restart on the same database |
| `src/store/import.rs` | T10 (uses `store::documents::write_document` and friends) | Shared document writers |
| `src/selection.rs`, `src/mcp/context.rs` | T6 (label), T7 (`view`) | Connection identity, views |
| `src/http/sessions.rs`, `src/app.rs` | T6 (create; `App::sessions`), T7 (`App::overlays`), T12 (prune in `poll`) | Session binding, shared state |
| `src/catalog.rs` | T7 (`Loaded::{workspace, revision}`), T10 (`publish_lock`) | Catalog state |
| `src/overlay.rs`, `src/lib.rs` | T7 (create; `pub mod overlay;`), T8 (`pub mod write;`) | Overlays |
| `src/mcp/{search,documents,history,reports}.rs` | T7 (read through `ctx.view`; `lint` gains `pending`) | Read tools see pending edits |
| `src/write.rs` | T8 (create) | The write path |
| `src/mcp/writes.rs` | T8 (create: content tools), T9 (bookkeeping tools) | Write tools |
| `src/mcp/changesets.rs` | T10 (create: `publish`), T11 (`discard`, `revert`, `pending_changes`), T12 (`stranded_sessions`, `adopt_changeset`) | Changeset tools |
| `src/mcp/mod.rs` | T8, T10 (one router line each) | `OkfServer::tools()` (keep both sides) |
| `src/store/publish.rs`, `src/store/documents.rs` | T10 (create; a shared `write_document` helper) | Publish |
| `src/store/history.rs` | T11 (`content_at`) | Revision content |
| `tests/server.rs`, `tests/writes.rs` | T6 (session binding tests), T13 (create `writes.rs`) | End-to-end |
| `tests/smoke.rs`, `README.md` | T14 | Smoke test, docs |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | T1 (identity, lock order), T5 (machinery list), T6 (binding), T10 (publish), T14 (M5 delivered) | Design |
| `changelog/*-m5-*.md` | every task | Iteration notes |

## Task dependencies

Layered (each task needs everything it points from; the edge list below is the source of truth):

```
Layer 0:  T1 changesets    T2 ops: frontmatter    T5 write rules
Layer 1:  T6 identity + session binding (T1)      T3 ops: log/index/replay (T2)
Layer 2:  T7 overlay + reads (T1, T6)    T12 stranded/adopt/prune (T1, T6)    T4 ops: content (T3, T5)
Layer 3:  T8 write path + content tools (T4, T5, T6, T7)
Layer 4:  T9 bookkeeping tools (T2, T3, T8)       T10 publish (T3, T5, T8)
Layer 5:  T11 discard/revert/pending (T8, T10)
Layer 6:  T13 end-to-end + concurrency (T9, T10, T11, T12)
Layer 7:  T14 smoke, README, design, changelog (all)
```

Edges, explicitly:
- T1 -> T6; T1 -> T7; T1 -> T12
- T6 -> T7 (`ToolContext::view` reads the label)
- T2 -> T3 (`OpOutcome.replay` holds `ReplayOp`, whose enum Task 2 creates); T3 -> T4
- T5 -> T4 (`edit_document` calls `rules::is_machinery`)
- T4 -> T8; T5 -> T8; T6 -> T8; T7 -> T8
- T2 -> T9; T3 -> T9; T8 -> T9
- T3 -> T10; T5 -> T10; T8 -> T10
- T8 -> T11; T10 -> T11
- T6 -> T12
- T9 -> T13; T10 -> T13; T11 -> T13; T12 -> T13
- every task -> T14

Tasks with no dependencies, which can start straight away: **T1, T2, T5**.
- T6 and T12 follow T1; T7 follows T6.
- T3 follows T2, and T4 follows T3.
- Shared files:
  - `Cargo.toml`, `src/error.rs`, `src/http/mod.rs`, `src/lib.rs` and `src/okf/mod.rs` take a few lines from several tasks;
  - T2 and T3 both extend the parity shim and golden.

  Keep both sides.

---

### Task 1: `store::changesets`: changesets keyed by (catalog, key, label), entries, and the lock order

> Interfaces note: written against `main` at `8161b3f`. (1) `store::catalogs::delete` has no query that names `session_id`; only the tests of `src/store/catalogs.rs` and `src/db.rs` do (and `db.rs` matches the index name `changesets_one_open_per_session`), so this task changes those, and in `delete` only adds `ORDER BY id` to the changeset lock and rewrites the lock-order doc. (2) `lock_catalog` keeps its signature; a catalog row that is gone (deleted while the write waited) is `OkfError::Refused` with a fixed message, because the function has only the id to name it by. (3) `OkfError::Busy` comes out of a hand-written `From<sqlx::Error>` (the `#[from]` on `Database` becomes `#[source]`), so every `?` on a driver error maps SQLSTATE `40P01` (deadlock) and `40001` (serialization failure) to `Busy` without a helper at each call site. `upsert_entry` and `remove_entries` also answer `Busy` when the changeset stopped being open meanwhile (a concurrent publish, discard or prune). (4) `app::note_failure` matches with a `_` arm, so `Busy` gets an explicit arm there (retry at the next poll, like `Database`), or a deadlock during a refresh would mark the catalog broken. (5) Added `ChangesetState::{as_str, parse}`, and `EntryRow` derives `sqlx::FromRow`.

Decision 1's identity and Decision 3's lock order, in the database. Migration `0002_changeset_identity.sql` renames `changesets.session_id` to `label`, makes the open-changeset unique index `(catalog_id, key_id, label)`, and adds `version`, which every entry change bumps (Task 7's overlay cache keys on it). `src/store/changesets.rs` holds every query the later tasks need: get-or-create, entries, state changes, the stranded list, adoption and the prune. Every write transaction takes the catalog row `FOR KEY SHARE` first (`lock_catalog`), then the changeset row (the `UPDATE` that bumps `version`), then entries; `catalogs::delete` takes the catalog row `FOR UPDATE` before the changesets, so the two orders agree and cannot deadlock. The prune takes no catalog lock and skips rows other transactions hold (`SKIP LOCKED`), so it never waits.

**Files:**
- Modify: `Cargo.toml` (sqlx `json` feature)
- Create: `migrations/0002_changeset_identity.sql`
- Create: `src/store/changesets.rs`; modify `src/store/mod.rs` (`pub mod changesets;`)
- Modify: `src/store/catalogs.rs` (`delete`'s lock order and doc; tests use `label`)
- Modify: `src/db.rs` (the constraint test)
- Modify: `src/error.rs` (`Busy`, `From<sqlx::Error>`)
- Modify: `src/http/mod.rs` (`Busy` is 503)
- Modify: `src/app.rs` (`note_failure` arm for `Busy`)
- Modify: `src/cli.rs` (`migrate_applies_the_schema` counts the embedded migrations)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Decisions", "Storage", "Session lifecycle", "Errors", "Open questions for M5")
- Create: `changelog/<ts>-m5-task-01-changesets.md`

> **Corrected (issue #90), 2026-11-03.** `cli::tests::migrate_applies_the_schema` asserted exactly one row in `_sqlx_migrations`, so the second migration failed it. It now compares against `crate::db::MIGRATOR.iter().count()`, so later migrations need no change there. The rest of this task's code shipped as written (`rustfmt` of Step 6 plus Step 2 equals `src/store/changesets.rs`). Review of PR #106 added three things, none of which changes an interface: `From<sqlx::Error>` logs a retryable driver error at `warn` before answering `Busy` (the variant drops it); a 12th test, `the_prune_passes_by_a_changeset_get_or_create_found_and_holds`, pins the conflict path's `FOR UPDATE` against the prune's `SKIP LOCKED`; and the design's "Lock order" paragraph covers key deletes.

- [ ] **Step 1: The dependency**

In `Cargo.toml`, add `"json"` to sqlx's features and extend its comment:

```toml
# Postgres access with runtime-checked queries, and the schema migrations
# embedded at compile time (`migrate!` needs `macros`). `json`: the
# `changeset_entries.replay` jsonb column as `serde_json::Value`. No TLS
# feature yet: that is a deployment decision (M7).
sqlx = { version = "0.9.0", default-features = false, features = ["runtime-tokio", "postgres", "chrono", "migrate", "macros", "json"] }
```

- [ ] **Step 2: Write the failing tests**

Create `src/store/changesets.rs` with only the test module below (and `pub mod changesets;` in `src/store/mod.rs`), so it fails to compile until Step 4 adds the code.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::catalogs::tests::until_a_lock_is_awaited;
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use chrono::{TimeDelta, TimeZone};
    use std::time::Duration;

    /// Generous: the tests share a loaded machine.
    const PATIENCE: Duration = Duration::from_secs(30);

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap()
    }

    /// Creates catalog `pg` and the keys `alice` and `bob` (the foreign key
    /// `changesets.key_id` needs them), returning the catalog's id.
    async fn setup(pool: &PgPool) -> i64 {
        let pg = catalogs::create(pool, "pg", "", now()).await.unwrap();
        for id in ["alice", "bob"] {
            let new = NewKey {
                id,
                access: Access::Write,
                grants: Grants::All,
                default_catalog: None,
                actor: id,
            };
            key_rows::create(pool, &new, now()).await.unwrap();
        }
        pg.id
    }

    fn key<'a>(catalog_id: i64, key_id: &'a str, label: &'a str) -> ChangesetKey<'a> {
        ChangesetKey {
            catalog_id,
            key_id,
            label,
        }
    }

    /// Gets or creates the changeset in its own transaction, in the lock order.
    async fn open(pool: &PgPool, key: &ChangesetKey<'_>, at: DateTime<Utc>) -> ChangesetRow {
        let mut tx = pool.begin().await.unwrap();
        lock_catalog(&mut tx, key.catalog_id).await.unwrap();
        let changeset = get_or_create(&mut tx, key, key.key_id, at).await.unwrap();
        tx.commit().await.unwrap();
        changeset
    }

    fn entry(path: &str, text: Option<&str>) -> EntryRow {
        EntryRow {
            path: path.to_string(),
            base_revision: None,
            content: text.map(|text| text.as_bytes().to_vec()),
            executable: false,
            replay: None,
        }
    }

    /// Stores one entry in its own transaction, in the lock order.
    async fn put(
        pool: &PgPool,
        changeset: &ChangesetRow,
        entry: &EntryRow,
        at: DateTime<Utc>,
    ) -> Result<(), OkfError> {
        let mut tx = pool.begin().await?;
        lock_catalog(&mut tx, changeset.catalog_id).await?;
        upsert_entry(&mut tx, changeset.id, entry, at).await?;
        tx.commit().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_returns_the_open_changeset_it_made() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let alice = key(pg, "alice", "default");

        let first = open(&db.pool, &alice, now()).await;
        let again = open(&db.pool, &alice, now() + TimeDelta::minutes(5)).await;
        let found = find_open(&db.pool, &alice).await.unwrap();

        assert_eq!(again, first);
        assert_eq!(found, Some(first.clone()));
        assert_eq!(
            (first.label.as_str(), first.key_id.as_deref(), first.actor.as_str()),
            ("default", Some("alice"), "alice")
        );
        assert_eq!((first.state, first.version), (ChangesetState::Open, 0));
        assert_eq!((first.created_at, first.updated_at), (now(), now()));
        assert_eq!(by_id(&db.pool, first.id).await.unwrap(), Some(first));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_label_and_each_key_get_their_own_changeset() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;

        let alice = open(&db.pool, &key(pg, "alice", "default"), now()).await;
        let alice_drafts = open(&db.pool, &key(pg, "alice", "drafts"), now()).await;
        let bob = open(&db.pool, &key(pg, "bob", "default"), now()).await;

        assert_ne!(alice.id, alice_drafts.id);
        assert_ne!(alice.id, bob.id);
        assert_eq!(bob.key_id.as_deref(), Some("bob"));
        assert_eq!(
            find_open(&db.pool, &key(pg, "bob", "drafts")).await.unwrap(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_replaces_the_entry_and_bumps_the_version() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let changeset = open(&db.pool, &key(pg, "alice", "default"), now()).await;
        let later = now() + TimeDelta::minutes(1);
        let mut replayed = entry("log.md", Some("# Log\n"));
        replayed.replay = Some(serde_json::json!([{"op": "log", "date": "2026-11-03"}]));

        put(&db.pool, &changeset, &entry("a.md", Some("one\n")), now())
            .await
            .unwrap();
        put(&db.pool, &changeset, &entry("a.md", Some("two\n")), later)
            .await
            .unwrap();
        put(&db.pool, &changeset, &replayed, later).await.unwrap();

        let stored = entries(&db.pool, changeset.id).await.unwrap();
        assert_eq!(stored, [entry("a.md", Some("two\n")), replayed]);
        let after = by_id(&db.pool, changeset.id).await.unwrap().unwrap();
        assert_eq!((after.version, after.updated_at), (3, later));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remove_entries_drops_the_named_paths_or_all_of_them() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let changeset = open(&db.pool, &key(pg, "alice", "default"), now()).await;
        for path in ["a.md", "b.md", "c.md"] {
            put(&db.pool, &changeset, &entry(path, Some("x\n")), now())
                .await
                .unwrap();
        }
        let named = ["b.md".to_string(), "nope.md".to_string()];

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let one = remove_entries(&mut tx, changeset.id, Some(&named), now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let left: Vec<String> = entries(&db.pool, changeset.id)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let rest = remove_entries(&mut tx, changeset.id, None, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(one, 1);
        assert_eq!(left, ["a.md", "c.md"]);
        assert_eq!(rest, 2);
        assert!(entries(&db.pool, changeset.id).await.unwrap().is_empty());
        // Three puts and two removals.
        assert_eq!(by_id(&db.pool, changeset.id).await.unwrap().unwrap().version, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_state_closes_a_changeset_and_a_new_one_opens_after_it() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let alice = key(pg, "alice", "default");
        let changeset = open(&db.pool, &alice, now()).await;

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        set_state(&mut tx, changeset.id, ChangesetState::Published, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let twice = set_state(&mut tx, changeset.id, ChangesetState::Discarded, now()).await;
        drop(tx);
        let late_write = put(&db.pool, &changeset, &entry("a.md", Some("x\n")), now()).await;
        let next = open(&db.pool, &alice, now()).await;

        let closed = by_id(&db.pool, changeset.id).await.unwrap().unwrap();
        assert_eq!(closed.state, ChangesetState::Published);
        assert!(matches!(twice, Err(OkfError::Refused { .. })));
        assert!(matches!(late_write, Err(OkfError::Busy { .. })));
        assert_ne!(next.id, changeset.id);
        assert_eq!(find_open(&db.pool, &alice).await.unwrap(), Some(next));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stranded_lists_only_open_non_empty_idle_changesets() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let old = now() - TimeDelta::hours(3);
        let idle = open(&db.pool, &key(pg, "alice", "idle"), old).await;
        put(&db.pool, &idle, &entry("b.md", Some("b\n")), old).await.unwrap();
        put(&db.pool, &idle, &entry("a.md", None), old).await.unwrap();
        open(&db.pool, &key(pg, "alice", "empty"), old).await;
        let fresh = open(&db.pool, &key(pg, "alice", "fresh"), now()).await;
        put(&db.pool, &fresh, &entry("a.md", Some("a\n")), now())
            .await
            .unwrap();
        let published = open(&db.pool, &key(pg, "bob", "done"), old).await;
        put(&db.pool, &published, &entry("a.md", Some("a\n")), old)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        set_state(&mut tx, published.id, ChangesetState::Published, old)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows = stranded(&db.pool, now() - TimeDelta::hours(2)).await.unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].changeset.id, idle.id);
        assert_eq!(rows[0].catalog, "pg");
        assert_eq!(rows[0].paths, ["a.md", "b.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_relabels_and_refuses_a_label_that_already_has_one() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let stranded_one = open(&db.pool, &key(pg, "alice", "old-session"), now()).await;
        let taken = open(&db.pool, &key(pg, "bob", "default"), now()).await;
        let later = now() + TimeDelta::minutes(1);

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let refused = adopt(&mut tx, stranded_one.id, "bob", "default", later).await;
        drop(tx);
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let adopted = adopt(&mut tx, stranded_one.id, "bob", "work", later)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(matches!(refused, Err(OkfError::Refused { reason }) if reason.contains("already")));
        assert_eq!(adopted.id, stranded_one.id);
        assert_eq!(
            (adopted.key_id.as_deref(), adopted.label.as_str()),
            (Some("bob"), "work")
        );
        assert_eq!((adopted.version, adopted.updated_at), (1, later));
        assert_eq!(
            find_open(&db.pool, &key(pg, "alice", "old-session")).await.unwrap(),
            None
        );
        assert_eq!(
            find_open(&db.pool, &key(pg, "bob", "default")).await.unwrap(),
            Some(taken)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_discards_only_empty_changesets_idle_past_the_cutoff() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let old = now() - TimeDelta::hours(25);
        let empty_old = open(&db.pool, &key(pg, "alice", "a"), old).await;
        let empty_new = open(&db.pool, &key(pg, "alice", "b"), now()).await;
        let full_old = open(&db.pool, &key(pg, "alice", "c"), old).await;
        put(&db.pool, &full_old, &entry("a.md", Some("a\n")), old)
            .await
            .unwrap();

        let pruned = prune_empty(&db.pool, now() - TimeDelta::hours(24), now())
            .await
            .unwrap();

        let state = |row: Option<ChangesetRow>| row.unwrap().state;
        assert_eq!(pruned, 1);
        assert_eq!(
            state(by_id(&db.pool, empty_old.id).await.unwrap()),
            ChangesetState::Discarded
        );
        assert_eq!(
            state(by_id(&db.pool, empty_new.id).await.unwrap()),
            ChangesetState::Open
        );
        assert_eq!(
            state(by_id(&db.pool, full_old.id).await.unwrap()),
            ChangesetState::Open
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deadlock_reported_by_postgres_is_busy() {
        let db = test_db().await;

        // `RAISE ... USING ERRCODE` makes Postgres fail with that SQLSTATE,
        // as if it had picked this statement as a deadlock victim.
        let error: OkfError = sqlx::query(
            "DO $$ BEGIN RAISE EXCEPTION 'victim' USING ERRCODE = '40P01'; END $$",
        )
        .execute(&db.pool)
        .await
        .unwrap_err()
        .into();

        assert_eq!(error.kind(), "Busy");
        assert!(!error.is_internal());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_waits_for_a_write_then_refuses_its_work() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        // `oneshot` channels: one message each way, so the test decides when
        // the writer commits.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let (commit_tx, commit_rx) = tokio::sync::oneshot::channel::<()>();
        let pool = db.pool.clone();
        let writer = tokio::spawn(async move {
            let mut tx = pool.begin().await?;
            lock_catalog(&mut tx, pg).await?;
            let changeset = get_or_create(&mut tx, &key(pg, "alice", "default"), "alice", now())
                .await?;
            upsert_entry(&mut tx, changeset.id, &entry("a.md", Some("x\n")), now()).await?;
            ready_tx.send(()).expect("the test waits");
            commit_rx.await.expect("the test signals");
            tx.commit().await?;
            Ok::<(), OkfError>(())
        });
        ready_rx.await.unwrap();
        let pool = db.pool.clone();
        let deleting = tokio::spawn(async move { catalogs::delete(&pool, "pg", false).await });
        until_a_lock_is_awaited(&db.pool).await;

        commit_tx.send(()).unwrap();
        let written = tokio::time::timeout(PATIENCE, writer).await;
        let deleted = tokio::time::timeout(PATIENCE, deleting).await;

        written.expect("the write finishes").unwrap().unwrap();
        let deleted = deleted.expect("the delete finishes").unwrap();
        assert!(matches!(deleted, Err(OkfError::Refused { reason }) if reason.contains("--force")));
        assert_eq!(catalogs::list(&db.pool).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_waits_for_a_delete_then_sees_the_catalog_gone() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        // The first two steps of `catalogs::delete`, held open: the catalog
        // row `FOR UPDATE`, then its open changesets.
        let mut deleter = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM catalogs WHERE id = $1 FOR UPDATE")
            .bind(pg)
            .execute(&mut *deleter)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let writer = tokio::spawn(async move {
            let mut tx = pool.begin().await?;
            lock_catalog(&mut tx, pg).await?;
            get_or_create(&mut tx, &key(pg, "alice", "default"), "alice", now()).await?;
            tx.commit().await?;
            Ok::<(), OkfError>(())
        });
        until_a_lock_is_awaited(&db.pool).await;

        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(pg)
            .execute(&mut *deleter)
            .await
            .unwrap();
        deleter.commit().await.unwrap();
        let written = tokio::time::timeout(PATIENCE, writer)
            .await
            .expect("the write finishes")
            .unwrap();

        assert!(matches!(written, Err(OkfError::Refused { reason }) if reason == CATALOG_GONE));
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM changesets")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test --lib -- store::changesets`
Expected: compile errors (`ChangesetKey`, `lock_catalog`, `get_or_create`, ... not found).

- [ ] **Step 4: The migration**

Create `migrations/0002_changeset_identity.sql`:

```sql
-- Changeset identity (M5, Decision 1): one open changeset per (catalog,
-- key, label). The label comes from the connection (`X-OKF-Changeset`,
-- `?changeset=`, else 'default'; never the MCP session id), so a client
-- keeps its changeset across reconnects and restarts, and the key is part of
-- the identity so one key never sees another's.
ALTER TABLE changesets RENAME COLUMN session_id TO label;

DROP INDEX changesets_one_open_per_session;
-- A racing "get or create" cannot open two changesets for one identity.
-- A NULL key_id (its key deleted) never conflicts, as NULLs are distinct.
CREATE UNIQUE INDEX changesets_one_open_per_label
    ON changesets (catalog_id, key_id, label) WHERE state = 'open';

-- Bumped by every change to the changeset or its entries; overlay caches
-- are valid while it (and the catalog's generation) stays the same.
ALTER TABLE changesets ADD COLUMN version bigint NOT NULL DEFAULT 0;
```

`build.rs` already reruns the build when `migrations/` changes.

- [ ] **Step 5: `OkfError::Busy` and the driver-error conversion**

In `src/error.rs`:

1. Change the `Database` variant's field attribute from `#[from]` to `#[source]` (and its comment):

```rust
    /// A database operation failed. M4 logs the details and returns only the
    /// variant to MCP clients; the CLI prints them.
    #[error("database: {source}")]
    Database {
        /// The driver's error.
        // `#[source]`, not `#[from]`: the `From` impl below is written by
        // hand, so a deadlock becomes `Busy` instead.
        #[source]
        source: sqlx::Error,
    },
```

2. Add the variant after `Refused`:

```rust
    /// The database aborted the request to keep others going (a deadlock or
    /// serialization failure), or the changeset changed under it. Nothing
    /// was stored: retry.
    #[error("busy: {reason}; retry")]
    Busy {
        /// What was aborted.
        reason: String,
    },
```

3. Add the conversion after the `impl OkfError` block:

```rust
/// SQLSTATEs that mean "Postgres aborted this transaction so others could go
/// on; running it again will likely succeed": `40P01` (deadlock detected) and
/// `40001` (serialization failure, from a repeatable-read snapshot).
const RETRYABLE_SQLSTATES: [&str; 2] = ["40P01", "40001"];

// The conversion `?` uses for every driver error: a retryable failure is
// `Busy`, anything else `Database`. (Before M5, `#[from]` generated this
// impl without the check.)
impl From<sqlx::Error> for OkfError {
    fn from(source: sqlx::Error) -> Self {
        // `code()` borrows `source`, so the check finishes before `source`
        // moves into the variant below. It returns `Option<Cow<str>>`, and
        // `as_ref()` views the `Cow` as a `&str`.
        let retryable = source
            .as_database_error()
            .and_then(|db_error| db_error.code())
            .is_some_and(|code| RETRYABLE_SQLSTATES.contains(&code.as_ref()));
        if retryable {
            OkfError::Busy {
                reason: "the database aborted this request to resolve a lock conflict"
                    .to_string(),
            }
        } else {
            OkfError::Database { source }
        }
    }
}
```

4. Add the arms: in `kind`, `OkfError::Busy { .. } => "Busy",`; in `to_json`'s last group, `| OkfError::Busy { .. }` beside `Refused` (no extra field). `is_internal` stays as it is: `Busy` is not the server's fault, and its message names no path or secret. The MCP mapping is `mcp::tool_error`, which goes through `to_json` and `is_internal`, so it needs no change: a `Busy` tool result is `{"error": "Busy", "message": "busy: ...; retry"}`, logged at debug level.

In `src/http/mod.rs`, `AppError::status`: add `OkfError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,` and end the doc's list with "503 for a request the database aborted (retry it)". Add this test beside `app_errors_have_a_status_and_the_error_json`:

```rust
    #[test]
    fn busy_is_503_and_says_to_retry() {
        let busy = AppError(OkfError::Busy {
            reason: "deadlock".to_string(),
        });

        let status = busy.status();
        let body = busy.0.to_json();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "Busy");
        assert!(body["message"].as_str().unwrap().ends_with("retry"));
    }
```

In `src/app.rs`, `note_failure`: add an arm before the `_` arm, so a deadlock during a refresh is retried instead of marking the generation broken:

```rust
        // Transient: the next pass tries again, as for a database error.
        OkfError::Database { .. } | OkfError::Busy { .. } => "at the next poll",
```

(replacing the `OkfError::Database { .. } => "at the next poll",` arm).

- [ ] **Step 6: The store**

Put this above the test module in `src/store/changesets.rs`:

```rust
//! Changesets and their entries (design "Storage", "Session lifecycle"):
//! one open changeset per (catalog, key, label) holds a connection's
//! unpublished edits until `publish` or `discard` closes it.
//!
//! # Lock order (Decision 3)
//!
//! Every transaction that writes a changeset or its entries takes:
//! 1. the catalog row, `FOR KEY SHARE` ([`lock_catalog`]), first;
//! 2. the changeset row (the `UPDATE` that bumps `version`);
//! 3. the entry rows.
//!
//! `catalogs::delete` takes the catalog row `FOR UPDATE` before the open
//! changesets, so a write and a delete wait for each other in the same order
//! and cannot deadlock: a write that waited sees the catalog gone. Every
//! function here that writes takes its caller's transaction, after
//! [`lock_catalog`]; only [`prune_empty`] runs on its own, without the
//! catalog lock, skipping rows another transaction holds (`SKIP LOCKED`), so
//! it never waits for anyone. A deadlock Postgres still detects is
//! [`OkfError::Busy`], through `From<sqlx::Error>`.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::is_unique_violation;

/// What [`lock_catalog`] reports when the catalog row is gone: it was
/// deleted while the write waited for the delete to finish.
pub const CATALOG_GONE: &str = "the catalog was deleted while this request waited; nothing was written";

/// How often [`get_or_create`] tries again when the changeset it found
/// closes before it can read it back.
const GET_OR_CREATE_TRIES: usize = 3;

/// The columns of a changeset row, in [`RawChangeset`]'s order.
// A macro rather than a `const`: `concat!` (below) joins string literals at
// compile time, and it expands a macro call among its arguments into one,
// while it cannot read a `const`. So every query stays one literal string.
macro_rules! changeset_columns {
    () => {
        "id, catalog_id, label, key_id, actor, state, version, created_at, updated_at"
    };
}

/// Where a changeset is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesetState {
    /// Receiving edits.
    Open,
    /// Its entries became documents and revisions.
    Published,
    /// Dropped by `discard` or by the prune.
    Discarded,
}

impl ChangesetState {
    /// The spelling stored in `changesets.state`.
    ///
    /// # Returns
    /// `"open"`, `"published"` or `"discarded"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ChangesetState::Open => "open",
            ChangesetState::Published => "published",
            ChangesetState::Discarded => "discarded",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - a `changesets.state` value.
    ///
    /// # Returns
    /// The state, or `None` for anything [`ChangesetState::as_str`] does not
    /// produce.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "open" => Some(ChangesetState::Open),
            "published" => Some(ChangesetState::Published),
            "discarded" => Some(ChangesetState::Discarded),
            _ => None,
        }
    }
}

/// A changeset row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesetRow {
    /// Database id (what `adopt_changeset` takes).
    pub id: i64,
    /// Its catalog.
    pub catalog_id: i64,
    /// The connection's label (Decision 1).
    pub label: String,
    /// The key it belongs to; `None` once that key's row is deleted.
    pub key_id: Option<String>,
    /// The actor of the key that opened it.
    pub actor: String,
    /// Open, published or discarded.
    pub state: ChangesetState,
    /// Bumped by every change to it or its entries.
    pub version: i64,
    /// When it was opened (server clock).
    pub created_at: DateTime<Utc>,
    /// Its last change (server clock).
    pub updated_at: DateTime<Utc>,
}

/// One changed path of a changeset.
// `FromRow`: the columns map onto the fields by name. `replay` is `jsonb`,
// which sqlx's `json` feature decodes into `serde_json::Value`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EntryRow {
    /// Catalog-relative path.
    pub path: String,
    /// The published revision the edit was based on; `None` when the path
    /// had no live document.
    pub base_revision: Option<i32>,
    /// The new bytes; `None` deletes the path.
    pub content: Option<Vec<u8>>,
    /// The new file's execute bit.
    pub executable: bool,
    /// The replay operations (Task 3's `ReplayOp`s as JSON) while every
    /// change to this entry came from one; `None` otherwise.
    pub replay: Option<serde_json::Value>,
}

/// The identity of an open changeset (Decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangesetKey<'a> {
    /// The catalog.
    pub catalog_id: i64,
    /// The key's id.
    pub key_id: &'a str,
    /// The connection's label.
    pub label: &'a str,
}

/// An open, non-empty changeset idle past a cutoff (`stranded_sessions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedRow {
    /// The changeset.
    pub changeset: ChangesetRow,
    /// Its catalog's name.
    pub catalog: String,
    /// Its changed paths, sorted.
    pub paths: Vec<String>,
}

/// A changeset row as selected, before `state` is parsed.
#[derive(sqlx::FromRow)]
struct RawChangeset {
    id: i64,
    catalog_id: i64,
    label: String,
    key_id: Option<String>,
    actor: String,
    state: String,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<RawChangeset> for ChangesetRow {
    fn from(raw: RawChangeset) -> Self {
        Self {
            id: raw.id,
            catalog_id: raw.catalog_id,
            label: raw.label,
            key_id: raw.key_id,
            actor: raw.actor,
            // The column's CHECK constraint allows only the three spellings.
            state: ChangesetState::parse(&raw.state).expect("the schema checks changesets.state"),
            version: raw.version,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        }
    }
}

/// A [`StrandedRow`] as selected.
#[derive(sqlx::FromRow)]
struct RawStranded {
    // `flatten`: the changeset's columns sit beside `catalog` and `paths` in
    // the same result row.
    #[sqlx(flatten)]
    changeset: RawChangeset,
    catalog: String,
    paths: Vec<String>,
}

/// The error for a changeset that stopped being open under a write.
fn no_longer_open(changeset_id: i64) -> OkfError {
    OkfError::Busy {
        reason: format!("changeset {changeset_id} was published or discarded meanwhile"),
    }
}

/// Takes the catalog row `FOR KEY SHARE` for the rest of the transaction:
/// the first lock of every write to changesets or entries (Decision 3).
///
/// `FOR KEY SHARE` lets other writers and publishes (whose `UPDATE` of
/// `generation` takes `FOR NO KEY UPDATE`) through, and holds off only
/// `catalogs::delete`'s `FOR UPDATE`.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `catalog_id` - the catalog.
///
/// # Errors
/// [`OkfError::Refused`] with [`CATALOG_GONE`] if the row is gone (a delete
/// committed while this waited); [`OkfError::Database`].
pub async fn lock_catalog(conn: &mut PgConnection, catalog_id: i64) -> Result<(), OkfError> {
    let locked: Option<i64> = sqlx::query_scalar("SELECT id FROM catalogs WHERE id = $1 FOR KEY SHARE")
        .bind(catalog_id)
        .fetch_optional(&mut *conn)
        .await?;
    if locked.is_none() {
        return Err(OkfError::Refused {
            reason: CATALOG_GONE.to_string(),
        });
    }
    Ok(())
}

/// The open changeset of an identity.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction (`&mut *tx`).
/// * `key` - catalog, key and label.
///
/// # Returns
/// The changeset, or `None` if that identity has none open. One lookup in
/// the partial unique index.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_open<'e>(
    executor: impl PgExecutor<'e>,
    key: &ChangesetKey<'_>,
) -> Result<Option<ChangesetRow>, OkfError> {
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "SELECT ",
        changeset_columns!(),
        " FROM changesets WHERE catalog_id = $1 AND key_id = $2 AND label = $3 \
         AND state = 'open'"
    ))
    .bind(key.catalog_id)
    .bind(key.key_id)
    .bind(key.label)
    .fetch_optional(executor)
    .await?;
    // `Option::map` with a `From` function: `Some(raw)` becomes `Some(row)`.
    Ok(row.map(ChangesetRow::from))
}

/// The open changeset of an identity, opening one if there is none.
///
/// The insert skips a conflicting open row (`ON CONFLICT DO NOTHING`, which
/// waits for a racing insert to commit or roll back), then reads the row
/// that won `FOR UPDATE`. Either way the transaction holds the changeset's
/// row lock until it ends. Call it after [`lock_catalog`], in the write's
/// transaction.
///
/// # Arguments
/// * `conn` - the transaction's connection.
/// * `key` - catalog, key and label.
/// * `actor` - the key's actor, recorded on a new changeset.
/// * `now` - the time (server clock), for a new changeset's timestamps.
///
/// # Returns
/// The open changeset.
///
/// # Errors
/// [`OkfError::Busy`] if the changeset kept closing between the insert and
/// the read; [`OkfError::Database`].
pub async fn get_or_create(
    conn: &mut PgConnection,
    key: &ChangesetKey<'_>,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<ChangesetRow, OkfError> {
    for _ in 0..GET_OR_CREATE_TRIES {
        let inserted = sqlx::query_as::<_, RawChangeset>(concat!(
            "INSERT INTO changesets (catalog_id, key_id, label, actor, state, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, 'open', $5, $5) \
             ON CONFLICT (catalog_id, key_id, label) WHERE state = 'open' DO NOTHING \
             RETURNING ",
            changeset_columns!()
        ))
        .bind(key.catalog_id)
        .bind(key.key_id)
        .bind(key.label)
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = inserted {
            return Ok(row.into());
        }
        // Read committed: this statement sees the row that won the race.
        // `FOR UPDATE` takes that row's lock too, as the insert above does
        // for a new row, so from here on the write holds its changeset and
        // the prune (`SKIP LOCKED`) passes it by.
        let found = sqlx::query_as::<_, RawChangeset>(concat!(
            "SELECT ",
            changeset_columns!(),
            " FROM changesets WHERE catalog_id = $1 AND key_id = $2 AND label = $3 \
             AND state = 'open' FOR UPDATE"
        ))
        .bind(key.catalog_id)
        .bind(key.key_id)
        .bind(key.label)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = found {
            return Ok(row.into());
        }
    }
    Err(OkfError::Busy {
        reason: "the changeset closed while this write opened it".to_string(),
    })
}

/// A changeset by id, in any state.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction.
/// * `id` - the changeset's id.
///
/// # Returns
/// The row, or `None` if there is none.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn by_id<'e>(
    executor: impl PgExecutor<'e>,
    id: i64,
) -> Result<Option<ChangesetRow>, OkfError> {
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "SELECT ",
        changeset_columns!(),
        " FROM changesets WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(ChangesetRow::from))
}

/// A changeset's entries.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction.
/// * `changeset_id` - the changeset.
///
/// # Returns
/// The entries, by path (the primary key's order, so no sort).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn entries<'e>(
    executor: impl PgExecutor<'e>,
    changeset_id: i64,
) -> Result<Vec<EntryRow>, OkfError> {
    let rows = sqlx::query_as::<_, EntryRow>(
        "SELECT path, base_revision, content, executable, replay FROM changeset_entries \
         WHERE changeset_id = $1 ORDER BY path",
    )
    .bind(changeset_id)
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// Bumps an open changeset's `version` and `updated_at`, taking its row
/// lock: step 2 of the lock order, before any entry is written.
///
/// # Errors
/// [`OkfError::Busy`] if it is not open (any more); [`OkfError::Database`].
async fn touch(
    conn: &mut PgConnection,
    changeset_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let touched = sqlx::query(
        "UPDATE changesets SET version = version + 1, updated_at = $2 \
         WHERE id = $1 AND state = 'open'",
    )
    .bind(changeset_id)
    .bind(now)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if touched == 0 {
        return Err(no_longer_open(changeset_id));
    }
    Ok(())
}

/// Stores an entry, replacing the changeset's entry for the same path, and
/// bumps the changeset's `version`.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `changeset_id` - an open changeset.
/// * `entry` - the path, base revision, content, execute bit and replay.
/// * `now` - the time (server clock).
///
/// # Errors
/// [`OkfError::Busy`] if the changeset is no longer open (a concurrent
/// publish, discard or prune closed it; a retry opens a new one);
/// [`OkfError::Database`].
pub async fn upsert_entry(
    conn: &mut PgConnection,
    changeset_id: i64,
    entry: &EntryRow,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    touch(conn, changeset_id, now).await?;
    sqlx::query(
        "INSERT INTO changeset_entries \
         (changeset_id, path, base_revision, content, executable, replay) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (changeset_id, path) DO UPDATE SET \
         base_revision = EXCLUDED.base_revision, content = EXCLUDED.content, \
         executable = EXCLUDED.executable, replay = EXCLUDED.replay",
    )
    .bind(changeset_id)
    .bind(&entry.path)
    .bind(entry.base_revision)
    // `as_deref`: `&Option<Vec<u8>>` to `Option<&[u8]>`, binding without a copy.
    .bind(entry.content.as_deref())
    .bind(entry.executable)
    .bind(&entry.replay)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Drops entries and bumps the changeset's `version`.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `changeset_id` - an open changeset.
/// * `paths` - the paths to drop; `None` drops every entry.
/// * `now` - the time (server clock).
///
/// # Returns
/// How many entries were dropped (a named path with no entry counts 0).
///
/// # Errors
/// [`OkfError::Busy`] if the changeset is no longer open;
/// [`OkfError::Database`].
pub async fn remove_entries(
    conn: &mut PgConnection,
    changeset_id: i64,
    paths: Option<&[String]>,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    touch(conn, changeset_id, now).await?;
    // `$2::text[] IS NULL`: a NULL array (`None`) matches every path.
    let removed = sqlx::query(
        "DELETE FROM changeset_entries WHERE changeset_id = $1 \
         AND ($2::text[] IS NULL OR path = ANY($2))",
    )
    .bind(changeset_id)
    .bind(paths)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(removed)
}

/// Closes an open changeset (publish or discard), bumping its `version`.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `changeset_id` - the changeset.
/// * `state` - the new state.
/// * `now` - the time (server clock).
///
/// # Errors
/// [`OkfError::Refused`] if it is not open; [`OkfError::Database`].
pub async fn set_state(
    conn: &mut PgConnection,
    changeset_id: i64,
    state: ChangesetState,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let changed = sqlx::query(
        "UPDATE changesets SET state = $2, version = version + 1, updated_at = $3 \
         WHERE id = $1 AND state = 'open'",
    )
    .bind(changeset_id)
    .bind(state.as_str())
    .bind(now)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(OkfError::Refused {
            reason: format!("changeset {changeset_id} is not open"),
        });
    }
    Ok(())
}

/// Every open changeset with entries whose last change is before
/// `idle_since`, across catalogs: what `stranded_sessions` filters by key.
///
/// # Arguments
/// * `pool` - the database.
/// * `idle_since` - the cutoff (now less `session_idle_timeout`).
///
/// # Returns
/// The changesets with their catalog's name and sorted paths, by catalog
/// name, then oldest change first.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn stranded(
    pool: &PgPool,
    idle_since: DateTime<Utc>,
) -> Result<Vec<StrandedRow>, OkfError> {
    // The inner join to the entries keeps only non-empty changesets;
    // grouping by the primary key `c.id` lets the other `c` columns be
    // selected without listing them in `GROUP BY`.
    let rows = sqlx::query_as::<_, RawStranded>(
        "SELECT c.id, c.catalog_id, c.label, c.key_id, c.actor, c.state, c.version, \
         c.created_at, c.updated_at, k.name AS catalog, \
         array_agg(e.path ORDER BY e.path) AS paths \
         FROM changesets c \
         JOIN catalogs k ON k.id = c.catalog_id \
         JOIN changeset_entries e ON e.changeset_id = c.id \
         WHERE c.state = 'open' AND c.updated_at < $1 \
         GROUP BY c.id, k.name \
         ORDER BY k.name, c.updated_at, c.id",
    )
    .bind(idle_since)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|raw| StrandedRow {
            changeset: raw.changeset.into(),
            catalog: raw.catalog,
            paths: raw.paths,
        })
        .collect())
}

/// Gives an open changeset a new key and label (`adopt_changeset`); the
/// caller has checked the calling key may.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `id` - the changeset.
/// * `key_id` - the adopting key.
/// * `label` - the adopting connection's label.
/// * `now` - the time (server clock).
///
/// # Returns
/// The changeset as adopted (its `version` bumped).
///
/// # Errors
/// [`OkfError::Refused`] if it is not open, or if that key and label already
/// have an open changeset in the catalog (the transaction is then aborted:
/// drop it); [`OkfError::Database`].
pub async fn adopt(
    conn: &mut PgConnection,
    id: i64,
    key_id: &str,
    label: &str,
    now: DateTime<Utc>,
) -> Result<ChangesetRow, OkfError> {
    let adopted = sqlx::query_as::<_, RawChangeset>(concat!(
        "UPDATE changesets SET key_id = $2, label = $3, version = version + 1, \
         updated_at = $4 WHERE id = $1 AND state = 'open' RETURNING ",
        changeset_columns!()
    ))
    .bind(id)
    .bind(key_id)
    .bind(label)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await;
    match adopted {
        Ok(Some(row)) => Ok(row.into()),
        Ok(None) => Err(OkfError::Refused {
            reason: format!("changeset {id} is not open"),
        }),
        Err(error) if is_unique_violation(&error) => Err(OkfError::Refused {
            reason: "this connection already has an open changeset in that catalog; publish \
                     or discard it first, or connect with another X-OKF-Changeset label"
                .to_string(),
        }),
        Err(error) => Err(error.into()),
    }
}

/// Discards every open changeset without entries whose last change is
/// before `older_than` (design "Session lifecycle": the 24-hour prune).
///
/// Changesets another transaction holds are skipped (`SKIP LOCKED`): a
/// writer holds its changeset's row from [`get_or_create`] on (the insert
/// locks a new row, the fallback read locks an existing one `FOR UPDATE`),
/// so a changeset is never discarded under a write, and the prune never
/// waits on a lock.
///
/// # Arguments
/// * `pool` - the database.
/// * `older_than` - the cutoff.
/// * `now` - the time (server clock), as their `updated_at`.
///
/// # Returns
/// How many were discarded.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune_empty(
    pool: &PgPool,
    older_than: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    let pruned = sqlx::query(
        "UPDATE changesets SET state = 'discarded', version = version + 1, updated_at = $2 \
         WHERE state = 'open' AND id IN (\
             SELECT c.id FROM changesets c \
             WHERE c.state = 'open' AND c.updated_at < $1 \
             AND NOT EXISTS (SELECT 1 FROM changeset_entries e WHERE e.changeset_id = c.id) \
             FOR UPDATE OF c SKIP LOCKED)",
    )
    .bind(older_than)
    .bind(now)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(pruned)
}
```

- [ ] **Step 7: `catalogs::delete` and the old column name**

In `src/store/catalogs.rs`:

1. Lock the open changesets in id order, so two multi-row lockers cannot take them in opposite orders:

```rust
    sqlx::query(
        "SELECT id FROM changesets WHERE catalog_id = $1 AND state = 'open' \
         ORDER BY id FOR UPDATE",
    )
```

2. Replace the last three sentences of `delete`'s doc ("A writer that holds an uncommitted entry and then needs the catalog row can deadlock with this (Postgres aborts one side); see the plan's note for M5.") with:

```rust
/// Writers take the catalog row (`FOR KEY SHARE`, `changesets::lock_catalog`)
/// before any changeset or entry, the same order as here, so a write and a
/// delete wait for each other rather than deadlock: a write that waited
/// finds the catalog gone.
```

3. In the tests `delete_refuses_unpublished_work_unless_forced`, `an_empty_open_changeset_does_not_block_delete` and `an_entry_written_during_a_delete_is_refused_not_cascaded`, rename `session_id` to `label` in each `INSERT INTO changesets` (`key_id` stays NULL there).

In `src/db.rs`, `constraints_hold`: the unique index now includes `key_id`, and NULLs never conflict, so the two open changesets need a key. Move the `INSERT INTO api_keys ... 'k1'` statement up to just before `let open = ...`, and change the insert and the asserted constraint:

```rust
        let open = "INSERT INTO changesets (catalog_id, label, key_id, actor, state, created_at, \
                    updated_at) VALUES ($1, 's1', 'k1', 'a', 'open', $2, $2)";
```

```rust
        assert_eq!(second.constraint(), Some("changesets_one_open_per_label"));
```

Grep for anything left: `grep -rnw session_id src tests; grep -rn one_open_per_session src tests` must print nothing (`-w` matches the whole word, so `session_idle_timeout` in `src/config.rs` and `src/http/mod.rs` does not count).

- [ ] **Step 8: Run the tests**

Run: `cargo test --lib -- store::changesets store::catalogs db:: error:: http::`
Expected: the 11 new `store::changesets` tests and `busy_is_503_and_says_to_retry` pass, with every older test in those modules. Then `cargo test`.

- [ ] **Step 9: Update the design**

In `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`:

1. "Decisions", the "Write grouping" row: "One changeset per (catalog, key, label), created on first write and kept in the database until published or discarded; explicit `publish`. The label is the connection's `X-OKF-Changeset` header, else `?changeset=`, else `default` (never the MCP session id, which changes on every reconnect)".
2. "Storage", the `changesets` row: "One open per (catalog, key, label): `id`, `catalog_id`, `label`, `key_id`, `actor`, `state` (`open`, `published`, `discarded`), `version` (bumped by every change to it or its entries; overlay caches key on it), `created_at`, `updated_at`". In the constraints paragraph: "`changesets` has a partial unique index on `(catalog_id, key_id, label) WHERE state = 'open'`, so a racing get-or-create cannot open two changesets for one identity". Add a paragraph after it:

   > **Lock order.** Every transaction that writes changesets or entries takes the catalog row `FOR KEY SHARE` first, then the changeset row (the `UPDATE` that bumps `version`), then entries. `catalog delete` takes the catalog's advisory publish lock (`catalogs::lock`), then the catalog row `FOR UPDATE`, then the open changesets in id order; publish (M5 Task 10) takes the in-process mutex, the same advisory lock, then the catalog row and the changeset. Plain writes never take the advisory lock, so no order is reversed: writes and deletes wait for each other instead of deadlocking; a write that waited finds the catalog gone and writes nothing. The 24-hour prune takes no catalog lock and skips changesets other transactions hold (`SKIP LOCKED`). A deadlock or serialization failure Postgres still reports (`40P01`, `40001`) is the `Busy` error, which says to retry.

3. "Session lifecycle": replace the three bullets with:

   > - A changeset is created on the first write by a (key, label) to a catalog (see "Write grouping"), and stays open until `publish` or `discard`, across sessions and restarts: closing an MCP session strands nothing, and a client on MCP `2026-07-28` (no sessions) gets the same changeset on every request through its label.
   > - `stranded_sessions` lists open, non-empty changesets not changed within `session_idle_timeout`: the key's own, or for an `admin` key every one in the catalogs it reaches. `adopt_changeset` moves one to the calling connection's key and label (the key's own with `write`, another key's with `admin`).
   > - The poll loop discards empty changesets whose last change is more than 24 hours old. Non-empty ones are never pruned: they hold unpublished work, and only `discard` (after `adopt_changeset`, if need be) or `catalog delete --force` removes them.

4. "Errors": add a row `| Busy | The database aborted the request to resolve a lock conflict, or the changeset closed under a write; nothing was stored; retry (HTTP 503) |`.
5. "Open questions for M5": delete the "Changeset identity without sessions" bullet, and from the "Carried from M3" bullet delete the deadlock rule clause (the lock order above answers it).

- [ ] **Step 10: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-01-changesets.md` (`<ts>` from `date -u +%Y-%m-%dT%H%M%SZ`): the migration; every `store::changesets` function and its locking; `Busy` and the driver-error conversion (`40P01`, `40001`); the `note_failure` arm; `delete`'s id-ordered changeset lock; the 11 store tests and the HTTP status test, naming the two concurrency tests; the design sections changed; and the Interfaces notes at the top of this task. Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock migrations/0002_changeset_identity.sql src/store/ src/db.rs \
    src/error.rs src/http/mod.rs src/app.rs src/cli.rs \
    docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "$(cat <<'EOF'
feat: changesets keyed by catalog, key and label, in lock order (M5.01)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `okf::ops`: the shared op types and the frontmatter ops

> Interfaces note (written while planning, 2026-11-03):
> - **Corrected (issue #91), 2026-11-03, review of PR #105:** `Doc::remove_lines` is `pub(crate)` (only `okf::ops` calls it, and it panics on a bad range), and a 14th frontmatter test, `unquestion_matches_okf_py_on_a_multi_line_flow_list_and_a_mapping_item`, pins `unquestion`'s two removal paths to okf.py's output.
> - `src/okf/ops/replay.rs` is created here with the `ReplayOp` enum only, because `OpOutcome::replay` needs the type; Task 3 adds the functions to that file. Task 3 also extends this task's parity script and shim command, so **Task 3 starts after Task 2** (the graph gains T2 -> T3).
> - `Doc::remove_lines(range)` is new in `src/okf/doc.rs`: `unquestion` must delete one list item's lines (`del doc.fm[s:e]` in `okf.py`), and `Doc::splice_lines` is private.
> - `okf.py`'s `die(msg)` in these commands becomes `OkfError::Refused { reason: msg }`, keeping `okf.py`'s text, except where an M1 variant already names the problem: `NotFound` (no such concept), `InvalidPath` (not a concept document), `FrontmatterParse` (frontmatter does not parse) and `Shape` (`open_questions` or `sources` is not a list). No new variant.
> - `StampField` and `LogKind` derive `Deserialize` (with `okf.py`'s spellings), so the parity test and Task 9's tool parameters parse them the same way.

`okf::ops` holds the edit logic every write tool runs: pure functions over a `Workspace` that read through it, write the files they change with `write_atomic`, and return an `OpOutcome` naming those files and, for bookkeeping edits, the replay operations that redo them at publish. This task adds the shared types and ports `okf.py`'s five concept commands (`okf.py:653-750`) byte for byte: `stamp`, `review`, `question`, `unquestion` and `source`. Each loads the concept the way `load_concept` does (refusing a document without frontmatter or whose frontmatter does not parse), edits the frontmatter textually through M1's `Doc`, checks that the result still parses (`Doc.save`), and returns `okf.py`'s printed line as the outcome's message. An op that changes nothing ("already queued") returns an outcome with no touched paths and `okf.py`'s message. A new `ops` command in the parity shim runs a JSON script of these commands through `okf.py`'s own `cmd_*` functions on a copy of the synthetic corpus, with `now_utc` frozen, and records every step's output and every changed file; a golden test and a live `--ignored` test compare the Rust ops against it.

**Files:**
- Create: `src/okf/ops/mod.rs`
- Create: `src/okf/ops/frontmatter.rs`
- Create: `src/okf/ops/replay.rs` (the `ReplayOp` type only; Task 3 adds the functions)
- Modify: `src/okf/mod.rs` (`pub mod ops;`)
- Modify: `src/okf/doc.rs` (`Doc::remove_lines`)
- Modify: `tests/parity/okf_shim.py` (the `ops` command)
- Create: `tests/parity/ops_script.json`
- Modify: `tests/parity.rs` (`ops_match_golden`, `live_ops_parity`, `regenerate_goldens`)
- Create: `tests/golden/ops.json` (generated)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Errors")
- Create: `changelog/<ts>-m5-task-02-frontmatter-ops.md`

- [ ] **Step 1: `Doc::remove_lines`**

In `src/okf/doc.rs`, add after `delete_block`:

```rust
    /// Removes the frontmatter lines in `range` (Python's `del fm[a:b]`):
    /// one list item located by [`Doc::seq_items`].
    ///
    /// # Arguments
    /// * `range` - indexes into the frontmatter lines, as a [`SeqItems`] span
    ///   of this document gives them.
    ///
    /// # Panics
    /// If `range` is out of bounds, which a span taken from this document's
    /// current state never is (like slicing a Python list out of range).
    pub(crate) fn remove_lines(&mut self, range: Range<usize>) {
        self.splice_lines(range, Vec::new());
    }
```

and this test in its `tests` module:

```rust
    #[test]
    fn remove_lines_drops_one_item_and_keeps_the_rest() {
        let mut d = doc("---\nk:\n  - a\n  - b\n  - c\n---\n");
        let items = d.seq_items("k").unwrap();

        d.remove_lines(items.spans[1].clone());

        assert_eq!(d.render(), "---\nk:\n  - a\n  - c\n---\n");
    }
```

- [ ] **Step 2: Write the failing tests**

Create `src/okf/ops/frontmatter.rs` with only the tests module first (the code goes above it in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};

    /// The frozen clock and actor every test uses.
    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn read(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn stamp_generated_replaces_the_block() {
        let ws = MemWorkspace::from_files([(
            "a.md",
            "---\ntype: Note\ngenerated: { by: old, at: 2026-01-01T00:00:00Z }\n---\nBody\n",
        )]);

        let outcome = stamp(&ws, &ctx(), "a", StampField::Generated).unwrap();

        assert_eq!(
            read(&ws, "a.md"),
            "---\ntype: Note\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\n---\nBody\n"
        );
        assert_eq!(outcome.touched, ["a.md"]);
        assert!(outcome.replay.is_empty());
        assert_eq!(
            outcome.message,
            "a.md: generated { by: tester, at: 2026-11-03T12:00:00Z }"
        );
    }

    #[test]
    fn stamp_verified_appends_after_the_last_item() {
        let ws = MemWorkspace::from_files([(
            "a.md",
            "---\ntype: Note\nverified:\n  - by: a\n    at: 2026-01-01T00:00:00Z\n---\n",
        )]);

        stamp(&ws, &ctx(), "a.md", StampField::Verified).unwrap();

        assert_eq!(
            read(&ws, "a.md"),
            concat!(
                "---\ntype: Note\nverified:\n  - by: a\n    at: 2026-01-01T00:00:00Z\n",
                "  - { by: tester, at: 2026-11-03T12:00:00Z }\n---\n",
            )
        );
    }

    #[test]
    fn stamp_keeps_crlf_line_endings() {
        let ws = MemWorkspace::from_files([("c.md", "---\r\ntype: Note\r\n---\r\nBody\r\n")]);

        stamp(&ws, &ctx(), "c", StampField::Generated).unwrap();

        assert_eq!(
            read(&ws, "c.md"),
            "---\r\ntype: Note\r\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\r\n---\r\nBody\r\n"
        );
    }

    #[test]
    fn ops_refuse_missing_or_broken_frontmatter_and_non_concepts() {
        let ws = MemWorkspace::from_files([
            ("n.md", "Text only\n"),
            ("b.md", "---\ntype: Note\ntype: again\n---\n"),
            ("log.md", "# Log\n"),
        ]);

        let none = stamp(&ws, &ctx(), "n", StampField::Generated).unwrap_err();
        let broken = question(&ws, &ctx(), "b", "Why?").unwrap_err();
        let missing = stamp(&ws, &ctx(), "nope", StampField::Generated).unwrap_err();
        let log = stamp(&ws, &ctx(), "log.md", StampField::Generated).unwrap_err();

        assert_eq!(
            refusal(none),
            "n.md has no frontmatter; add it with your editor first"
        );
        assert!(matches!(broken, OkfError::FrontmatterParse { line: 3, .. }));
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(log, OkfError::InvalidPath { .. }));
        assert_eq!(read(&ws, "n.md"), "Text only\n");
    }

    #[test]
    fn review_replaces_reviewed_and_queues_only_new_questions() {
        let ws = MemWorkspace::from_files([(
            "q.md",
            "---\ntype: Note\nreviewed: { by: x, at: y, outcome: verified }\nopen_questions:\n  - Old?\n---\n",
        )]);
        let questions = ["Old?", "New?", "New?"].map(String::from);

        let outcome = review(&ws, &ctx(), "q", "partial", "Checked: half", &questions).unwrap();

        assert_eq!(
            read(&ws, "q.md"),
            concat!(
                "---\ntype: Note\nreviewed:\n  by: tester\n  at: 2026-11-03T12:00:00Z\n",
                "  outcome: partial\n  note: \"Checked: half\"\n",
                "open_questions:\n  - Old?\n  - \"New?\"\n---\n",
            )
        );
        assert_eq!(
            outcome.message,
            concat!(
                "q.md: open question already queued: Old?\n",
                "q.md: open question already queued: New?\n",
                "q.md: reviewed partial at 2026-11-03T12:00:00Z; queued 1 question(s)",
            )
        );
    }

    #[test]
    fn review_rejects_an_unknown_outcome() {
        let ws = MemWorkspace::from_files([("q.md", "---\ntype: Note\n---\n")]);

        let error = review(&ws, &ctx(), "q", "maybe", "n", &[]).unwrap_err();

        assert_eq!(
            refusal(error),
            "invalid outcome 'maybe': choose from verified, updated, partial, unverifiable"
        );
    }

    #[test]
    fn question_already_queued_changes_nothing() {
        let text = "---\ntype: Note\nopen_questions:\n  - Old?\n---\n";
        let ws = MemWorkspace::from_files([("q.md", text)]);

        let outcome = question(&ws, &ctx(), "q", "Old?").unwrap();

        assert!(outcome.touched.is_empty());
        assert_eq!(outcome.message, "q.md: open question already queued: Old?");
        assert_eq!(read(&ws, "q.md"), text);
    }

    #[test]
    fn question_converts_a_flow_list_and_refuses_a_scalar() {
        let ws = MemWorkspace::from_files([
            ("f.md", "---\ntype: Note\nopen_questions: [a, \"b, c\"]\n---\n"),
            ("s.md", "---\ntype: Note\nopen_questions: just text\n---\n"),
        ]);

        let outcome = question(&ws, &ctx(), "f", "d").unwrap();
        let scalar = question(&ws, &ctx(), "s", "d").unwrap_err();

        assert_eq!(
            read(&ws, "f.md"),
            "---\ntype: Note\nopen_questions:\n  - \"a\"\n  - \"b, c\"\n  - \"d\"\n---\n"
        );
        assert_eq!(outcome.message, "f.md: queued open question: d");
        assert!(matches!(scalar, OkfError::Shape { ref key, .. } if key == "open_questions"));
    }

    #[test]
    fn unquestion_by_index_then_text_then_the_last_one() {
        let ws = MemWorkspace::from_files([(
            "u.md",
            "---\ntype: Note\nopen_questions:\n  - First?\n  - Second?\n  - Third?\n---\n",
        )]);

        let by_index = unquestion(&ws, &ctx(), "u", "2").unwrap();
        let after_index = read(&ws, "u.md");
        let by_text = unquestion(&ws, &ctx(), "u", "Third?").unwrap();
        let last = unquestion(&ws, &ctx(), "u", "1").unwrap();

        assert_eq!(by_index.message, "u.md: removed open question: Second?");
        assert_eq!(
            after_index,
            "---\ntype: Note\nopen_questions:\n  - First?\n  - Third?\n---\n"
        );
        assert_eq!(by_text.message, "u.md: removed open question: Third?");
        assert_eq!(
            last.message,
            "u.md: removed open question: First? (key deleted, none left)"
        );
        assert_eq!(read(&ws, "u.md"), "---\ntype: Note\n---\n");
    }

    #[test]
    fn unquestion_rewrites_a_flow_list_and_prints_a_mapping_as_python_does() {
        let ws = MemWorkspace::from_files([(
            "f.md",
            "---\ntype: Note\nopen_questions: [a, { k: v }, \"b, c\"]\n---\n",
        )]);

        let outcome = unquestion(&ws, &ctx(), "f", "2").unwrap();

        assert_eq!(outcome.message, "f.md: removed open question: {'k': 'v'}");
        assert_eq!(
            read(&ws, "f.md"),
            "---\ntype: Note\nopen_questions:\n  - \"a\"\n  - \"b, c\"\n---\n"
        );
    }

    #[test]
    fn unquestion_matches_okf_py_on_a_multi_line_flow_list_and_a_mapping_item() {
        // Expected text is okf.py's output for the same files and steps.
        let ws = MemWorkspace::from_files([
            ("m.md", "---\ntype: Note\nopen_questions: [a,\n  { k: v }, c]\n---\n"),
            (
                "b.md",
                "---\ntype: Note\nopen_questions:\n  - k: v\n    j: w\n  # keep\n  - b\n  - c\n---\n",
            ),
        ]);

        let flow = unquestion(&ws, &ctx(), "m", "3").unwrap();
        let middle = unquestion(&ws, &ctx(), "b", "2").unwrap();
        let mapping = unquestion(&ws, &ctx(), "b", "1").unwrap();

        assert_eq!(flow.message, "m.md: removed open question: c");
        assert_eq!(
            read(&ws, "m.md"),
            "---\ntype: Note\nopen_questions:\n  - \"a\"\n  - \"{'k': 'v'}\"\n---\n"
        );
        assert_eq!(middle.message, "b.md: removed open question: b");
        assert_eq!(
            mapping.message,
            "b.md: removed open question: {'k': 'v', 'j': 'w'}"
        );
        assert_eq!(
            read(&ws, "b.md"),
            "---\ntype: Note\nopen_questions:\n  # keep\n  - c\n---\n"
        );
    }

    #[test]
    fn unquestion_refusals_use_okf_py_wording() {
        let ws = MemWorkspace::from_files([
            ("u.md", "---\ntype: Note\nopen_questions:\n  - A?\n  - B?\n---\n"),
            ("e.md", "---\ntype: Note\n---\n"),
        ]);

        let high = unquestion(&ws, &ctx(), "u", "3").unwrap_err();
        let zero = unquestion(&ws, &ctx(), "u", "0").unwrap_err();
        let text = unquestion(&ws, &ctx(), "u", "Nope").unwrap_err();
        let none = unquestion(&ws, &ctx(), "e", "1").unwrap_err();

        assert_eq!(
            refusal(high),
            "u.md has 2 open question(s); index 3 is out of range (1-based)"
        );
        assert_eq!(
            refusal(zero),
            "u.md has 2 open question(s); index 0 is out of range (1-based)"
        );
        assert_eq!(
            refusal(text),
            "no open question in u.md exactly matches 'Nope'"
        );
        assert_eq!(refusal(none), "e.md has no open_questions");
    }

    #[test]
    fn source_appends_a_block_item_and_skips_an_empty_title() {
        let ws = MemWorkspace::from_files([(
            "s.md",
            "---\ntype: Note\nsources:\n  - id: one\n    resource: r1\n---\n",
        )]);
        let entry = SourceEntry {
            id: "two",
            resource: "a: b",
            title: Some(""),
            author: Some("human:pgrace"),
        };

        let outcome = source(&ws, &ctx(), "s", &entry).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            concat!(
                "---\ntype: Note\nsources:\n  - id: one\n    resource: r1\n",
                "  - id: two\n    resource: \"a: b\"\n    author: human:pgrace\n---\n",
            )
        );
        assert_eq!(outcome.message, "s.md: added source two");
    }

    #[test]
    fn source_refuses_a_duplicate_id_and_a_scalar() {
        let ws = MemWorkspace::from_files([
            ("s.md", "---\ntype: Note\nsources:\n  - { id: one }\n---\n"),
            ("x.md", "---\ntype: Note\nsources: nope\n---\n"),
        ]);
        let entry = SourceEntry {
            id: "one",
            resource: "r",
            title: None,
            author: None,
        };

        let duplicate = source(&ws, &ctx(), "s", &entry).unwrap_err();
        let scalar = source(&ws, &ctx(), "x", &entry).unwrap_err();

        assert_eq!(refusal(duplicate), "s.md already has a source with id 'one'");
        assert!(matches!(scalar, OkfError::Shape { ref key, .. } if key == "sources"));
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib -- okf::ops okf::doc::tests::remove_lines`
Expected: compile errors (`ops` is not a module of `okf`; `stamp`, `OpContext` and the rest not found). The `remove_lines` test needs Step 4's module line to compile too.

- [ ] **Step 4: Implement**

Add `pub mod ops;` to `src/okf/mod.rs` (alphabetical, after `markdown`).

Create `src/okf/ops/replay.rs` with the type only:

```rust
//! Replay operations: how a bookkeeping edit of `log.md` or an `index.md`
//! is redone on newer text at publish (design "Data flow", "Write" step 3).
//! Task 3 adds the functions that apply them.

use serde::{Deserialize, Serialize};

/// One replayable bookkeeping edit, stored in a changeset entry's `replay`
/// column as JSON (`{"op": "log", "date": ..., "bullet": ...}`).
///
/// `#[serde(tag = "op")]` writes the variant name into an `op` field next
/// to the variant's own fields, like a Python dict with a "type" key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReplayOp {
    /// `log`: add `bullet` under `## <date>` in `log.md`.
    Log {
        /// The UTC date of the heading, `2026-11-03`.
        date: String,
        /// The whole bullet line, `* **Update:** text`.
        bullet: String,
    },
    /// `index` and folder listings: add `bullet` to an `index.md` unless it
    /// already links `target`.
    ListBullet {
        /// Catalog-relative path the bullet lists (a concept, or a folder's
        /// `index.md`).
        target: String,
        /// The whole bullet line.
        bullet: String,
        /// The heading to put it under; `None` for `okf.py`'s default place.
        section: Option<String>,
        /// Create the heading when it is missing (folder listings under `# Sections`).
        create_section: bool,
    },
    /// `ensure_index`: create a folder's `index.md` unless it exists.
    CreateIndex {
        /// Catalog-relative folder, `infra/hosts`.
        folder: String,
    },
}
```

Create `src/okf/ops/mod.rs`:

> **Corrected (issue #91), 2026-11-03.** `OpOutcome::record` is not added here: nothing in Task 2 calls it, and an unused `pub(crate)` method fails `clippy -D warnings` (`dead_code`). Task 3, its first caller, adds it (Task 3, Step 3). The code below is what shipped.

```rust
//! Edit operations on a catalog: Rust ports of `okf.py`'s concept commands
//! (`stamp`, `review`, `question`, `unquestion`, `source`), its bookkeeping
//! (`log`, `index`, Task 3) and the server's content edits (Task 4).
//!
//! Every op is a pure function over a [`Workspace`]: it reads through it,
//! writes the files it changes with [`Workspace::write_atomic`], and returns
//! an [`OpOutcome`] naming them. The write path (`crate::write`, Task 8) runs
//! an op on a copy of a changeset's overlay and stores what it touched;
//! nothing here knows about changesets or the database.
//!
//! Byte parity with `okf.py` is the point: messages, rendered lines and
//! refusals are `okf.py`'s, checked by `tests/parity.rs` (`ops.json`).
//! `okf.py`'s `die(msg)` becomes [`OkfError::Refused`] with `msg` as its
//! reason, except where an M1 variant already names the problem
//! ([`OkfError::NotFound`], [`OkfError::InvalidPath`],
//! [`OkfError::FrontmatterParse`], [`OkfError::Shape`]).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::Deserialize;

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::okf::doc::Doc;
use crate::okf::paths::resolve_concept;
use crate::okf::workspace::Workspace;
use crate::okf::yaml::Value;

pub mod frontmatter;
pub mod replay;

// `pub use` re-exports names from the submodules, so callers write
// `okf::ops::stamp` (like a package `__init__.py` importing from its modules).
pub use frontmatter::{question, review, source, stamp, unquestion, OUTCOMES};
pub use replay::ReplayOp;

/// What an op needs from its caller besides the catalog.
///
/// The `'a` lifetime says the struct borrows `by` and cannot outlive the
/// string it points at (Rust checks this at compile time; Python has no
/// equivalent because everything is reference-counted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpContext<'a> {
    /// The server clock's "now"; every timestamp the op writes is this instant.
    pub now: DateTime<Utc>,
    /// The actor recorded in `by:` fields (the key's `actor`, or the call's `by`).
    pub by: &'a str,
}

/// What an op did. A plain record, so its fields are public.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpOutcome {
    /// Catalog-relative paths written, in order, without duplicates. Empty
    /// when the op changed nothing.
    pub touched: Vec<String>,
    /// Per touched path, the operations that redo the edit on newer text
    /// (Task 3). A touched path missing here is a free edit.
    pub replay: BTreeMap<String, Vec<ReplayOp>>,
    /// The line `okf.py` prints (several lines joined with `\n` for `review`).
    pub message: String,
    /// What `okf.py` prints to stderr, without its `okf: ` prefix.
    pub notes: Vec<String>,
}

impl OpOutcome {
    /// An outcome that changed nothing, carrying `okf.py`'s message.
    pub(crate) fn unchanged(message: String) -> Self {
        // `..Self::default()` fills the other fields with their defaults.
        Self {
            message,
            ..Self::default()
        }
    }

    /// Records that `path` was written (once; later calls are no-ops).
    pub(crate) fn touch(&mut self, path: &str) {
        if !self.touched.iter().any(|p| p == path) {
            self.touched.push(path.to_string());
        }
    }
}

/// Which stamp `stamp` writes.
///
/// `Copy` makes it a plain value (passing it copies it, like a Python int).
/// `Deserialize` reads `"generated"` / `"verified"`, `okf.py`'s spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StampField {
    /// Replace `generated`.
    Generated,
    /// Append to `verified`.
    Verified,
}

impl StampField {
    /// The key's name, as `okf.py` prints it.
    pub fn as_str(self) -> &'static str {
        match self {
            StampField::Generated => "generated",
            StampField::Verified => "verified",
        }
    }
}

/// One `sources` entry for [`source`]. `title` and `author` are left out
/// when `None` or empty (`okf.py`'s `if a.title:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceEntry<'a> {
    /// The entry's id, unique within the concept.
    pub id: &'a str,
    /// What was consulted: a URI, a path or a description.
    pub resource: &'a str,
    /// Optional title.
    pub title: Option<&'a str>,
    /// Optional author, e.g. `human:pgrace`.
    pub author: Option<&'a str>,
}

/// The kind of a `log.md` entry (`okf.py`'s `LOG_KINDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum LogKind {
    /// `Update` (the default in `okf.py`).
    Update,
    /// `Creation`.
    Creation,
    /// `Change`.
    Change,
    /// `Import`.
    Import,
}

impl LogKind {
    /// The kind as written in the bullet, `Update` etc.
    pub fn as_str(self) -> &'static str {
        match self {
            LogKind::Update => "Update",
            LogKind::Creation => "Creation",
            LogKind::Change => "Change",
            LogKind::Import => "Import",
        }
    }
}

/// A concept loaded for editing (`okf.py`'s `load_concept` result).
pub(crate) struct LoadedConcept {
    /// Catalog-relative path.
    pub(crate) path: String,
    /// The document, ready for textual edits.
    pub(crate) doc: Doc,
    /// The frontmatter as parsed before any edit.
    pub(crate) data: IndexMap<String, Value>,
}

/// `okf.py`'s `load_concept`: resolves `reference`, reads the file byte for
/// byte and parses its frontmatter.
///
/// # Errors
/// [`OkfError::NotFound`] / [`OkfError::InvalidPath`] from
/// [`resolve_concept`]; [`OkfError::Refused`] without frontmatter;
/// [`OkfError::FrontmatterParse`] when it does not parse.
pub(crate) fn load_concept(
    workspace: &dyn Workspace,
    reference: &str,
) -> Result<LoadedConcept, OkfError> {
    let path = resolve_concept(workspace, reference)?;
    let doc = Doc::from_text(&path, &workspace.read_to_string(&path)?);
    if !doc.has_frontmatter() {
        return Err(refused(format!(
            "{path} has no frontmatter; add it with your editor first"
        )));
    }
    let data = doc.parse_checked()?;
    Ok(LoadedConcept { path, doc, data })
}

/// `okf.py`'s `Doc.save`: refuses frontmatter an edit broke, then writes.
///
/// # Errors
/// [`OkfError::FrontmatterParse`] (nothing is written), or the workspace's
/// write error.
pub(crate) fn save(
    workspace: &dyn Workspace,
    doc: &Doc,
    outcome: &mut OpOutcome,
) -> Result<(), OkfError> {
    let text = doc.render_checked()?;
    workspace.write_atomic(doc.path(), &text)?;
    outcome.touch(doc.path());
    Ok(())
}

/// `{ by, at }` for a stamp: the actor and the frozen "now".
pub(crate) fn stamp_value(ctx: &OpContext<'_>) -> Value {
    Value::Map(IndexMap::from([
        ("by".to_string(), Value::Str(ctx.by.to_string())),
        ("at".to_string(), Value::Str(format_timestamp(ctx.now))),
    ]))
}

/// `okf.py`'s `die(reason)` as an error.
pub(crate) fn refused(reason: String) -> OkfError {
    OkfError::Refused { reason }
}
```

Put this above the tests in `src/okf/ops/frontmatter.rs`:

```rust
//! The concept commands `stamp`, `review`, `question`, `unquestion` and
//! `source` (`okf.py:653-750`): textual edits of one concept's frontmatter.
//!
//! Divergences from `okf.py` (none reachable from the parity script):
//! - `unquestion` reads only ASCII digits as an index; Python's `isdigit`
//!   also accepts other Unicode digits;
//! - `review` refuses an outcome outside [`OUTCOMES`] itself (argparse
//!   refuses it in `okf.py`).

use indexmap::IndexMap;

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::okf::ops::{
    load_concept, refused, save, stamp_value, LoadedConcept, OpContext, OpOutcome, SourceEntry,
    StampField,
};
use crate::okf::pystr::{py_repr_str, py_str};
use crate::okf::workspace::Workspace;
use crate::okf::yaml::{block_item, flow_value, scalar, ScalarStyle, Value};

/// `okf.py`'s `OUTCOMES`: what a review may conclude.
pub const OUTCOMES: [&str; 4] = ["verified", "updated", "partial", "unverifiable"];

const OPEN_QUESTIONS: &str = "open_questions";
const SOURCES: &str = "sources";

/// One always-quoted list item: `" " * ind + "- " + scalar(q, force=True)`.
fn quoted_item(text: &str, indent: usize) -> String {
    format!("{}- {}", " ".repeat(indent), scalar(text, ScalarStyle::Quoted))
}

/// `okf.py`'s `render=lambda v, ind: [... scalar(v, force=True)]`: an
/// existing item re-rendered when a flow list becomes a block list. Python's
/// `scalar` calls `str(v)` first, so a mapping item becomes its `repr` text.
fn render_quoted(value: &Value, indent: usize) -> Vec<String> {
    vec![quoted_item(&py_str(value), indent)]
}

/// A key's list items, refusing anything but a list or nothing
/// (`okf.py`'s "`<key>` is not a list; fix it by hand").
fn list_or_nothing<'d>(
    path: &str,
    data: &'d IndexMap<String, Value>,
    key: &str,
) -> Result<&'d [Value], OkfError> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Seq(items)) => Ok(items),
        Some(Value::Str(_) | Value::Map(_)) => Err(OkfError::Shape {
            path: path.to_string(),
            key: key.to_string(),
        }),
    }
}

/// Port of `cmd_stamp`: sets `generated`, or appends to `verified`, with
/// `{ by, at: now }`.
///
/// # Arguments
/// * `workspace` - the catalog (an overlay copy in the server).
/// * `ctx` - the frozen "now" and the actor.
/// * `concept` - the concept reference (`/dir/x`, `dir/x.md`).
/// * `field` - which stamp.
///
/// # Returns
/// The outcome; its message is `okf.py`'s, e.g.
/// `infra/x.md: generated { by: a, at: 2026-11-03T12:00:00Z }`.
///
/// # Errors
/// [`OkfError::NotFound`] when no concept matches `concept`;
/// [`OkfError::InvalidPath`] when it names a non-concept (`log.md`, an
/// `index.md`); [`OkfError::Refused`] when it has no frontmatter;
/// [`OkfError::FrontmatterParse`] when its frontmatter does not parse, or
/// the edit would break it; [`OkfError::Shape`] when `verified` is a scalar.
pub fn stamp(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    concept: &str,
    field: StampField,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept { path, mut doc, .. } = load_concept(workspace, concept)?;
    let item = flow_value(&stamp_value(ctx));
    // `match` must cover every variant: a third stamp field would not compile here.
    match field {
        StampField::Generated => doc.set_block("generated", vec![format!("generated: {item}")]),
        StampField::Verified => doc.append_seq_item(
            "verified",
            |indent| vec![format!("{}- {item}", " ".repeat(indent))],
            block_item,
        )?,
    }
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: {} {item}", field.as_str());
    Ok(outcome)
}

/// Port of `cmd_review`: replaces `reviewed` and queues open questions not
/// already queued.
///
/// # Arguments
/// * `workspace`, `ctx`, `concept` - as for [`stamp`].
/// * `outcome` - one of [`OUTCOMES`].
/// * `note` - what was and wasn't checked (always double-quoted).
/// * `questions` - open questions to queue; duplicates are reported, not added.
///
/// # Returns
/// The outcome; its message is one "already queued" line per duplicate,
/// then `<path>: reviewed <outcome> at <ts>[; queued N question(s)]`.
///
/// # Errors
/// [`OkfError::Refused`] for an unknown outcome; as [`stamp`] otherwise
/// ([`OkfError::Shape`] when `open_questions` is a scalar).
pub fn review(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    concept: &str,
    outcome: &str,
    note: &str,
    questions: &[String],
) -> Result<OpOutcome, OkfError> {
    if !OUTCOMES.contains(&outcome) {
        return Err(refused(format!(
            "invalid outcome {}: choose from {}",
            py_repr_str(outcome),
            OUTCOMES.join(", ")
        )));
    }
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let at = format_timestamp(ctx.now);
    doc.set_block(
        "reviewed",
        vec![
            "reviewed:".to_string(),
            format!("  by: {}", scalar(ctx.by, ScalarStyle::Block)),
            format!("  at: {at}"),
            format!("  outcome: {outcome}"),
            format!("  note: {}", scalar(note, ScalarStyle::Quoted)),
        ],
    );
    // A non-list `open_questions` counts as empty here, as in okf.py; the
    // append below then converts a mapping or refuses a scalar.
    let have: &[Value] = match data.get(OPEN_QUESTIONS) {
        Some(Value::Seq(items)) => items,
        None | Some(Value::Null | Value::Str(_) | Value::Map(_)) => &[],
    };
    let mut added: Vec<&str> = Vec::new();
    let mut lines = Vec::new();
    for question in questions {
        let queued = have.iter().any(|v| v.as_str() == Some(question.as_str()))
            || added.contains(&question.as_str());
        if queued {
            lines.push(format!("{path}: open question already queued: {question}"));
            continue;
        }
        doc.append_seq_item(
            OPEN_QUESTIONS,
            |indent| vec![quoted_item(question, indent)],
            render_quoted,
        )?;
        added.push(question);
    }
    let mut result = OpOutcome::default();
    save(workspace, &doc, &mut result)?;
    let mut summary = format!("{path}: reviewed {outcome} at {at}");
    if !added.is_empty() {
        summary.push_str(&format!("; queued {} question(s)", added.len()));
    }
    lines.push(summary);
    result.message = lines.join("\n");
    Ok(result)
}

/// Port of `cmd_question`: queues one open question without a review.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused (no
///   timestamp), kept so every concept op has the same shape.
/// * `question` - the question, stored double-quoted.
///
/// # Returns
/// The outcome; nothing is touched when the question is already queued.
///
/// # Errors
/// As [`stamp`] when loading the concept; [`OkfError::Shape`] when
/// `open_questions` is not a list.
pub fn question(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    question: &str,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let existing = list_or_nothing(&path, &data, OPEN_QUESTIONS)?;
    if existing.iter().any(|v| v.as_str() == Some(question)) {
        return Ok(OpOutcome::unchanged(format!(
            "{path}: open question already queued: {question}"
        )));
    }
    doc.append_seq_item(
        OPEN_QUESTIONS,
        |indent| vec![quoted_item(question, indent)],
        render_quoted,
    )?;
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: queued open question: {question}");
    Ok(outcome)
}

/// The 0-based index `unquestion` removes: a 1-based number, else the
/// item whose text is exactly `question`.
fn question_index(path: &str, questions: &[Value], question: &str) -> Result<usize, OkfError> {
    let is_number = !question.is_empty() && question.bytes().all(|b| b.is_ascii_digit());
    if is_number {
        // `parse` fails only on overflow here, and such an index is out of range anyway.
        return question
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=questions.len()).contains(n))
            .map(|n| n - 1)
            .ok_or_else(|| {
                refused(format!(
                    "{path} has {} open question(s); index {question} is out of range (1-based)",
                    questions.len()
                ))
            });
    }
    questions
        .iter()
        .position(|v| v.as_str() == Some(question))
        .ok_or_else(|| {
            refused(format!(
                "no open question in {path} exactly matches {}",
                py_repr_str(question)
            ))
        })
}

/// Port of `cmd_unquestion`: removes one open question, deleting the key
/// when none are left.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused.
/// * `question` - a 1-based index (as `oldest --open-questions` lists them)
///   or the question's exact text.
///
/// # Returns
/// The outcome; the message names the removed question (Python's `str()`
/// of it, so a mapping item prints as `{'k': 'v'}`).
///
/// # Errors
/// [`OkfError::Refused`] when there are no open questions, the index is out
/// of range, or no question matches; as [`stamp`] when loading the concept.
pub fn unquestion(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    question: &str,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let questions = match data.get(OPEN_QUESTIONS) {
        Some(Value::Seq(items)) if !items.is_empty() => items,
        None | Some(Value::Null | Value::Str(_) | Value::Seq(_) | Value::Map(_)) => {
            return Err(refused(format!("{path} has no open_questions")));
        }
    };
    let index = question_index(&path, questions, question)?;
    let removed = py_str(&questions[index]);
    let last = questions.len() == 1;
    if last {
        doc.delete_block(OPEN_QUESTIONS);
    } else {
        match doc.seq_items(OPEN_QUESTIONS) {
            Some(items) if items.spans.len() == questions.len() => {
                doc.remove_lines(items.spans[index].clone());
            }
            // A flow list, on one line or several, is rewritten as a block
            // list of the remaining items. okf.py takes this branch for any
            // list `seq_items` can't map item by item; probing okf.py found
            // no block list that parses and gets here (a continuation or
            // indent `seq_items` refuses also fails to parse), so in practice
            // it is flow lists only.
            _ => {
                let mut lines = vec![format!("{OPEN_QUESTIONS}:")];
                lines.extend(
                    questions
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != index)
                        .map(|(_, q)| quoted_item(&py_str(q), 2)),
                );
                doc.set_block(OPEN_QUESTIONS, lines);
            }
        }
    }
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    let left = if last { " (key deleted, none left)" } else { "" };
    outcome.message = format!("{path}: removed open question: {removed}{left}");
    Ok(outcome)
}

/// Port of `cmd_source`: appends an entry to `sources`, refusing a
/// duplicate id.
///
/// # Arguments
/// * `workspace`, `concept` - as for [`stamp`]; `_ctx` is unused.
/// * `entry` - the new entry.
///
/// # Returns
/// The outcome, message `<path>: added source <id>`.
///
/// # Errors
/// [`OkfError::Shape`] when `sources` is not a list; [`OkfError::Refused`]
/// when an entry with this id exists; as [`stamp`] when loading the concept.
pub fn source(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    entry: &SourceEntry<'_>,
) -> Result<OpOutcome, OkfError> {
    let LoadedConcept {
        path,
        mut doc,
        data,
    } = load_concept(workspace, concept)?;
    let existing = list_or_nothing(&path, &data, SOURCES)?;
    // `matches!(x, Pattern if guard)` is a one-line `match` returning a bool.
    let duplicate = existing.iter().any(|s| {
        matches!(s, Value::Map(map) if map.get("id").and_then(Value::as_str) == Some(entry.id))
    });
    if duplicate {
        return Err(refused(format!(
            "{path} already has a source with id {}",
            py_repr_str(entry.id)
        )));
    }
    let mut fields = IndexMap::new();
    fields.insert("id".to_string(), Value::Str(entry.id.to_string()));
    fields.insert("resource".to_string(), Value::Str(entry.resource.to_string()));
    for (key, value) in [("title", entry.title), ("author", entry.author)] {
        if let Some(text) = value.filter(|t| !t.is_empty()) {
            fields.insert(key.to_string(), Value::Str(text.to_string()));
        }
    }
    let item = Value::Map(fields);
    doc.append_seq_item(SOURCES, |indent| block_item(&item, indent), block_item)?;
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: added source {}", entry.id);
    Ok(outcome)
}
```

- [ ] **Step 5: Run the unit tests**

Run: `cargo test --lib -- okf::ops okf::doc::tests::remove_lines`
Expected: 15 passed (14 in `okf::ops::frontmatter`, 1 in `okf::doc`).

- [ ] **Step 6: The parity script and the shim's `ops` command**

Create `tests/parity/ops_script.json`. `setup` files are written into the copy of the corpus before anything runs, so the script can exercise shapes the shared corpus lacks without adding fixtures (which would change every other golden). Every `error` step is a refusal whose wording the port keeps (`Refused`); errors the port words differently (`NotFound`, `FrontmatterParse`, `Shape`) are unit-tested instead.

```json
{
  "setup": {
    "ops/flow.md": "---\ntype: Note\ntitle: Flow shapes\nverified: { by: x, at: 2026-01-01T00:00:00Z }\nopen_questions: [one, \"two, three\", { k: v }]\n---\nBody.\n"
  },
  "steps": [
    {"cmd": "stamp", "concept": "infra/time-server.md", "field": "generated", "by": "claude-code/claude-opus-5-5"},
    {"cmd": "stamp", "concept": "/infra/time-server", "field": "verified", "by": "human:pgrace"},
    {"cmd": "stamp", "concept": "analysis/stamps.md", "field": "verified"},
    {"cmd": "stamp", "concept": "analysis/hub.md", "field": "verified"},
    {"cmd": "stamp", "concept": "infra/hosts/crlf-host.md", "field": "generated"},
    {"cmd": "stamp", "concept": "infra/hosts/no-frontmatter.md", "field": "generated"},
    {"cmd": "stamp", "concept": "ops/flow.md", "field": "verified"},
    {"cmd": "unquestion", "concept": "ops/flow.md", "question": "3"},
    {"cmd": "question", "concept": "ops/flow.md", "question": "four"},
    {"cmd": "review", "concept": "ops/flow.md", "outcome": "verified", "note": "n", "question": ["one", "five"]},
    {"cmd": "review", "concept": "analysis/hub.md", "outcome": "partial", "note": "Links checked; anchors not.", "question": ["Is the hub still needed?", "Is the hub still needed?"]},
    {"cmd": "review", "concept": "infra/time-server.md", "outcome": "verified", "note": "Re-checked", "question": ["Is the antenna on the roof?", "Who owns it?"], "by": "human:pgrace"},
    {"cmd": "review", "concept": "analysis/offsets.md", "outcome": "updated", "note": "x: y"},
    {"cmd": "question", "concept": "analysis/stamps.md", "question": "Is the clock right?"},
    {"cmd": "question", "concept": "analysis/lead.md", "question": "Does it need splitting?"},
    {"cmd": "unquestion", "concept": "analysis/stamps.md", "question": "2"},
    {"cmd": "unquestion", "concept": "infra/time-server.md", "question": "Is the antenna on the roof?"},
    {"cmd": "unquestion", "concept": "infra/time-server.md", "question": "1"},
    {"cmd": "unquestion", "concept": "analysis/lead.md", "question": "5"},
    {"cmd": "unquestion", "concept": "analysis/hub.md", "question": "no such question"},
    {"cmd": "unquestion", "concept": "analysis/backlog.md", "question": "1"},
    {"cmd": "source", "concept": "analysis/lead.md", "id": "s6", "resource": "f", "title": "Six", "author": "human:pgrace"},
    {"cmd": "source", "concept": "analysis/lead.md", "id": "s1", "resource": "again"},
    {"cmd": "source", "concept": "analysis/hub.md", "id": "web", "resource": "https://example.com/x", "title": ""},
    {"cmd": "source", "concept": "analysis/stamps.md", "id": "d", "resource": "r"},
    {"cmd": "source", "concept": "yaml/same-indent-lists.md", "id": "q", "resource": "r: s"}
  ]
}
```

What the steps cover: a replaced `generated` (with and without CRLF, and a `...` closing delimiter); `verified` appended to a mixed block list, to a list of flow items, created at its `KEY_ORDER` place, and converted from a bare flow mapping; `unquestion` by index on a flow list (rewritten as a block list, the mapping item printed as Python's `str`), by index on a block list, by text, and of the last question; `question` and `review` converting a flow list with quoted items, and reporting duplicates (already present, and repeated within one call); the four `unquestion`/`source` refusals and "has no frontmatter"; `source` with an empty title (left out), an absent `sources` key, flow-item lists, and a zero-indent list.

In `tests/parity/okf_shim.py`, add to the usage docstring after `analysis`:

```
    okf_shim.py ops ROOT NOW         stdin: an ops script ({"setup": {path: text}, "steps": [...]});
                                     runs each step through okf.py's cmd_* functions on a temp copy
                                     of ROOT (plus setup) with now_utc frozen at NOW; stdout: each
                                     step's output or error, and every file that changed
```

add `argparse`, `shutil` and `tempfile` to the imports (alphabetical) and `Any` to the `typing` import, and add after `analysis`:

```python
# argparse defaults of the commands the ops script may run (okf.py's `main`);
# a step's own fields override them. `by` defaults to "fixture" here, not
# okf.py's DEFAULT_ACTOR (which follows $OKF_ACTOR), so goldens are stable.
OPS_DEFAULTS: dict[str, dict[str, Json]] = {
    "stamp": {"by": "fixture"},
    "review": {"question": None, "by": "fixture"},
    "question": {},
    "unquestion": {},
    "source": {"title": None, "author": None},
}


def tree_bytes(root: str) -> dict[str, bytes]:
    """Every file under root, by root-relative path."""
    out: dict[str, bytes] = {}
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            full = os.path.join(dirpath, name)
            with open(full, "rb") as fh:
                out[os.path.relpath(full, root)] = fh.read()
    return out


def run_op(okf: ModuleType, work: str, step: dict[str, Any]) -> Json:
    """Run one script step as okf.py's CLI would, returning its output or error.

    okf.py's `die` prints `okf: <msg>` to stderr and exits; that becomes
    {"error": msg}. Otherwise stdout is the output, and stderr lines (okf.py's
    notes) are kept without their `okf: ` prefix. Paths under the temp copy
    are made catalog-relative, so the result does not depend on where it ran.
    """
    command = str(step["cmd"])
    fields = {**OPS_DEFAULTS[command], **{k: v for k, v in step.items() if k != "cmd"}}
    out, err = io.StringIO(), io.StringIO()
    failed = False
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            getattr(okf, f"cmd_{command}")(argparse.Namespace(root=work, **fields))
        except SystemExit:
            failed = True
    prefix = work + os.sep
    stdout = out.getvalue().replace(prefix, "")
    stderr = err.getvalue().replace(prefix, "")
    if failed:
        return {"error": stderr.removeprefix("okf: ").rstrip("\n")}
    entry: dict[str, Json] = {"output": stdout.rstrip("\n")}
    notes: list[Json] = [line.removeprefix("okf: ") for line in stderr.splitlines()]
    if notes:
        entry["notes"] = notes
    return entry


def ops(okf: ModuleType, root: str, now: str, script: dict[str, Any]) -> Json:
    """Run an ops script on a temp copy of root with now_utc frozen at now.

    `now_stamp` and `cmd_log` read `now_utc` as a module global at call
    time, so replacing it freezes every timestamp the commands write. The
    working directory is the copy while the steps run, because
    `resolve_concept` also tries its argument relative to the cwd.
    """
    fixed = okf.parse_ts(now)
    setattr(okf, "now_utc", lambda: fixed)
    home = os.getcwd()
    with tempfile.TemporaryDirectory() as tmp:
        work = os.path.join(tmp, "catalog")
        shutil.copytree(root, work, symlinks=True)
        for rel, text in script["setup"].items():
            full = os.path.join(work, rel)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w", encoding="utf-8", newline="") as fh:
                fh.write(text)
        before = tree_bytes(work)
        os.chdir(work)
        try:
            steps: list[Json] = [run_op(okf, work, step) for step in script["steps"]]
        finally:
            os.chdir(home)
        after = tree_bytes(work)
    changed = sorted(p for p in before.keys() | after.keys() if before.get(p) != after.get(p))
    files: dict[str, Json] = {p: after[p].decode("utf-8") if p in after else None for p in changed}
    return {"steps": steps, "files": files}
```

and in `main`, before the final `else`:

```python
    elif command == "ops":
        if len(args) != 2:
            usage("ops takes ROOT NOW (and the script on stdin)")
        script = json.load(sys.stdin)
        if not isinstance(script, dict):
            usage("ops reads a JSON object on stdin")
        result = ops(load_okf(), args[0], args[1], script)
```

- [ ] **Step 7: The Rust side of the parity test**

In `tests/parity.rs`, add to the module docs, after the paragraph on the analyses:

```rust
//! The ops (`ops.json`) run `tests/parity/ops_script.json` through
//! `okf_mcp::okf::ops` on an in-memory copy of the corpus and through
//! okf.py's `cmd_*` functions on a temp copy, both with "now" frozen at
//! [`FIXED_NOW`], and compare every step's output or error and every file
//! that changed, byte for byte. The script's error steps are refusals the
//! port words as okf.py does (`OkfError::Refused`); the variants it words
//! differently (`NotFound`, `FrontmatterParse`, `Shape`) are unit-tested.
//! The live test runs on the synthetic corpus only: the script names its paths.
```

Extend the imports:

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use okf_mcp::error::OkfError;
use okf_mcp::okf::ops::{
    question, review, source, stamp, unquestion, OpContext, OpOutcome, SourceEntry,
};
use okf_mcp::okf::workspace::{DiskWorkspace, MemWorkspace, StoredFile, Workspace};
```

(the `workspace` line replaces the existing one; `std::collections` gains `BTreeMap` and `BTreeSet`). Add the constant after `COMMIT_TIMES`:

```rust
const OPS_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/parity/ops_script.json");
```

and, after `secrets_corpus`, the Rust side:

```rust
/// The ops script, `{"setup": {path: text}, "steps": [...]}`.
fn ops_script() -> Json {
    let text = std::fs::read_to_string(OPS_SCRIPT).expect("read ops script");
    serde_json::from_str(&text).expect("ops script is JSON")
}

/// The corpus as stored files, plus the script's setup files: the state
/// both sides start from.
fn ops_corpus(root: &Path, script: &Json) -> BTreeMap<String, StoredFile> {
    let workspace = DiskWorkspace::new(root).expect("corpus root exists");
    let mut files: BTreeMap<String, StoredFile> = workspace
        .list_files()
        .expect("list corpus")
        .into_iter()
        .map(|path| {
            let content = std::fs::read(root.join(&path)).expect("read corpus file");
            let file = StoredFile {
                content,
                executable: false,
            };
            (path, file)
        })
        .collect();
    let setup = script["setup"].as_object().expect("setup is an object");
    for (path, text) in setup {
        let content = text.as_str().expect("setup text is a string").as_bytes().to_vec();
        let file = StoredFile {
            content,
            executable: false,
        };
        files.insert(path.clone(), file);
    }
    files
}

/// Runs one script step through `okf_mcp::okf::ops`. Missing fields fall
/// back to the shim's `OPS_DEFAULTS` (`by` is "fixture").
fn run_op(workspace: &MemWorkspace, now: DateTime<Utc>, step: &Json) -> Result<OpOutcome, OkfError> {
    let text = |key: &str| {
        step[key]
            .as_str()
            .unwrap_or_else(|| panic!("step {step} needs a string {key}"))
    };
    let optional = |key: &str| step.get(key).and_then(Json::as_str);
    let ctx = OpContext {
        now,
        by: optional("by").unwrap_or("fixture"),
    };
    match text("cmd") {
        "stamp" => {
            let field = serde_json::from_value(step["field"].clone()).expect("a stamp field");
            stamp(workspace, &ctx, text("concept"), field)
        }
        "review" => {
            let questions: Vec<String> = step
                .get("question")
                .map(|q| serde_json::from_value(q.clone()).expect("a list of questions"))
                .unwrap_or_default();
            review(
                workspace,
                &ctx,
                text("concept"),
                text("outcome"),
                text("note"),
                &questions,
            )
        }
        "question" => question(workspace, &ctx, text("concept"), text("question")),
        "unquestion" => unquestion(workspace, &ctx, text("concept"), text("question")),
        "source" => {
            let entry = SourceEntry {
                id: text("id"),
                resource: text("resource"),
                title: optional("title"),
                author: optional("author"),
            };
            source(workspace, &ctx, text("concept"), &entry)
        }
        other => panic!("unknown ops command {other}"),
    }
}

/// The ops script's result, in `okf_shim.py ops`'s JSON shape.
fn rust_ops(root: &Path, now: DateTime<Utc>, script: &Json) -> Json {
    let before = ops_corpus(root, script);
    let workspace = MemWorkspace::from_stored(before.clone()).expect("corpus loads");
    let steps: Vec<Json> = script["steps"]
        .as_array()
        .expect("steps is a list")
        .iter()
        .map(|step| match run_op(&workspace, now, step) {
            Ok(outcome) => {
                let mut entry = json!({ "output": outcome.message });
                if !outcome.notes.is_empty() {
                    entry["notes"] = json!(outcome.notes);
                }
                entry
            }
            Err(error) => json!({ "error": error.to_string() }),
        })
        .collect();
    let after = workspace.into_stored();
    // Sorted by path (a `BTreeSet`), as the shim sorts.
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut files = serde_json::Map::new();
    for path in paths {
        let (old, new) = (before.get(path), after.get(path));
        if old.map(|f| &f.content) != new.map(|f| &f.content) {
            let text = new.map(|f| String::from_utf8(f.content.clone()).expect("ops write UTF-8"));
            files.insert(path.clone(), json!(text));
        }
    }
    json!({ "steps": steps, "files": files })
}
```

the golden test, after `render_matches_golden`:

```rust
#[test]
fn ops_match_golden() {
    assert_json_equal(
        &rust_ops(Path::new(FIXTURES), instant(FIXED_NOW), &ops_script()),
        &read_golden("ops.json"),
        "ops",
    );
}
```

the live side, after `python_analysis`:

```rust
fn python_ops(root: &Path, now: &str, script: &Json) -> Json {
    shim(
        &[OsStr::new("ops"), root.as_os_str(), OsStr::new(now)],
        Some(script),
    )
}
```

the live test, after `live_analysis_parity`:

```rust
#[test]
#[ignore = "needs python3 and okf.py; run with --ignored"]
fn live_ops_parity() {
    let fixtures = Path::new(FIXTURES);
    let script = ops_script();
    assert_json_equal(
        &rust_ops(fixtures, instant(FIXED_NOW), &script),
        &python_ops(fixtures, FIXED_NOW, &script),
        "ops on the synthetic corpus",
    );
}
```

and in `regenerate_goldens`, add to `files` before `SOURCE`:

```rust
        (
            "ops.json",
            pretty(&python_ops(root, FIXED_NOW, &ops_script())),
        ),
```

- [ ] **Step 8: Generate the golden and run the parity tests**

Run: `UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens`
Expected: 1 passed; `git status tests/golden` shows only the new `ops.json` (the other goldens and `SOURCE` are rewritten byte-identical; okf.py's SHA-256 is still `3ee65c4d...`).

Check the golden by eye: 26 steps, 5 of them errors (`has no frontmatter`, `index 5 is out of range`, `exactly matches 'no such question'`, `has no open_questions`, `already has a source with id 's1'`), step 8's output `ops/flow.md: removed open question: {'k': 'v'}`, and 8 changed files (`analysis/hub.md`, `analysis/lead.md`, `analysis/offsets.md`, `analysis/stamps.md`, `infra/hosts/crlf-host.md`, `infra/time-server.md`, `ops/flow.md`, `yaml/same-indent-lists.md`; `infra/hosts/crlf-host.md` keeps its `\r\n`).

Run: `cargo test --test parity` then `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
Expected: every test passes, including `ops_match_golden` and `live_ops_parity`.

- [ ] **Step 9: Update the design**

In "Errors", after the table, add: "The ported `okf.py` commands (`stamp`, `review`, `question`, `unquestion`, `add_source`, `log`, `index`) refuse with `okf.py`'s own words: its `die(msg)` is `Refused { reason: msg }`, except where a variant above already names the problem (`NotFound`, `InvalidPath`, `FrontmatterParse`, and `Shape` for an `open_questions` or `sources` that is not a list)."

- [ ] **Step 10: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-02-frontmatter-ops.md`:

```markdown
# M5 Task 2: `okf::ops`: the shared op types and the frontmatter ops (<ts>)

Task 2 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md` (milestone 5).

## Added

- `src/okf/ops/mod.rs`: `OpContext`, `OpOutcome` (`touched`, `replay`, `message`,
  `notes`), `StampField`, `SourceEntry`, `LogKind`, and the crate-internal helpers
  `load_concept`, `save`, `stamp_value`, `refused`.
- `src/okf/ops/frontmatter.rs`: `stamp`, `review`, `question`, `unquestion`, `source`,
  `OUTCOMES`; ports of `okf.py:653-750`, with 14 unit tests on a `MemWorkspace` with a
  frozen clock.
- `src/okf/ops/replay.rs`: the `ReplayOp` type (Task 3 adds its functions).
- `Doc::remove_lines`, for `unquestion`'s single-item removal.
- Parity: `okf_shim.py ops ROOT NOW` runs a JSON script through okf.py's `cmd_*`
  functions on a temp copy with `now_utc` frozen; `tests/parity/ops_script.json`
  (26 steps, one setup file); `tests/golden/ops.json`; `ops_match_golden` and the
  live `live_ops_parity`.

## Changed

- Design, "Errors": the ported commands refuse with okf.py's wording.

## Notes

- `die(msg)` maps to `Refused { reason: msg }`; `NotFound`, `InvalidPath`,
  `FrontmatterParse` and `Shape` keep their M1 wording, so the parity script holds only
  refusals.
- Removing a mapping item from `open_questions` prints Python's `str()` of it
  (`{'k': 'v'}`), and converting a flow list re-renders such an item as that quoted
  text, as okf.py does.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/okf/mod.rs src/okf/doc.rs tests/parity/okf_shim.py tests/parity/ops_script.json tests/parity.rs tests/golden/ops.json docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: okf::ops frontmatter ops, ported from okf.py with byte parity (M5.02)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `log`, `index`, `ensure_index` and replay

> Interfaces note (written while planning, 2026-11-03):
> - **Starts after Task 2:** it fills in Task 2's `src/okf/ops/replay.rs` (which holds `ReplayOp`) and extends Task 2's parity script and shim command.
> - **`insert_index_bullet_text` returns `Option<String>`**, `None` when `section` names a heading that is missing and `create_section` is false, instead of `Result<String, OkfError>`. The function stays pure: the caller knows the file's path and words the refusal (`okf.py`'s "no heading 'X' in <path>").
> - **`ensure_index` is `pub`**, not `pub(crate)`, so `tests/parity.rs` (a separate crate) can compare it with `okf.py`'s.
> - **Two crate-internal helpers** that Task 4's `create_concept` uses: `log_index::{Listing, list_concept}`, the listing half of `index`.
> - **Visibility change:** `okf::analysis::links::INDEX_HEADING` becomes `pub(crate)`, so `insert_index_bullet_text` uses the same `^#+\s` pattern as the index-drift check.

> **Corrected (issue #91), 2026-11-03.** Two changes from Task 2's implementation: Task 3 adds `OpOutcome::record` (Step 3), which Task 2 left out because nothing there called it; and a public item's doc comment may not link the crate-private `load_concept`, `save` or `refused` (`RUSTDOCFLAGS="-D warnings" cargo doc` fails on it), so `index`'s Errors section points at `stamp` instead.

> **Corrected (issue #93), 2026-11-03.** Three mechanical fixes, made inline below: the two Hegel properties are `#[hegel::test(derandomize = true)]`, so every run draws the same cases (the seed comes from the test name; both stay bounded at 6 and 4 `log` calls per case); `log_index.rs`'s module doc links [`mod@super::replay`], because `super::replay` is both a module and a function and rustdoc refuses the ambiguous link; and okf.py's `no log.md at <root>` is in backticks, since rustdoc reads a bare `<root>` as an unclosed HTML tag. `cargo fmt` also rewraps a few test lines; the code is otherwise what shipped. After review of PR #107: `log_index.rs`'s Divergences list also notes that `index` refuses a list or mapping `title` (okf.py crashes; scalars are strings in both parsers), an 8th `log_index` test (`index_lists_a_scalar_title_and_refuses_a_list_title`) pins it, and `ensure_index`'s doc describes its real order (own index first, then the parent, then the listing) and that files written before an error stay written. Read `src/okf/ops/log_index.rs` for that text.

This task ports `okf.py`'s bookkeeping:
- `log` (`cmd_log`, `okf.py:752`);
- `index` (`cmd_index`, `:795`), through `insert_index_bullet` (`:1410`);
- `ensure_index` (`:1471`), which `create_concept` (Task 4) and M6's `move_concept` use.

Each op writes through the workspace and records `ReplayOp`s, so publish (Task 10) can redo the edit on whatever `log.md` or `index.md` is current by then, as git's `union` merge did. The text edits are pure functions shared by the op and by `replay`, so the two cannot drift apart:
- `log_text` is `cmd_log`'s body;
- `insert_index_bullet_text` is `insert_index_bullet`'s.

`replay` is idempotent:
- a log bullet already under its date adds nothing;
- an index that already links `target` gains nothing;
- `CreateIndex` of an existing file does nothing;
- `CreateIndex` with no current file creates it.

Like `okf.py`, these functions read universal-newline text and write `\n`. So `replay` normalizes a file's line endings only when an operation actually changes it.

**Files:**
- Modify: `src/okf/ops/replay.rs` (the functions)
- Create: `src/okf/ops/log_index.rs`
- Modify: `src/okf/ops/mod.rs` (`pub mod log_index;` and re-exports)
- Modify: `src/okf/analysis/links.rs` (`INDEX_HEADING` becomes `pub(crate)`)
- Modify: `tests/parity/okf_shim.py`, `tests/parity/ops_script.json`, `tests/parity.rs`
- Modify: `tests/golden/ops.json` (regenerated)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Data flow", "Write" step 3)
- Create: `changelog/<ts>-m5-task-03-log-index-replay.md`

- [ ] **Step 1: Write the failing tests**

Append this tests module to `src/okf/ops/replay.rs` (the code goes above it in Step 3). The two `#[hegel::test]` properties draw random sequences of `log` calls:
- replaying what one branch recorded reproduces what the op wrote, and replaying it twice is the same as once;
- replaying a second branch onto the first branch's published `log.md` keeps every bullet of both, each exactly once.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::{ensure_index, log, LogKind, OpContext, OpOutcome};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    const FIXTURE_LOG: &str = "# Log\n\n## 2026-10-26\n\n* **Creation:** Added fixtures.\n";

    fn on(month: u32, day: u32) -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, month, day, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn listing(target: &str, bullet: &str, section: Option<&str>) -> ReplayOp {
        ReplayOp::ListBullet {
            target: target.to_string(),
            bullet: bullet.to_string(),
            section: section.map(str::to_string),
            create_section: false,
        }
    }

    #[test]
    fn log_text_adds_a_newer_heading_above_older_ones() {
        let text = log_text(FIXTURE_LOG, "2026-10-27", "* **Update:** Ran.").unwrap();

        assert_eq!(
            text,
            concat!(
                "# Log\n\n## 2026-10-27\n\n* **Update:** Ran.\n\n",
                "## 2026-10-26\n\n* **Creation:** Added fixtures.\n",
            )
        );
    }

    #[test]
    fn log_text_puts_the_newest_bullet_first_and_skips_a_duplicate() {
        let once = log_text(FIXTURE_LOG, "2026-10-26", "* **Change:** More.").unwrap();

        let again = log_text(&once, "2026-10-26", "* **Creation:** Added fixtures.");

        assert_eq!(
            once,
            "# Log\n\n## 2026-10-26\n\n* **Change:** More.\n* **Creation:** Added fixtures.\n"
        );
        assert_eq!(again, None);
    }

    #[test]
    fn log_text_fills_an_empty_heading_and_logs_without_dates() {
        let date = "2026-10-27";

        let empty_heading = log_text("# Log\n\n## 2026-10-27\n", date, "* b").unwrap();
        let title_only = log_text("# Log\n", date, "* b").unwrap();
        let title_and_intro = log_text("# Log\n\nIntro.\n", date, "* b").unwrap();
        let no_title = log_text("Intro, no title\n", date, "* b").unwrap();
        let empty = log_text("", date, "* b").unwrap();

        // Each expected value is what okf.py's cmd_log writes.
        assert_eq!(empty_heading, "# Log\n\n## 2026-10-27\n\n* b\n");
        assert_eq!(title_only, "# Log\n\n## 2026-10-27\n\n* b\n");
        assert_eq!(title_and_intro, "# Log\n\n## 2026-10-27\n\n* b\n\n\nIntro.\n");
        assert_eq!(no_title, "Intro, no title\n\n## 2026-10-27\n\n* b\n");
        assert_eq!(empty, "\n## 2026-10-27\n\n* b\n");
    }

    #[test]
    fn insert_under_a_named_section() {
        let blank_after = insert_index_bullet_text("# A\n\n# Extras\n\nText.\n", "* b", Some("Extras"), false);
        let text_after = insert_index_bullet_text("# A\n\n# Extras\nText.\n", "* b", Some("## extras "), false);
        let after_bullets = insert_index_bullet_text(
            "# A\n\n# Extras\n\n* old\n- older\n\n# B\n",
            "* b",
            Some("Extras"),
            false,
        );

        assert_eq!(blank_after.unwrap(), "# A\n\n# Extras\n\n* b\n\nText.\n");
        assert_eq!(text_after.unwrap(), "# A\n\n# Extras\n\n* b\n\nText.\n");
        assert_eq!(
            after_bullets.unwrap(),
            "# A\n\n# Extras\n\n* old\n- older\n* b\n\n# B\n"
        );
    }

    #[test]
    fn insert_without_a_section_avoids_the_sections_list() {
        let sections = "\n# Sections\n\n* [S](s/) - s\n";
        let under_concepts = format!("# A\n\n# Concepts\n\nText.\n{sections}");
        let loose_bullets = format!("* [x](x.md) - x\n{sections}");
        let only_sections = format!("# A\n\nIntro.\n\n{sections}");

        let results = [
            insert_index_bullet_text(&under_concepts, "* b", None, false),
            insert_index_bullet_text(&loose_bullets, "* b", None, false),
            insert_index_bullet_text(&only_sections, "* b", None, false),
            insert_index_bullet_text("# A", "* b", None, false),
            insert_index_bullet_text("", "* b", Some(""), false),
        ];

        assert_eq!(
            results.map(Option::unwrap),
            [
                format!("# A\n\n# Concepts\n\n* b\n\nText.\n{sections}"),
                format!("* [x](x.md) - x\n* b\n{sections}"),
                format!("# A\n\nIntro.\n\n# Concepts\n\n* b\n\n{sections}"),
                "# A\n\n# Concepts\n\n* b\n".to_string(),
                "\n# Concepts\n\n* b\n".to_string(),
            ]
        );
    }

    #[test]
    fn a_missing_section_is_refused_or_created() {
        let refused = insert_index_bullet_text("# A\n", "* b", Some("Nope"), false);
        let created = insert_index_bullet_text(
            "# Deep\n\nTODO: say what this folder holds.\n",
            "* [Er](er/) - TODO: describe this folder",
            Some("Sections"),
            true,
        );

        assert_eq!(refused, None);
        assert_eq!(
            created.unwrap(),
            concat!(
                "# Deep\n\nTODO: say what this folder holds.\n\n# Sections\n\n",
                "* [Er](er/) - TODO: describe this folder\n",
            )
        );
    }

    #[test]
    fn create_index_creates_only_a_missing_file() {
        let create = [ReplayOp::CreateIndex {
            folder: "ops/new-stuff".to_string(),
        }];

        let missing = replay("ops/new-stuff/index.md", None, &create).unwrap();
        let present = replay("ops/new-stuff/index.md", Some("# Mine\n"), &create).unwrap();

        assert_eq!(missing, "# New stuff\n\nTODO: say what this folder holds.\n");
        assert_eq!(present, "# Mine\n");
    }

    #[test]
    fn a_listing_is_added_once_and_a_folder_link_needs_no_slash() {
        let ops = [listing("ops/a.md", "* [A](a.md) - a", None)];
        let folder = [listing("ops/index.md", "* [Ops](ops/) - o", Some("Sections"))];

        let once = replay("ops/index.md", Some("# Ops\n"), &ops).unwrap();
        let twice = replay("ops/index.md", Some(&once), &ops).unwrap();
        let listed = replay("index.md", Some("# Root\n\n* [Ops](ops) - o\n"), &folder).unwrap();

        assert_eq!(once, "# Ops\n\n# Concepts\n\n* [A](a.md) - a\n");
        assert_eq!(twice, once);
        assert_eq!(listed, "# Root\n\n* [Ops](ops) - o\n");
    }

    #[test]
    fn replay_normalizes_newlines_only_when_it_changes_the_text() {
        let crlf_log = "# Log\r\n\r\n## 2026-10-26\r\n\r\n* **Update:** old\r\n";
        let crlf_index = "# Ops\r\n\r\n* [A](a.md) - a\r\n";
        let log_op = [ReplayOp::Log {
            date: "2026-10-27".to_string(),
            bullet: "* **Update:** b".to_string(),
        }];
        let listed = [listing("ops/a.md", "* [A](a.md) - a", None)];

        let changed = replay("log.md", Some(crlf_log), &log_op).unwrap();
        let unchanged = replay("ops/index.md", Some(crlf_index), &listed).unwrap();

        assert_eq!(
            changed,
            "# Log\n\n## 2026-10-27\n\n* **Update:** b\n\n## 2026-10-26\n\n* **Update:** old\n"
        );
        assert_eq!(unchanged, crlf_index);
    }

    #[test]
    fn replay_refuses_a_missing_file_or_heading() {
        let log_op = [ReplayOp::Log {
            date: "2026-10-27".to_string(),
            bullet: "* b".to_string(),
        }];
        let in_section = [listing("people/x.md", "* [X](x.md) - x", Some("Nowhere"))];

        let no_log = replay("log.md", None, &log_op).unwrap_err();
        let no_heading = replay("people/index.md", Some("# People\n"), &in_section).unwrap_err();

        assert!(matches!(no_log, OkfError::NotFound { ref path } if path == "log.md"));
        assert!(matches!(
            no_heading,
            OkfError::Refused { ref reason } if reason == "no heading 'Nowhere' in people/index.md"
        ));
    }

    #[test]
    fn replay_ops_serialize_with_an_op_tag() {
        let op = listing("a/b.md", "* [B](b.md) - b", None);

        let value = serde_json::to_value(&op).unwrap();
        let back: ReplayOp = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(
            value,
            json!({
                "op": "list_bullet",
                "target": "a/b.md",
                "bullet": "* [B](b.md) - b",
                "section": null,
                "create_section": false,
            })
        );
        assert_eq!(back, op);
    }

    #[test]
    fn ensure_index_ops_rebuild_every_file_they_touched() {
        let root = "# Root\n\n* [Infra](infra/) - systems\n";
        let workspace = MemWorkspace::from_files([("index.md", root)]);
        let mut outcome = OpOutcome::default();

        ensure_index(&workspace, "ops/deep/er", &mut outcome).unwrap();

        assert_eq!(
            outcome.touched,
            ["ops/deep/er/index.md", "ops/deep/index.md", "ops/index.md", "index.md"]
        );
        for path in &outcome.touched {
            let original = (path == "index.md").then_some(root);
            let replayed = replay(path, original, &outcome.replay[path]).unwrap();
            assert_eq!(replayed, workspace.read_to_string(path).unwrap(), "{path}");
        }
    }

    #[hegel::test(derandomize = true)]
    fn log_replay_redoes_the_ops_and_is_idempotent(tc: hegel::TestCase) {
        use hegel::generators;
        let bases = vec![
            FIXTURE_LOG,
            "# Log\n",
            "",
            "Intro, no title\n",
            "# Log\n\n## 2026-10-27\n",
            "# Log\r\n\r\n## 2026-10-27\r\n\r\n* **Update:** a\r\n",
        ];
        let words = vec!["a", "b", "c  d", "e\nf"];
        let base: &str = tc.draw(generators::sampled_from(&bases));
        let workspace = MemWorkspace::from_files([("log.md", base)]);
        let count = tc.draw(generators::integers::<usize>().max_value(6));
        let mut ops = Vec::new();
        for _ in 0..count {
            let day = tc.draw(generators::integers::<u32>().min_value(26).max_value(28));
            let word: &str = tc.draw(generators::sampled_from(&words));
            let outcome = log(&workspace, &on(10, day), LogKind::Update, word).unwrap();
            ops.extend(outcome.replay.get("log.md").cloned().unwrap_or_default());
        }

        let once = replay("log.md", Some(base), &ops).unwrap();
        let twice = replay("log.md", Some(&once), &ops).unwrap();

        assert_eq!(once, workspace.read_to_string("log.md").unwrap());
        assert_eq!(twice, once);
    }

    #[hegel::test(derandomize = true)]
    fn replaying_a_branch_onto_another_keeps_every_bullet(tc: hegel::TestCase) {
        use hegel::generators;
        let left = MemWorkspace::from_files([("log.md", FIXTURE_LOG)]);
        let right = MemWorkspace::from_files([("log.md", FIXTURE_LOG)]);
        let mut bullets = vec!["* **Creation:** Added fixtures.".to_string()];
        let mut right_ops = Vec::new();
        for (workspace, side) in [(&left, "left"), (&right, "right")] {
            let count = tc.draw(generators::integers::<usize>().max_value(4));
            for n in 0..count {
                let day = tc.draw(generators::integers::<u32>().min_value(26).max_value(28));
                let text = format!("{side} {n}");
                let outcome = log(workspace, &on(10, day), LogKind::Update, &text).unwrap();
                let ops = outcome.replay["log.md"].clone();
                bullets.push(format!("* **Update:** {text}"));
                if side == "right" {
                    right_ops.extend(ops);
                }
            }
        }
        let published = left.read_to_string("log.md").unwrap();

        let merged = replay("log.md", Some(&published), &right_ops).unwrap();
        let again = replay("log.md", Some(&merged), &right_ops).unwrap();

        for bullet in &bullets {
            let copies = merged.split('\n').filter(|line| line == bullet).count();
            assert_eq!(copies, 1, "{bullet} in\n{merged}");
        }
        assert_eq!(again, merged);
    }
}
```

Create `src/okf/ops/log_index.rs` with only its tests module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};

    const INFRA_INDEX: &str =
        "# Infra\n\n# Concepts\n\n* [Time server](time-server.md) - GPS clock\n";

    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn infra() -> MemWorkspace {
        MemWorkspace::from_files([
            ("infra/index.md", INFRA_INDEX),
            ("infra/ntp.md", "---\ntype: Service\ntitle: NTP [pool]\n---\n"),
            ("infra/untitled.md", "---\ntype: Note\n---\n"),
            ("people/x.md", "---\ntype: Person\ntitle: X\n---\n"),
        ])
    }

    #[test]
    fn log_writes_the_entry_and_records_its_replay() {
        let ws = MemWorkspace::from_files([("log.md", "# Log\n")]);

        let outcome = log(&ws, &ctx(), LogKind::Creation, "  Added   the\n pool. ").unwrap();

        assert_eq!(
            ws.read_to_string("log.md").unwrap(),
            "# Log\n\n## 2026-10-27\n\n* **Creation:** Added the pool.\n"
        );
        assert_eq!(outcome.touched, ["log.md"]);
        assert_eq!(
            outcome.replay["log.md"],
            [ReplayOp::Log {
                date: "2026-10-27".to_string(),
                bullet: "* **Creation:** Added the pool.".to_string(),
            }]
        );
        assert_eq!(
            outcome.message,
            "log.md: ## 2026-10-27: * **Creation:** Added the pool."
        );
    }

    #[test]
    fn log_reports_a_duplicate_and_needs_log_md() {
        let ws = MemWorkspace::from_files([("log.md", "# Log\n")]);
        log(&ws, &ctx(), LogKind::Update, "Once.").unwrap();

        let again = log(&ws, &ctx(), LogKind::Update, "Once.").unwrap();
        let missing = log(&MemWorkspace::default(), &ctx(), LogKind::Update, "x").unwrap_err();

        assert!(again.touched.is_empty());
        assert_eq!(again.message, "log.md: already logged under 2026-10-27");
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }

    #[test]
    fn index_lists_a_concept_with_its_title_escaped() {
        let ws = infra();

        let outcome = index(&ws, &ctx(), "infra/ntp", "  the   pool ", None, None).unwrap();

        let bullet = "* [NTP \\[pool\\]](ntp.md) - the pool";
        assert_eq!(
            ws.read_to_string("infra/index.md").unwrap(),
            format!("{INFRA_INDEX}{bullet}\n")
        );
        assert_eq!(outcome.message, format!("infra/index.md: {bullet}"));
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::ListBullet {
                target: "infra/ntp.md".to_string(),
                bullet: bullet.to_string(),
                section: None,
                create_section: false,
            }]
        );
    }

    #[test]
    fn index_of_a_listed_concept_changes_nothing() {
        let ws = infra();
        index(&ws, &ctx(), "infra/ntp", "pool", None, None).unwrap();

        let again = index(&ws, &ctx(), "infra/ntp.md", "other", None, Some("T")).unwrap();

        assert!(again.touched.is_empty());
        assert_eq!(again.message, "infra/index.md: already lists infra/ntp.md");
    }

    #[test]
    fn index_refusals_use_okf_py_wording() {
        let ws = infra();

        let untitled = index(&ws, &ctx(), "infra/untitled", "s", None, None).unwrap_err();
        let no_index = index(&ws, &ctx(), "people/x", "s", None, None).unwrap_err();
        let no_heading =
            index(&ws, &ctx(), "infra/untitled", "s", Some("Nowhere"), Some("T")).unwrap_err();

        assert_eq!(refusal(untitled), "infra/untitled.md has no title; pass --title");
        assert_eq!(refusal(no_index), "no index.md in /people; create it first");
        assert_eq!(refusal(no_heading), "no heading 'Nowhere' in infra/index.md");
        assert_eq!(ws.read_to_string("infra/index.md").unwrap(), INFRA_INDEX);
    }

    #[test]
    fn ensure_index_follows_the_parents_link_style() {
        let ws = MemWorkspace::from_files([
            ("index.md", "# Root\n\n* [Infra](infra/) - systems\n"),
            (
                "analysis/index.md",
                "# Analysis\n\n# Sections\n\n* [Sub](/analysis/sub/) - a folder\n",
            ),
        ]);
        let mut absolute = OpOutcome::default();
        let mut nested = OpOutcome::default();

        ensure_index(&ws, "analysis/new-stuff", &mut absolute).unwrap();
        ensure_index(&ws, "ops/deep", &mut nested).unwrap();

        assert_eq!(
            absolute.touched,
            ["analysis/new-stuff/index.md", "analysis/index.md"]
        );
        assert_eq!(
            ws.read_to_string("analysis/new-stuff/index.md").unwrap(),
            "# New stuff\n\nTODO: say what this folder holds.\n"
        );
        assert_eq!(
            ws.read_to_string("analysis/index.md").unwrap(),
            concat!(
                "# Analysis\n\n# Sections\n\n* [Sub](/analysis/sub/) - a folder\n",
                "* [New stuff](/analysis/new-stuff/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            absolute.replay["analysis/index.md"],
            [ReplayOp::ListBullet {
                target: "analysis/new-stuff/index.md".to_string(),
                bullet: "* [New stuff](/analysis/new-stuff/) - TODO: describe this folder"
                    .to_string(),
                section: Some("Sections".to_string()),
                create_section: true,
            }]
        );
        assert_eq!(nested.touched, ["ops/deep/index.md", "ops/index.md", "index.md"]);
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "# Root\n\n* [Infra](infra/) - systems\n\n# Sections\n\n",
                "* [Ops](ops/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            nested.notes,
            [
                "created /ops/index.md; replace its TODO lines (and the parent's listing)",
                "created /ops/deep/index.md; replace its TODO lines (and the parent's listing)",
            ]
        );
    }

    #[test]
    fn ensure_index_of_an_existing_folder_or_the_root_does_nothing() {
        let ws = MemWorkspace::from_files([("index.md", "# Root\n"), ("ops/index.md", "# Ops\n")]);
        let mut outcome = OpOutcome::default();

        ensure_index(&ws, "ops", &mut outcome).unwrap();
        ensure_index(&ws, "", &mut outcome).unwrap();

        assert_eq!(outcome, OpOutcome::default());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib -- okf::ops`
Expected: compile errors (`log_text`, `replay`, `log`, `index`, `ensure_index` not found).

- [ ] **Step 3: Implement**

In `src/okf/analysis/links.rs`, make the heading pattern crate-visible and say who else uses it:

```rust
/// `^#+\s` (Python `\s`): an index heading. Also used by
/// `okf::ops::replay::insert_index_bullet_text`.
pub(crate) static INDEX_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#+[\s\x1c-\x1f]").expect("INDEX_HEADING regex is valid"));
```

Replace `src/okf/ops/replay.rs` with the following (the `ReplayOp` type is unchanged from Task 2):

```rust
//! Replay operations: how a bookkeeping edit of `log.md` or an `index.md`
//! is redone on newer text at publish (design "Data flow", "Write" step 3
//! and "Publish" step 3), and the pure text edits that both the ops
//! (`log_index.rs`) and [`replay`] run, so an edit and its replay cannot
//! drift apart.
//!
//! [`log_text`] is `okf.py`'s `cmd_log` body (`:752`) and
//! [`insert_index_bullet_text`] its `insert_index_bullet` (`:1410`), line
//! for line. `okf.py` reads and writes these files in text mode, so both
//! work on universal-newline text and produce `\n` endings.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::okf::analysis::links::INDEX_HEADING;
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::paths::basename;
use crate::okf::pystr::{py_repr_str, py_strip, universal_newlines};
use crate::okf::workspace::{MemWorkspace, Workspace};

/// One replayable bookkeeping edit, stored in a changeset entry's `replay`
/// column as JSON (`{"op": "log", "date": ..., "bullet": ...}`).
///
/// `#[serde(tag = "op")]` writes the variant name into an `op` field next
/// to the variant's own fields, like a Python dict with a "type" key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReplayOp {
    /// `log`: add `bullet` under `## <date>` in `log.md`.
    Log {
        /// The UTC date of the heading, `2026-11-03`.
        date: String,
        /// The whole bullet line, `* **Update:** text`.
        bullet: String,
    },
    /// `index` and folder listings: add `bullet` to an `index.md` unless it
    /// already links `target`.
    ListBullet {
        /// Catalog-relative path the bullet lists (a concept, or a folder's
        /// `index.md`).
        target: String,
        /// The whole bullet line.
        bullet: String,
        /// The heading to put it under; `None` for `okf.py`'s default place.
        section: Option<String>,
        /// Create the heading when it is missing (folder listings under `# Sections`).
        create_section: bool,
    },
    /// `ensure_index`: create a folder's `index.md` unless it exists.
    CreateIndex {
        /// Catalog-relative folder, `infra/hosts`.
        folder: String,
    },
}

/// Redoes `ops` on a file's current text (design "Publish", step 3).
///
/// Idempotent where publish needs it: a log bullet already under its date,
/// a listing whose `target` the index already links, and a `CreateIndex`
/// of a file that exists all leave the text alone. Text that no operation
/// changes is returned byte for byte; changed text has `\n` endings, as
/// `okf.py` writes it.
///
/// # Arguments
/// * `path` - the file's catalog-relative path (`log.md`, `infra/index.md`),
///   used to resolve relative links and in errors.
/// * `current` - its current published text; `None` when it does not exist.
/// * `ops` - the operations recorded for it, in order.
///
/// # Returns
/// The text after the operations.
///
/// # Errors
/// [`OkfError::NotFound`] when a `Log` or `ListBullet` (or nothing) finds
/// no file; [`OkfError::Refused`] when a listing's section heading is gone.
///
/// # Examples
/// ```
/// use okf_mcp::okf::ops::{replay, ReplayOp};
/// let ops = [ReplayOp::Log { date: "2026-11-03".into(), bullet: "* **Update:** x".into() }];
/// let once = replay("log.md", Some("# Log\n"), &ops).unwrap();
/// assert_eq!(once, "# Log\n\n## 2026-11-03\n\n* **Update:** x\n");
/// assert_eq!(replay("log.md", Some(&once), &ops).unwrap(), once);
/// ```
pub fn replay(path: &str, current: Option<&str>, ops: &[ReplayOp]) -> Result<String, OkfError> {
    // Publish has no directory listing, so links resolve as if no path were
    // a directory; `lists_target` then also accepts `folder` for `folder/index.md`.
    let no_dirs = MemWorkspace::default();
    // `Cow` holds either the borrowed `current` or a newly built `String`,
    // so unchanged text is never copied.
    let mut text: Option<Cow<'_, str>> = current.map(Cow::Borrowed);
    for op in ops {
        let updated = match op {
            ReplayOp::CreateIndex { folder } => text.is_none().then(|| new_index_text(folder)),
            ReplayOp::Log { date, bullet } => {
                log_text(&universal_newlines(present(path, &text)?), date, bullet)
            }
            ReplayOp::ListBullet {
                target,
                bullet,
                section,
                create_section,
            } => {
                let base = universal_newlines(present(path, &text)?);
                if lists_target(&no_dirs, &base, path, target) {
                    None
                } else {
                    let section = section.as_deref();
                    let inserted =
                        insert_index_bullet_text(&base, bullet, section, *create_section);
                    Some(inserted.ok_or_else(|| missing_heading(path, section.unwrap_or("")))?)
                }
            }
        };
        if let Some(updated) = updated {
            text = Some(Cow::Owned(updated));
        }
    }
    text.map(Cow::into_owned).ok_or_else(|| OkfError::NotFound {
        path: path.to_string(),
    })
}

/// The current text, or `NotFound` for an operation that needs a file.
fn present<'t>(path: &str, text: &'t Option<Cow<'_, str>>) -> Result<&'t str, OkfError> {
    text.as_deref().ok_or_else(|| OkfError::NotFound {
        path: path.to_string(),
    })
}

/// `okf.py`'s "no heading 'X' in <path>" refusal of a listing.
pub(crate) fn missing_heading(path: &str, section: &str) -> OkfError {
    OkfError::Refused {
        reason: format!("no heading {} in {path}", py_repr_str(section)),
    }
}

/// Does `text` (an `index.md` at `index`) link `target`? `cmd_index`'s
/// "already lists" check. A link resolving to `folder` also counts for
/// `folder/index.md`, which only matters when `workspace` cannot tell that
/// `folder` is a directory (replay): a concept target never ends in
/// `/index.md`, so `index`'s own check is exactly `okf.py`'s.
pub(crate) fn lists_target(
    workspace: &dyn Workspace,
    text: &str,
    index: &str,
    target: &str,
) -> bool {
    find_links(text)
        .iter()
        .filter_map(|link| resolve_link(workspace, &link.target, index))
        .any(|resolved| {
            resolved.path == target
                || target.strip_suffix("/index.md") == Some(resolved.path.as_str())
        })
}

/// Python's `str.capitalize`: the first character upper-cased, the rest
/// lower-cased.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        // `to_uppercase` yields one or more chars (`ß` becomes `SS`), so
        // the pieces are chained and collected rather than pushed one by one.
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

/// A folder's heading: `ensure_index`'s `name.replace("-", " ").capitalize()`.
pub(crate) fn folder_heading(folder: &str) -> String {
    capitalize(&basename(folder).replace('-', " "))
}

/// The `index.md` `ensure_index` writes for a new folder.
pub(crate) fn new_index_text(folder: &str) -> String {
    format!(
        "# {}\n\nTODO: say what this folder holds.\n",
        folder_heading(folder)
    )
}

/// `cmd_log`'s edit: adds `bullet` under `## <date>`, newest heading first
/// and newest bullet first under it.
///
/// # Arguments
/// * `text` - `log.md`, with universal newlines.
/// * `date` - the UTC date, `2026-11-03`.
/// * `bullet` - the whole bullet line.
///
/// # Returns
/// The new text, or `None` when `bullet` is already under that heading
/// (`okf.py`'s "already logged").
///
/// # Examples
/// ```
/// use okf_mcp::okf::ops::log_text;
/// let text = log_text("# Log\n", "2026-11-03", "* **Update:** x").unwrap();
/// assert_eq!(text, "# Log\n\n## 2026-11-03\n\n* **Update:** x\n");
/// assert_eq!(log_text(&text, "2026-11-03", "* **Update:** x"), None);
/// ```
pub fn log_text(text: &str, date: &str, bullet: &str) -> Option<String> {
    let heading = format!("## {date}");
    // `lines` borrows from `text`, `heading` and `bullet`; Rust checks they
    // all outlive it, which is why `heading` is declared first.
    let mut lines: Vec<&str> = text.split('\n').collect();
    if let Some(at) = lines.iter().position(|line| *line == heading) {
        let blank = lines[at + 1..]
            .iter()
            .take_while(|line| py_strip(line).is_empty())
            .count();
        let first = at + 1 + blank;
        let section_end = first
            + lines[first..]
                .iter()
                .take_while(|line| !line.starts_with("## "))
                .count();
        if lines[first..section_end].contains(&bullet) {
            return None;
        }
        if lines.get(first).is_some_and(|line| line.starts_with("* ")) {
            lines.insert(first, bullet);
        } else {
            // `splice` is Python's slice assignment `lines[a:b] = [...]`;
            // dropping the iterator it returns applies the edit.
            lines.splice(at + 1..first, ["", bullet, ""]);
        }
    } else if let Some(first) = lines.iter().position(|line| line.starts_with("## ")) {
        lines.splice(first..first, [heading.as_str(), "", bullet, ""]);
    } else {
        let title = lines.iter().position(|line| line.starts_with("# "));
        while lines.last() == Some(&"") {
            lines.pop();
        }
        let insert_at = title.map_or(lines.len(), |t| t + 1);
        let mut block = vec!["", heading.as_str(), "", bullet];
        if insert_at < lines.len() {
            block.push("");
        }
        lines.splice(insert_at..insert_at, block);
        lines.push("");
    }
    Some(lines.join("\n"))
}

/// Is this line an index bullet (`okf.py`'s `_index_bullet`)?
fn is_bullet(line: &str) -> bool {
    line.starts_with("* ") || line.starts_with("- ")
}

/// Drops trailing empty lines (`while lines and lines[-1] == "": lines.pop()`).
fn trim_trailing_blank(lines: &mut Vec<Cow<'_, str>>) {
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
}

/// The first bullet of a heading's span at `start`, with the blank lines
/// `okf.py` puts around it.
fn insert_first_bullet<'a>(lines: &mut Vec<Cow<'a, str>>, start: usize, bullet: &'a str) {
    let text_follows = lines
        .get(start)
        .is_some_and(|line| !py_strip(line).is_empty());
    let mut block: Vec<Cow<'a, str>> = vec!["".into(), bullet.into()];
    if text_follows {
        block.push("".into());
    }
    lines.splice(start..start, block);
}

/// `insert_index_bullet`'s edit: adds `bullet` after the last bullet of
/// its `section` heading, or, with no section, at the end of `# Concepts`
/// (or after the last bullet outside `# Sections`), creating `# Concepts`
/// ahead of `# Sections` when there is nowhere else to put it.
///
/// # Arguments
/// * `text` - the `index.md`, with universal newlines.
/// * `bullet` - the whole bullet line.
/// * `section` - the heading to list under (matched case-insensitively,
///   leading `#`s ignored); `None` or empty for the default place.
/// * `create_section` - append the heading when it is missing.
///
/// # Returns
/// The new text, ending in a newline; `None` when `section` is missing and
/// `create_section` is false (`okf.py`'s "no heading 'X'").
pub fn insert_index_bullet_text(
    text: &str,
    bullet: &str,
    section: Option<&str>,
    create_section: bool,
) -> Option<String> {
    let mut lines: Vec<Cow<'_, str>> = text.split('\n').map(Cow::Borrowed).collect();
    let total = lines.len();
    let headings: Vec<(usize, String)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| INDEX_HEADING.is_match(line))
        .map(|(i, line)| (i, py_strip(line.trim_start_matches('#')).to_lowercase()))
        .collect();
    // The lines after heading `i`, up to the next heading (`okf.py`'s `span`).
    let span = |i: usize| {
        let stop = headings
            .iter()
            .map(|(j, _)| *j)
            .find(|j| *j > i)
            .unwrap_or(total);
        (i + 1)..stop
    };
    let named = |want: &str| -> Vec<usize> {
        headings
            .iter()
            .filter(|(_, name)| name == want)
            .map(|(i, _)| *i)
            .collect()
    };
    if let Some(section) = section.filter(|s| !s.is_empty()) {
        let want = py_strip(py_strip(section).trim_start_matches('#'));
        match named(&want.to_lowercase()).first() {
            None if !create_section => return None,
            None => {
                trim_trailing_blank(&mut lines);
                // `Vec::extend` has two impls (owned and `Copy` references),
                // so the items are spelled as `Cow`s rather than `.into()`.
                lines.extend([
                    Cow::Borrowed(""),
                    Cow::Owned(format!("# {want}")),
                    Cow::Borrowed(""),
                    Cow::Borrowed(bullet),
                    Cow::Borrowed(""),
                ]);
            }
            Some(&hit) => {
                let range = span(hit);
                let start = range.start;
                // The last bullet: searched from the end (`rev().find`).
                let last = range.rev().find(|&i| is_bullet(&lines[i]));
                match last {
                    Some(last) => lines.insert(last + 1, bullet.into()),
                    None => insert_first_bullet(&mut lines, start, bullet),
                }
            }
        }
    } else {
        // Concept bullets never go under `# Sections`, which lists sub-folders.
        let concepts = named("concepts");
        let sections = named("sections");
        let last = match concepts.first() {
            Some(&heading) => span(heading).rev().find(|&i| is_bullet(&lines[i])),
            None => {
                let mut barred = vec![false; total];
                for range in sections.iter().map(|&s| span(s)) {
                    barred[range].iter_mut().for_each(|b| *b = true);
                }
                (0..total)
                    .rev()
                    .find(|&i| !barred[i] && is_bullet(&lines[i]))
            }
        };
        if let Some(last) = last {
            lines.insert(last + 1, bullet.into());
        } else if let Some(&heading) = concepts.first() {
            insert_first_bullet(&mut lines, heading + 1, bullet);
        } else if let Some(&heading) = sections.first() {
            let mut at = heading;
            while at > 0 && lines[at - 1].is_empty() {
                at -= 1;
            }
            lines.splice(
                at..at,
                [
                    Cow::Borrowed(""),
                    Cow::Borrowed("# Concepts"),
                    Cow::Borrowed(""),
                    Cow::Borrowed(bullet),
                ],
            );
        } else {
            trim_trailing_blank(&mut lines);
            lines.extend([
                Cow::Borrowed(""),
                Cow::Borrowed("# Concepts"),
                Cow::Borrowed(""),
                Cow::Borrowed(bullet),
                Cow::Borrowed(""),
            ]);
        }
    }
    if lines.last().is_none_or(|line| !line.is_empty()) {
        lines.push("".into());
    }
    Some(lines.join("\n"))
}
```

Put this above the tests in `src/okf/ops/log_index.rs`:

```rust
//! The bookkeeping ops `log` and `index` (`okf.py:752-820`) and
//! `ensure_index` (`:1471`). Each writes through the workspace and records
//! the [`ReplayOp`]s that redo its edit at publish (see [`mod@super::replay`]).
//!
//! Divergences from `okf.py`:
//! - a missing `log.md` is [`OkfError::NotFound`] (okf.py: `no log.md at <root>`);
//! - `ensure_index` upper-cases a folder name's first character where
//!   Python's `capitalize` title-cases it; they differ only for the few
//!   digraph letters such as `ǆ`;
//! - `ensure_index` needs no existing directory (`okf.py` expects `mv` to
//!   have created it): a workspace creates parents on write.

use crate::error::OkfError;
use crate::okf::ops::replay::{
    folder_heading, insert_index_bullet_text, lists_target, log_text, missing_heading,
    new_index_text, ReplayOp,
};
use crate::okf::ops::{load_concept, refused, LogKind, OpContext, OpOutcome};
use crate::okf::paths::{basename, dirname, join};
use crate::okf::pystr::is_py_space;
use crate::okf::workspace::Workspace;
use crate::okf::yaml::Value;

/// The catalog's log, at the root.
const LOG: &str = "log.md";
/// The heading folder listings go under.
const SECTIONS: &str = "Sections";

/// Python's `" ".join(text.split())`: runs of whitespace become one space,
/// and leading and trailing whitespace goes.
fn collapse_whitespace(text: &str) -> String {
    text.split(is_py_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Port of `cmd_log`: adds `* **<kind>:** <text>` under today's (UTC)
/// heading in the root `log.md`.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `ctx` - its `now` gives the date.
/// * `kind` - the entry's kind.
/// * `text` - the entry; whitespace runs collapse to single spaces.
///
/// # Returns
/// The outcome, with a `Log` replay op for `log.md`; nothing is touched
/// when the bullet is already under today's heading.
///
/// # Errors
/// [`OkfError::NotFound`] without a `log.md`; the workspace's write error.
pub fn log(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    kind: LogKind,
    text: &str,
) -> Result<OpOutcome, OkfError> {
    // Text mode with universal newlines, as `okf.py` reads `log.md`.
    let current = workspace.read_text(LOG)?;
    let date = ctx.now.format("%Y-%m-%d").to_string();
    let bullet = format!("* **{}:** {}", kind.as_str(), collapse_whitespace(text));
    let Some(updated) = log_text(&current, &date, &bullet) else {
        return Ok(OpOutcome::unchanged(format!(
            "{LOG}: already logged under {date}"
        )));
    };
    workspace.write_atomic(LOG, &updated)?;
    // A struct literal with `..default()` (not assignments after
    // `default()`, which clippy's `field_reassign_with_default` flags).
    let mut outcome = OpOutcome {
        message: format!("{LOG}: ## {date}: {bullet}"),
        ..OpOutcome::default()
    };
    outcome.record(LOG, ReplayOp::Log { date, bullet });
    Ok(outcome)
}

/// What [`list_concept`] lists: `index`'s arguments once resolved.
pub(crate) struct Listing<'a> {
    /// Catalog-relative path of the concept.
    pub(crate) concept: &'a str,
    /// The link text, before escaping.
    pub(crate) title: &'a str,
    /// The summary after ` - `; whitespace collapses.
    pub(crate) summary: &'a str,
    /// The heading to list under, if any.
    pub(crate) section: Option<&'a str>,
}

/// `cmd_index` after the concept is loaded: lists it in its folder's
/// `index.md` unless the index already links it, recording the replay op.
///
/// # Returns
/// `okf.py`'s message: `<folder>/index.md: <bullet>`, or
/// `<folder>/index.md: already lists <concept>`.
///
/// # Errors
/// [`OkfError::Refused`] without a folder `index.md`, or when `section`
/// names a missing heading; the workspace's read or write error.
pub(crate) fn list_concept(
    workspace: &dyn Workspace,
    listing: &Listing<'_>,
    outcome: &mut OpOutcome,
) -> Result<String, OkfError> {
    let folder = dirname(listing.concept);
    let index_path = join(folder, "index.md");
    if !workspace.exists(&index_path) {
        return Err(refused(format!(
            "no index.md in /{folder}; create it first"
        )));
    }
    let text = workspace.read_text(&index_path)?;
    if lists_target(workspace, &text, &index_path, listing.concept) {
        return Ok(format!("{index_path}: already lists {}", listing.concept));
    }
    let label = listing.title.replace('[', "\\[").replace(']', "\\]");
    let bullet = format!(
        "* [{label}]({}) - {}",
        basename(listing.concept),
        collapse_whitespace(listing.summary)
    );
    let updated = insert_index_bullet_text(&text, &bullet, listing.section, false)
        .ok_or_else(|| missing_heading(&index_path, listing.section.unwrap_or("")))?;
    workspace.write_atomic(&index_path, &updated)?;
    let message = format!("{index_path}: {bullet}");
    outcome.record(
        &index_path,
        ReplayOp::ListBullet {
            target: listing.concept.to_string(),
            bullet,
            section: listing.section.map(str::to_string),
            create_section: false,
        },
    );
    Ok(message)
}

/// Port of `cmd_index`: lists a concept in its folder's `index.md`.
///
/// # Arguments
/// * `workspace` - the catalog; `_ctx` is unused (no timestamp).
/// * `concept` - the concept reference.
/// * `summary` - the text after ` - `.
/// * `section` - the heading to list under; `None` for `# Concepts`.
/// * `title` - the link text; `None` (or empty) for the concept's `title`.
///
/// # Returns
/// The outcome, with a `ListBullet` replay op; nothing is touched when the
/// index already links the concept.
///
/// # Errors
/// As [`crate::okf::ops::stamp`] when loading the concept;
/// [`OkfError::Refused`] without a title, without a folder `index.md`, or
/// for a missing `section` heading.
pub fn index(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    summary: &str,
    section: Option<&str>,
    title: Option<&str>,
) -> Result<OpOutcome, OkfError> {
    let loaded = load_concept(workspace, concept)?;
    // `a.title or data.get("title")`: an empty string counts as absent.
    let frontmatter_title = loaded.data.get("title").and_then(Value::as_str);
    let title = title
        .filter(|t| !t.is_empty())
        .or(frontmatter_title.filter(|t| !t.is_empty()))
        .ok_or_else(|| refused(format!("{} has no title; pass --title", loaded.path)))?;
    let listing = Listing {
        concept: &loaded.path,
        title,
        summary,
        section,
    };
    let mut outcome = OpOutcome::default();
    let message = list_concept(workspace, &listing, &mut outcome)?;
    outcome.message = message;
    Ok(outcome)
}

/// Port of `ensure_index`: makes sure `folder/index.md` exists and is
/// listed in its parent's index, creating parents first.
///
/// A new index is `# <Folder name>` and a TODO line; its parent gains
/// `* [<Folder name>](<name>/) - TODO: describe this folder` under
/// `# Sections` (created when missing), with an absolute `/<folder>/` href
/// when the parent's bullets are absolute.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `folder` - catalog-relative folder; `""` (the root) does nothing.
/// * `outcome` - gains each file written, its replay ops (`CreateIndex`,
///   then the parent's `ListBullet`) and one note per created index.
///
/// # Errors
/// The workspace's read or write error (a missing root `index.md` is
/// [`OkfError::NotFound`]).
pub fn ensure_index(
    workspace: &dyn Workspace,
    folder: &str,
    outcome: &mut OpOutcome,
) -> Result<(), OkfError> {
    if folder.is_empty() {
        return Ok(());
    }
    let index_path = join(folder, "index.md");
    if workspace.exists(&index_path) {
        return Ok(());
    }
    workspace.write_atomic(&index_path, &new_index_text(folder))?;
    outcome.record(
        &index_path,
        ReplayOp::CreateIndex {
            folder: folder.to_string(),
        },
    );
    let parent = dirname(folder);
    ensure_index(workspace, parent, outcome)?;
    let parent_index = join(parent, "index.md");
    let parent_text = workspace.read_text(&parent_index)?;
    let absolute = parent_text
        .split('\n')
        .any(|line| line.starts_with("* [") && line.contains("](/"));
    let href = if absolute {
        format!("/{folder}/")
    } else {
        format!("{}/", basename(folder))
    };
    let bullet = format!(
        "* [{}]({href}) - TODO: describe this folder",
        folder_heading(folder)
    );
    let updated = insert_index_bullet_text(&parent_text, &bullet, Some(SECTIONS), true)
        .expect("create_section always finds a place for the bullet");
    workspace.write_atomic(&parent_index, &updated)?;
    outcome.record(
        &parent_index,
        ReplayOp::ListBullet {
            target: index_path,
            bullet,
            section: Some(SECTIONS.to_string()),
            create_section: true,
        },
    );
    outcome.notes.push(format!(
        "created /{folder}/index.md; replace its TODO lines (and the parent's listing)"
    ));
    Ok(())
}
```

In `src/okf/ops/mod.rs`, add `pub mod log_index;` after `pub mod frontmatter;`. Add `OpOutcome::record` after `OpOutcome::touch` (Task 2 left it out because it had no caller there; Corrected, issue #91):

```rust
    /// Records that `path` was written by a replayable edit.
    pub(crate) fn record(&mut self, path: &str, op: ReplayOp) {
        self.touch(path);
        // `entry(..).or_default()` is `dict.setdefault(key, [])`.
        self.replay.entry(path.to_string()).or_default().push(op);
    }
```

Then replace the `pub use replay::ReplayOp;` line with:

```rust
pub use log_index::{ensure_index, index, log};
// `replay` names both the module and its main function: Rust keeps modules
// and functions in separate namespaces, so `ops::replay(...)` and
// `ops::replay::ReplayOp` both work.
pub use replay::{insert_index_bullet_text, log_text, replay, ReplayOp};
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test --lib -- okf::ops okf::analysis::links`
Expected: Task 2's 15 tests (14 in `okf::ops::frontmatter`, 1 in `okf::doc`), 14 new ones in `okf::ops::replay` (2 of them Hegel properties), 7 in `okf::ops::log_index`, and the links tests all pass. Run `cargo test --doc -- okf::ops` too: the `replay` and `log_text` examples pass.

- [ ] **Step 5: Extend the parity script and shim**

Replace `tests/parity/ops_script.json` with Task 2's script plus these additions:
- setup files for an index with no bullets, one with `# Sections` only, titles with brackets, and concepts in folders with and without an `index.md`;
- 16 more steps.

```json
{
  "setup": {
    "ops/flow.md": "---\ntype: Note\ntitle: Flow shapes\nverified: { by: x, at: 2026-01-01T00:00:00Z }\nopen_questions: [one, \"two, three\", { k: v }]\n---\nBody.\n",
    "ops/index.md": "# Ops\n\nNo bullets yet.\n",
    "ops/bracket-title.md": "---\ntype: Note\ntitle: \"NTP [pool] notes\"\n---\n",
    "ops/second.md": "---\ntype: Note\ntitle: Second\n---\n",
    "people/index.md": "# People\n\n# Sections\n\n* [Teams](teams/) - groups\n",
    "people/pgrace.md": "---\ntype: Person\ntitle: Pete\n---\n",
    "people/other.md": "---\ntype: Person\ntitle: Other\n---\n",
    "infra/ntp.md": "---\ntype: Service\ntitle: NTP\n---\n",
    "root-note.md": "---\ntype: Note\ntitle: Root note\n---\n",
    "nowhere/thing.md": "---\ntype: Note\ntitle: Thing\n---\n"
  },
  "steps": [
    {"cmd": "stamp", "concept": "infra/time-server.md", "field": "generated", "by": "claude-code/claude-opus-5-5"},
    {"cmd": "stamp", "concept": "/infra/time-server", "field": "verified", "by": "human:pgrace"},
    {"cmd": "stamp", "concept": "analysis/stamps.md", "field": "verified"},
    {"cmd": "stamp", "concept": "analysis/hub.md", "field": "verified"},
    {"cmd": "stamp", "concept": "infra/hosts/crlf-host.md", "field": "generated"},
    {"cmd": "stamp", "concept": "infra/hosts/no-frontmatter.md", "field": "generated"},
    {"cmd": "stamp", "concept": "ops/flow.md", "field": "verified"},
    {"cmd": "unquestion", "concept": "ops/flow.md", "question": "3"},
    {"cmd": "question", "concept": "ops/flow.md", "question": "four"},
    {"cmd": "review", "concept": "ops/flow.md", "outcome": "verified", "note": "n", "question": ["one", "five"]},
    {"cmd": "review", "concept": "analysis/hub.md", "outcome": "partial", "note": "Links checked; anchors not.", "question": ["Is the hub still needed?", "Is the hub still needed?"]},
    {"cmd": "review", "concept": "infra/time-server.md", "outcome": "verified", "note": "Re-checked", "question": ["Is the antenna on the roof?", "Who owns it?"], "by": "human:pgrace"},
    {"cmd": "review", "concept": "analysis/offsets.md", "outcome": "updated", "note": "x: y"},
    {"cmd": "question", "concept": "analysis/stamps.md", "question": "Is the clock right?"},
    {"cmd": "question", "concept": "analysis/lead.md", "question": "Does it need splitting?"},
    {"cmd": "unquestion", "concept": "analysis/stamps.md", "question": "2"},
    {"cmd": "unquestion", "concept": "infra/time-server.md", "question": "Is the antenna on the roof?"},
    {"cmd": "unquestion", "concept": "infra/time-server.md", "question": "1"},
    {"cmd": "unquestion", "concept": "analysis/lead.md", "question": "5"},
    {"cmd": "unquestion", "concept": "analysis/hub.md", "question": "no such question"},
    {"cmd": "unquestion", "concept": "analysis/backlog.md", "question": "1"},
    {"cmd": "source", "concept": "analysis/lead.md", "id": "s6", "resource": "f", "title": "Six", "author": "human:pgrace"},
    {"cmd": "source", "concept": "analysis/lead.md", "id": "s1", "resource": "again"},
    {"cmd": "source", "concept": "analysis/hub.md", "id": "web", "resource": "https://example.com/x", "title": ""},
    {"cmd": "source", "concept": "analysis/stamps.md", "id": "d", "resource": "r"},
    {"cmd": "source", "concept": "yaml/same-indent-lists.md", "id": "q", "resource": "r: s"},
    {"cmd": "log", "text": "Ran   the\n ops  script.", "kind": "Update"},
    {"cmd": "log", "text": "Second entry.", "kind": "Change"},
    {"cmd": "log", "text": "Ran the ops script.", "kind": "Update"},
    {"cmd": "index", "concept": "ops/bracket-title.md", "summary": "  brackets   escaped "},
    {"cmd": "index", "concept": "ops/second.md", "summary": "after the first"},
    {"cmd": "index", "concept": "people/pgrace.md", "summary": "me"},
    {"cmd": "index", "concept": "infra/ntp.md", "summary": "pool", "section": "concepts"},
    {"cmd": "index", "concept": "root-note.md", "summary": "at the root"},
    {"cmd": "index", "concept": "analysis/stamps.md", "summary": "s", "title": "T", "section": "Extras"},
    {"cmd": "index", "concept": "people/other.md", "summary": "x", "section": "Nowhere"},
    {"cmd": "index", "concept": "markdown/slugs.md", "summary": "slugs", "title": "Slugs"},
    {"cmd": "index", "concept": "nowhere/thing.md", "summary": "t"},
    {"cmd": "ensure_index", "folder": "ops/deep/er"},
    {"cmd": "ensure_index", "folder": "analysis/new-stuff"},
    {"cmd": "ensure_index", "folder": "brand-new"},
    {"cmd": "ensure_index", "folder": "ops"}
  ]
}
```

What the new steps cover:
- `log`:
  - a new date heading above the fixture's older one;
  - a second kind under it, newest first;
  - a duplicate, after whitespace collapses.
- `index`:
  - a title with brackets (escaped) and a summary with extra whitespace;
  - an index with no bullets or headings (`# Concepts` appended), then a second bullet after it;
  - an index with only `# Sections` (`# Concepts` created ahead of it);
  - a case-insensitive section;
  - a root concept;
  - an already-listed concept, which the check makes win over a missing section;
  - three refusals: a missing heading, no frontmatter, and no folder `index.md`.
- `ensure_index` (not an `okf.py` command; `mv` and `split` call it), with the shim printing the files it wrote:
  - two new levels under an existing index;
  - an absolute-href parent;
  - a new top-level folder, which creates the root's `# Sections`;
  - an existing folder.

In `tests/parity/okf_shim.py`, add to `OPS_DEFAULTS`:

```python
    "log": {"kind": "Update"},
    "index": {"title": None, "section": None},
    "ensure_index": {},
```

and in `run_op` replace the `try` body with:

```python
        try:
            if command == "ensure_index":
                # A helper, not a command: mv and split call it after creating
                # the folder. The output is the files it wrote, each once.
                folder = str(step["folder"])
                os.makedirs(os.path.join(work, folder), exist_ok=True)
                touched: list[str] = []
                okf.ensure_index(work, folder, touched)
                print("\n".join(dict.fromkeys(touched)))
            else:
                getattr(okf, f"cmd_{command}")(argparse.Namespace(root=work, **fields))
```

In `tests/parity.rs`, extend the `ops` import:

```rust
use okf_mcp::okf::ops::{
    ensure_index, index, log, question, review, source, stamp, unquestion, LogKind, OpContext,
    OpOutcome, SourceEntry,
};
```

and add these arms to `run_op`'s `match`, before `other =>`:

```rust
        "log" => {
            let kind = step.get("kind").map_or(LogKind::Update, |k| {
                serde_json::from_value(k.clone()).expect("a log kind")
            });
            log(workspace, &ctx, kind, text("text"))
        }
        "index" => index(
            workspace,
            &ctx,
            text("concept"),
            text("summary"),
            optional("section"),
            optional("title"),
        ),
        "ensure_index" => {
            let mut outcome = OpOutcome::default();
            ensure_index(workspace, text("folder"), &mut outcome)?;
            // The shim prints the files ensure_index wrote, in order.
            outcome.message = outcome.touched.join("\n");
            Ok(outcome)
        }
```

- [ ] **Step 6: Regenerate the golden and run the parity tests**

Run: `UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens`

Expected:
- 1 passed;
- `git diff --stat tests/golden` shows only `ops.json`;
- it now has 42 steps, 8 of them errors. The 3 new ones:
  - `no heading 'Nowhere' in people/index.md`;
  - `markdown/slugs.md has no frontmatter; add it with your editor first`;
  - `no index.md in /nowhere; create it first`;
- step 39's output is `ops/deep/er/index.md\nops/deep/index.md\nops/index.md`, with two notes: `/ops/deep` first, then `/ops/deep/er`, the order `okf.py` prints them;
- 18 changed files, among them:
  - `log.md` (`## 2026-10-27` above `## 2026-10-26`, with `Change` before `Update`);
  - the root `index.md` (the root note, then a new `# Sections` listing `brand-new/`);
  - `people/index.md` (`# Concepts` before `# Sections`).

Run: `cargo test --test parity` then `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
Expected: every test passes, including `ops_match_golden` and `live_ops_parity`.

- [ ] **Step 7: Update the design**

In "Data flow", "Write", step 3, after "add, rename or remove an `index.md` bullet.", add: "The operations are `Log { date, bullet }`, `ListBullet { target, bullet, section, create_section }` and `CreateIndex { folder }` (`okf::ops::ReplayOp`). They are stored as JSON with an `op` tag and re-applied by `okf::ops::replay`, which shares its text edits with the ops themselves. Text that no operation changes keeps its bytes; changed text gets `\n` line endings, as `okf.py` writes."

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-03-log-index-replay.md`:

```markdown
# M5 Task 3: `log`, `index`, `ensure_index` and replay (<ts>)

Task 3 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md` (milestone 5).

## Added

- `src/okf/ops/replay.rs`: `replay` (idempotent re-application onto current text),
  `log_text` (`cmd_log`'s edit) and `insert_index_bullet_text` (`insert_index_bullet`),
  plus the crate-internal `lists_target`, `missing_heading`, `folder_heading` and
  `new_index_text`; 14 unit tests, two of them Hegel properties (replay equals the op
  and is idempotent; a second branch replayed onto the first keeps every bullet once).
- `src/okf/ops/log_index.rs`: `log`, `index`, `ensure_index` (recording `Log`,
  `ListBullet` and `CreateIndex` replay ops) and the crate-internal `list_concept`
  that Task 4's `create_concept` reuses; 7 unit tests.
- Parity: the script gains `log`, `index` and `ensure_index` steps and nine setup files
  (42 steps in all); the shim runs `ensure_index` directly; `ops.json` regenerated.

## Changed

- `okf::analysis::links::INDEX_HEADING` is `pub(crate)`, shared with the index edit.
- Design, "Data flow", "Write" step 3: the three replay operations.

## Notes

- `insert_index_bullet_text` returns `None` for a missing heading, and its callers word
  okf.py's refusal with the path; `ensure_index` is `pub` for the parity test.
- Replay resolves links without directory information, so a folder link without its
  trailing slash also counts as listing that folder's `index.md`.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/okf/analysis/links.rs tests/parity/okf_shim.py tests/parity/ops_script.json tests/parity.rs tests/golden/ops.json docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: okf::ops log, index, ensure_index and idempotent replay (M5.03)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `okf::ops`: the content ops

> Interfaces note (written while planning, 2026-11-03):
> - **`NewConcept` lives in `src/okf/ops/content.rs`**, re-exported from `okf::ops` like the functions.
> - **`doc::is_top_key(key)` is new and `pub(crate)` in `src/okf/doc.rs`.** It reuses `Doc`'s own key pattern, so `create_concept` and `set_frontmatter` refuse a key that `Doc` could not find again.
> - **Machinery check:** `edit_document` refuses machinery with Task 5's `crate::okf::rules::is_machinery`, so this task depends on Task 5 (edge T5 -> T4; review of PR #88 replaced a temporary local copy).

> **Corrected (issue #94), 2026-11-03.** Three changes, made inline below (Steps 3 and 5 hold the shipped code after `cargo fmt`):
> - `edit_replaces_one_match_and_keeps_every_other_byte` replaces `" here"`, not `"here"`, which also occurs inside `there` (2 matches, so the plan's test failed with `EditMismatch`).
> - `editable_path` starts from Task 5's `rules::check_path` (normalize, inside the catalog, not machinery, which calls `is_machinery`) instead of repeating those checks, so a machinery refusal carries `check_path`'s reason; `normpath` and `escapes_root` are no longer imported here.
> - `set_frontmatter` compares the key's current lines as a borrowed slice instead of copying them.
>
> - After review of PR #109 (2026-11-03): `EditMismatch` reads "it must occur exactly once, or at least once with replace_all" (Step 1); a shared `non_blank` check makes `create_concept` refuse a blank `title` for a listing and makes `set_frontmatter` refuse a `type` that is not a non-blank string (`type must be a non-blank string; every concept needs one`); `create_concept` refuses `index_section` without `index_summary`; its doc says the concept and new folder indexes stay written when the listing is refused, so it runs on a scratch copy. A 14th test, `create_concept_lists_it_under_the_named_section`, covers `index_section`, and the edit test pins `check_path`'s machinery reason.
>
> No interface changed: `NewConcept` and the four op signatures are as in "Interfaces shared across tasks".

The four content tools of the design's "Write tools" table. `okf.py` has none of them, so there is no parity golden. They reuse M1's path rules (`new_concept_path`), renderers (`okf::yaml`) and textual frontmatter edits (`Doc`), plus Task 3's `ensure_index` and listing.

- **`create_concept`:**
  - validates the path as `new_concept_path` does, passing on its non-kebab-case note;
  - requires a `type`;
  - writes `generated: { by, at: now }` itself and refuses `generated`, `verified` and `reviewed` in the frontmatter;
  - renders the keys in `KEY_ORDER`, with other keys after them in the caller's order;
  - with `index_summary`, creates the folder's index (and its parents') as `ensure_index` does, then lists the concept the way `index` does. Every index edit is recorded as replay operations.
- **`edit_document`:** exact replacement in a concept or an `index.md`.
  - `old` must occur exactly once unless `replace_all`, otherwise the edit fails with the new `OkfError::EditMismatch` and the count.
  - A match that touches the frontmatter (or its delimiters) is refused.
  - `log.md` and machinery are refused.
  - The edit records no replay, so an edited `index.md` conflicts like any other file at publish.
- **`replace_body`:** keeps everything up to the end of the closing frontmatter delimiter, byte for byte.
- **`set_frontmatter`:**
  - refuses the five keys a dedicated tool owns, and deleting `type`;
  - a JSON null deletes the key;
  - any other value is converted to an `okf::yaml::Value` and rendered with M1's renderers as a textual `set_block`;
  - a document without frontmatter is refused with `okf.py`'s "has no frontmatter" wording (the item M2 deferred).

**Files:**
- Create: `src/okf/ops/content.rs`
- Modify: `src/okf/ops/mod.rs` (`pub mod content;`, re-exports)
- Modify: `src/okf/doc.rs` (`is_top_key`)
- Modify: `src/error.rs` (`EditMismatch`)
- Modify: `src/http/mod.rs` (`AppError::status`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Write tools", "Carried into M3 and later")
- Create: `changelog/<ts>-m5-task-04-content-ops.md`

- [ ] **Step 1: The error variant (test first)**

In `src/error.rs`, add this test to the `tests` module:

```rust
    #[test]
    fn edit_mismatch_names_the_path_and_the_count() {
        let error = OkfError::EditMismatch {
            path: "a.md".to_string(),
            matches: 2,
        };

        let body = error.to_json();

        assert!(!error.is_internal());
        assert_eq!(
            body,
            serde_json::json!({
                "error": "EditMismatch",
                "message": "a.md: the old text occurs 2 times; it must occur exactly once, \
                            or at least once with replace_all",
                "path": "a.md",
                "matches": 2,
            })
        );
    }
```

Run: `cargo test --lib -- error::tests::edit_mismatch`
Expected: compile error (no variant `EditMismatch`).

Add the variant after `Shape`:

```rust
    /// `edit_document`'s old text does not occur exactly once (or, with
    /// `replace_all`, not at all).
    #[error(
        "{path}: the old text occurs {matches} times; it must occur exactly once, \
         or at least once with replace_all"
    )]
    EditMismatch {
        /// Catalog-relative path of the document.
        path: String,
        /// How many times the old text occurs.
        matches: usize,
    },
```

In `kind`, add `OkfError::EditMismatch { .. } => "EditMismatch",` after `Shape`. In `to_json`'s `match`, add an arm before `CatalogNotSelected`:

```rust
            OkfError::EditMismatch { path, matches } => {
                body["path"] = path.as_str().into();
                // `*matches` copies the `usize` out of the borrowed field.
                body["matches"] = (*matches).into();
            }
```

In `src/http/mod.rs` `AppError::status`, add `| OkfError::EditMismatch { .. }` to the `BAD_REQUEST` arm (after `Shape`). MCP tool errors go through `mcp::tool_error`, which uses `to_json`, so they need no other code.

Run: `cargo test --lib -- error::tests http::tests`
Expected: all pass.

- [ ] **Step 2: `doc::is_top_key`**

In `src/okf/doc.rs`, add after `blocks_of`:

```rust
/// Can `key` be written as a top-level frontmatter key, one [`Doc::block`]
/// finds again (`okf.py`'s block-key pattern)?
pub(crate) fn is_top_key(key: &str) -> bool {
    TOP_KEY
        .captures(&format!("{key}:"))
        .is_some_and(|caps| &caps[1] == key)
}
```

and this test:

```rust
    #[test]
    fn top_keys_follow_the_block_pattern() {
        assert!(is_top_key("x_custom.v2-b"));
        assert!(is_top_key("a\u{b2}"));
        assert!(!is_top_key("bad key"));
        assert!(!is_top_key("1st"));
        assert!(!is_top_key("a:b"));
        assert!(!is_top_key(""));
    }
```

- [ ] **Step 3: Write the failing tests**

Create `src/okf/ops/content.rs` with only the tests module first (the code goes above it in Step 5):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::{replay, ReplayOp};
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    const ROOT_INDEX: &str = "# Root\n\n* [Infra](infra/) - systems\n";
    const EDIT_DOC: &str =
        "---\ntype: Note\ntitle: Same words\n---\nSame words here.\r\nAnd Same words there.\n";
    const SET_DOC: &str = "---\ntype: Note\n# a comment\ntitle: Old\nx_custom: 1\n---\nBody\n";

    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn object(value: Json) -> Map<String, Json> {
        match value {
            Json::Object(map) => map,
            other => panic!("expected an object, got {other}"),
        }
    }

    fn concept<'a>(
        path: &'a str,
        frontmatter: &'a Map<String, Json>,
        body: &'a str,
    ) -> NewConcept<'a> {
        NewConcept {
            path,
            frontmatter,
            body,
            index_summary: None,
            index_section: None,
        }
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn read(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    #[test]
    fn create_concept_renders_frontmatter_in_key_order() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX)]);
        let frontmatter = object(json!({
            "tags": ["time", "ntp"],
            "x_owner": "ops: team",
            "title": "NTP pool",
            "type": "Service",
            "description": "Pool servers.",
        }));

        let outcome = create_concept(
            &ws,
            &ctx(),
            &concept("/infra/ntp-pool", &frontmatter, "# NTP pool\n"),
        )
        .unwrap();

        assert_eq!(
            read(&ws, "infra/ntp-pool.md"),
            concat!(
                "---\ntype: Service\ntitle: NTP pool\ndescription: Pool servers.\n",
                "tags: [time, ntp]\ngenerated: { by: tester, at: 2026-11-03T12:00:00Z }\n",
                "x_owner: \"ops: team\"\n---\n# NTP pool\n",
            )
        );
        assert_eq!(outcome.touched, ["infra/ntp-pool.md"]);
        assert!(outcome.replay.is_empty());
        assert!(outcome.notes.is_empty());
        assert_eq!(outcome.message, "created infra/ntp-pool.md");
    }

    #[test]
    fn create_concept_lists_it_creating_folder_indexes() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX)]);
        let frontmatter = object(json!({ "type": "Note", "title": "Thing" }));
        let new = NewConcept {
            index_summary: Some("a  thing"),
            ..concept("ops/new/thing", &frontmatter, "")
        };

        let outcome = create_concept(&ws, &ctx(), &new).unwrap();

        assert_eq!(
            outcome.touched,
            [
                "ops/new/thing.md",
                "ops/new/index.md",
                "ops/index.md",
                "index.md"
            ]
        );
        assert_eq!(
            read(&ws, "ops/new/index.md"),
            "# New\n\nTODO: say what this folder holds.\n\n# Concepts\n\n* [Thing](thing.md) - a thing\n"
        );
        assert_eq!(
            read(&ws, "ops/index.md"),
            concat!(
                "# Ops\n\nTODO: say what this folder holds.\n\n# Sections\n\n",
                "* [New](new/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            read(&ws, "index.md"),
            format!("{ROOT_INDEX}\n# Sections\n\n* [Ops](ops/) - TODO: describe this folder\n")
        );
        assert_eq!(
            outcome.message,
            "created ops/new/thing.md\nops/new/index.md: * [Thing](thing.md) - a thing"
        );
        assert_eq!(outcome.notes.len(), 2);
        assert_eq!(
            outcome.replay["ops/new/index.md"].last(),
            Some(&ReplayOp::ListBullet {
                target: "ops/new/thing.md".to_string(),
                bullet: "* [Thing](thing.md) - a thing".to_string(),
                section: None,
                create_section: false,
            })
        );
        // The concept is a free edit; every index edit replays onto the original.
        assert!(!outcome.replay.contains_key("ops/new/thing.md"));
        for path in ["ops/new/index.md", "ops/index.md", "index.md"] {
            let original = (path == "index.md").then_some(ROOT_INDEX);
            assert_eq!(
                replay(path, original, &outcome.replay[path]).unwrap(),
                read(&ws, path),
                "{path}"
            );
        }
    }

    #[test]
    fn create_concept_lists_it_under_the_named_section() {
        let infra = "# Infra\n\n# Concepts\n\n# Hosts\n\n* [A](a.md) - a\n";
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX), ("infra/index.md", infra)]);
        let frontmatter = object(json!({ "type": "Host", "title": "B" }));
        let listed = |path, section| NewConcept {
            index_summary: Some("b"),
            index_section: Some(section),
            ..concept(path, &frontmatter, "")
        };

        let outcome = create_concept(&ws, &ctx(), &listed("infra/b", "Hosts")).unwrap();
        let missing = create_concept(&ws, &ctx(), &listed("infra/c", "Nope")).unwrap_err();

        assert_eq!(
            read(&ws, "infra/index.md"),
            "# Infra\n\n# Concepts\n\n# Hosts\n\n* [A](a.md) - a\n* [B](b.md) - b\n"
        );
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::ListBullet {
                target: "infra/b.md".to_string(),
                bullet: "* [B](b.md) - b".to_string(),
                section: Some("Hosts".to_string()),
                create_section: false,
            }]
        );
        assert!(matches!(missing, OkfError::Refused { .. }), "{missing:?}");
        // The listing is refused after the concept is written (see the doc).
        assert!(ws.exists("infra/c.md"));
    }

    #[test]
    fn create_concept_notes_a_name_that_is_not_kebab_case() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX)]);
        let frontmatter = object(json!({ "type": "Note" }));

        let outcome = create_concept(&ws, &ctx(), &concept("Big_Name", &frontmatter, "")).unwrap();

        assert_eq!(
            outcome.notes,
            ["note: Big_Name.md isn't short, lowercase and hyphenated"]
        );
    }

    #[test]
    fn create_concept_refusals() {
        let ws = MemWorkspace::from_files([("index.md", ROOT_INDEX), ("infra/x.md", "x\n")]);
        let untyped = object(json!({ "title": "x" }));
        let blank_type = object(json!({ "type": " " }));
        let bad_key = object(json!({ "type": "Note", "bad key": "x" }));
        let untitled = object(json!({ "type": "Note" }));

        let errors = [
            create_concept(&ws, &ctx(), &concept("a", &untyped, "")).unwrap_err(),
            create_concept(&ws, &ctx(), &concept("a", &blank_type, "")).unwrap_err(),
            create_concept(&ws, &ctx(), &concept("a", &bad_key, "")).unwrap_err(),
        ];
        let listed = NewConcept {
            index_summary: Some("s"),
            ..concept("a", &untitled, "")
        };
        let no_title = create_concept(&ws, &ctx(), &listed).unwrap_err();
        let blank = object(json!({ "type": "Note", "title": " " }));
        let blank_title = create_concept(
            &ws,
            &ctx(),
            &NewConcept {
                frontmatter: &blank,
                ..listed
            },
        )
        .unwrap_err();
        let unlisted = NewConcept {
            index_section: Some("Concepts"),
            ..concept("a", &untitled, "")
        };
        let section_only = create_concept(&ws, &ctx(), &unlisted).unwrap_err();
        let taken = create_concept(&ws, &ctx(), &concept("infra/x", &untitled, "")).unwrap_err();
        let machinery =
            create_concept(&ws, &ctx(), &concept("tools/x", &untitled, "")).unwrap_err();

        assert_eq!(
            errors.map(refusal),
            [
                "a.md: frontmatter needs a type".to_string(),
                "a.md: frontmatter needs a type".to_string(),
                "'bad key' is not a valid frontmatter key".to_string(),
            ]
        );
        assert_eq!(
            refusal(no_title),
            "a.md has no title to list it by; add one or leave out index_summary"
        );
        assert_eq!(
            refusal(blank_title),
            "a.md has no title to list it by; add one or leave out index_summary"
        );
        assert_eq!(
            refusal(section_only),
            "a.md: index_section needs index_summary"
        );
        assert!(!ws.exists("a.md"));
        assert!(matches!(taken, OkfError::InvalidPath { .. }));
        assert!(matches!(machinery, OkfError::InvalidPath { .. }));
        for key in ["generated", "verified", "reviewed"] {
            let mut stamped = untitled.clone();
            stamped.insert(key.to_string(), json!("x"));
            let error = create_concept(&ws, &ctx(), &concept("a", &stamped, "")).unwrap_err();
            assert_eq!(
                refusal(error),
                format!("{key} is written by the server (stamp, review); leave it out")
            );
        }
    }

    #[test]
    fn edit_replaces_one_match_and_keeps_every_other_byte() {
        let ws = MemWorkspace::from_files([("a.md", EDIT_DOC)]);

        let outcome = edit_document(&ws, "/a.md", " here", " HERE", false).unwrap();

        assert_eq!(
            read(&ws, "a.md"),
            "---\ntype: Note\ntitle: Same words\n---\nSame words HERE.\r\nAnd Same words there.\n"
        );
        assert_eq!(outcome.touched, ["a.md"]);
        assert!(outcome.replay.is_empty());
        assert_eq!(outcome.message, "a.md: replaced 1 occurrence(s)");
    }

    #[test]
    fn edit_needs_exactly_one_match_unless_replace_all() {
        let ws = MemWorkspace::from_files([
            ("a.md", EDIT_DOC),
            ("b.md", "---\ntype: Note\n---\nx x x\n"),
        ]);

        let many = edit_document(&ws, "a.md", "Same words", "S", false).unwrap_err();
        let none = edit_document(&ws, "a.md", "absent", "S", true).unwrap_err();
        let all = edit_document(&ws, "b.md", "x", "y", true).unwrap();
        let same = edit_document(&ws, "b.md", "y y", "y y", false).unwrap();
        let empty = edit_document(&ws, "b.md", "", "y", true).unwrap_err();

        assert!(matches!(many, OkfError::EditMismatch { matches: 3, .. }));
        assert!(matches!(none, OkfError::EditMismatch { matches: 0, .. }));
        assert_eq!(read(&ws, "b.md"), "---\ntype: Note\n---\ny y y\n");
        assert_eq!(all.message, "b.md: replaced 3 occurrence(s)");
        assert!(same.touched.is_empty());
        assert_eq!(same.message, "b.md: no change");
        assert_eq!(refusal(empty), "b.md: the old text is empty");
    }

    #[test]
    fn edit_refuses_a_match_that_touches_the_frontmatter() {
        let ws = MemWorkspace::from_files([("a.md", EDIT_DOC)]);

        let every = edit_document(&ws, "a.md", "Same words", "S", true).unwrap_err();
        let across = edit_document(&ws, "a.md", "words\n---\nSame", "x", false).unwrap_err();

        let expected = "a.md: the match on line 3 touches the frontmatter; \
                        use set_frontmatter or the bookkeeping tools";
        assert_eq!(refusal(every), expected);
        assert_eq!(refusal(across), expected);
        assert_eq!(read(&ws, "a.md"), EDIT_DOC);
    }

    #[test]
    fn edit_works_on_index_md_and_refuses_log_md_and_machinery() {
        let ws = MemWorkspace::from_files([
            ("index.md", "# Root\n\n* [A](a.md) - old\n"),
            ("log.md", "# Log\n"),
            ("AGENTS.md", "# Agents\n"),
            ("tools/notes.md", "old\n"),
            ("install/setup.md", "old\n"),
            ("notes.txt", "old\n"),
        ]);

        let index = edit_document(&ws, "index.md", "old", "new", false).unwrap();
        let refused: Vec<OkfError> = [
            "log.md",
            "AGENTS.md",
            "tools/notes.md",
            "install/setup.md",
            "notes.txt",
            "../x.md",
        ]
        .iter()
        .map(|path| edit_document(&ws, path, "old", "new", false).unwrap_err())
        .collect();
        let missing = edit_document(&ws, "nope.md", "old", "new", false).unwrap_err();

        assert_eq!(index.touched, ["index.md"]);
        assert_eq!(read(&ws, "index.md"), "# Root\n\n* [A](a.md) - new\n");
        assert!(
            refused
                .iter()
                .all(|e| matches!(e, OkfError::InvalidPath { .. })),
            "{refused:?}"
        );
        assert!(matches!(
            &refused[0],
            OkfError::InvalidPath { reason, .. } if reason == "is written by the log tool only"
        ));
        // Machinery is refused by `rules::check_path`, with its reason.
        assert!(matches!(
            &refused[2],
            OkfError::InvalidPath { path, reason }
                if path == "tools/notes.md" && reason.starts_with("is repository machinery")
        ));
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }

    #[test]
    fn replace_body_keeps_the_frontmatter_byte_for_byte() {
        let ws = MemWorkspace::from_files([
            (
                "c.md",
                "---\r\ntype: Note # kept\r\n\r\n---\r\nOld body\r\n",
            ),
            ("n.md", "old\n"),
            ("e.md", "---\ntype: Note\n---"),
            ("index.md", "# Root\n"),
        ]);

        let outcome = replace_body(&ws, "c", "New body\n").unwrap();
        replace_body(&ws, "n", "new\n").unwrap();
        replace_body(&ws, "e", "Text\n").unwrap();
        let same = replace_body(&ws, "c", "New body\n").unwrap();
        let index = replace_body(&ws, "index.md", "x").unwrap_err();

        assert_eq!(
            read(&ws, "c.md"),
            "---\r\ntype: Note # kept\r\n\r\n---\r\nNew body\n"
        );
        assert_eq!(outcome.message, "c.md: body replaced");
        assert_eq!(read(&ws, "n.md"), "new\n");
        assert_eq!(read(&ws, "e.md"), "---\ntype: Note\n---\nText\n");
        assert!(same.touched.is_empty());
        assert_eq!(same.message, "c.md: body unchanged");
        assert!(matches!(index, OkfError::InvalidPath { .. }));
    }

    #[test]
    fn set_frontmatter_inserts_at_the_key_order_place_and_replaces_in_place() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC), ("t.md", SET_DOC)]);

        let outcome = set_frontmatter(&ws, "s", "status", Some(&json!("draft"))).unwrap();
        set_frontmatter(&ws, "t", "title", Some(&json!("New: value"))).unwrap();
        let same = set_frontmatter(&ws, "t", "title", Some(&json!("New: value"))).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            "---\ntype: Note\n# a comment\ntitle: Old\nstatus: draft\nx_custom: 1\n---\nBody\n"
        );
        assert_eq!(outcome.message, "s.md: set status");
        assert_eq!(
            read(&ws, "t.md"),
            "---\ntype: Note\n# a comment\ntitle: \"New: value\"\nx_custom: 1\n---\nBody\n"
        );
        assert!(same.touched.is_empty());
        assert_eq!(same.message, "t.md: title unchanged");
    }

    #[test]
    fn set_frontmatter_renders_lists_and_converts_scalars() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC)]);

        set_frontmatter(&ws, "s", "tags", Some(&json!(["a", "b c"]))).unwrap();
        set_frontmatter(
            &ws,
            "s",
            "x_links",
            Some(&json!([{ "id": "a", "url": "u" }])),
        )
        .unwrap();
        set_frontmatter(&ws, "s", "x_custom", Some(&json!(2))).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            concat!(
                "---\ntype: Note\n# a comment\ntitle: Old\ntags: [a, b c]\n",
                "x_custom: \"2\"\nx_links:\n  - id: a\n    url: u\n---\nBody\n",
            )
        );
    }

    #[test]
    fn set_frontmatter_null_deletes_the_key() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC)]);

        let deleted = set_frontmatter(&ws, "s", "x_custom", None).unwrap();
        let again = set_frontmatter(&ws, "s", "x_custom", Some(&Json::Null)).unwrap();

        assert_eq!(
            read(&ws, "s.md"),
            "---\ntype: Note\n# a comment\ntitle: Old\n---\nBody\n"
        );
        assert_eq!(deleted.message, "s.md: deleted x_custom");
        assert!(again.touched.is_empty());
        assert_eq!(again.message, "s.md: x_custom is not set");
    }

    #[test]
    fn set_frontmatter_refusals() {
        let ws = MemWorkspace::from_files([("s.md", SET_DOC), ("n.md", "No frontmatter.\n")]);

        let no_type = set_frontmatter(&ws, "s", "type", None).unwrap_err();
        let bad_key = set_frontmatter(&ws, "s", "bad key", Some(&json!("x"))).unwrap_err();
        let bare = set_frontmatter(&ws, "n", "title", Some(&json!("x"))).unwrap_err();
        let bad_types = [json!(""), json!(" "), json!(["a"]), json!(2)]
            .map(|kind| refusal(set_frontmatter(&ws, "s", "type", Some(&kind)).unwrap_err()));

        for key in MANAGED_KEYS {
            let error = set_frontmatter(&ws, "s", key, Some(&json!("x"))).unwrap_err();
            assert_eq!(
                refusal(error),
                format!(
                    "{key} has its own tool (stamp, review, question, unquestion, add_source); \
                     set_frontmatter does not write it"
                )
            );
        }
        assert_eq!(
            refusal(no_type),
            "type cannot be deleted; every concept needs one"
        );
        assert_eq!(
            bad_types,
            ["type must be a non-blank string; every concept needs one"; 4].map(String::from)
        );
        assert_eq!(refusal(bad_key), "'bad key' is not a valid frontmatter key");
        assert_eq!(
            refusal(bare),
            "n.md has no frontmatter; add it with your editor first"
        );
        assert_eq!(read(&ws, "s.md"), SET_DOC);
    }
}
```

- [ ] **Step 4: Run them to see them fail**

Run: `cargo test --lib -- okf::ops::content`
Expected: compile errors (`create_concept`, `NewConcept` and the rest not found).

- [ ] **Step 5: Implement**

Put this above the tests in `src/okf/ops/content.rs`:

```rust
//! The content ops (design "Write tools"): `create_concept`,
//! `edit_document`, `replace_body` and `set_frontmatter`. `okf.py` has no
//! counterpart for them. They reuse its path rules (`new_concept_path`), its
//! renderers (`okf::yaml`) and its textual frontmatter edits (`Doc`), and
//! word their refusals as `okf.py` would.

use serde_json::{Map, Value as Json};

use crate::error::OkfError;
use crate::okf::doc::{is_top_key, Doc, KEY_ORDER};
use crate::okf::ops::log_index::{list_concept, Listing};
use crate::okf::ops::{
    ensure_index, load_concept, refused, save, stamp_value, LoadedConcept, OpContext, OpOutcome,
};
use crate::okf::paths::{basename, dirname, is_concept, new_concept_path, resolve_concept};
use crate::okf::pystr::{py_repr_str, py_strip};
use crate::okf::rules::check_path;
use crate::okf::workspace::Workspace;
use crate::okf::yaml::{block_item, flow_value, scalar, ScalarStyle, Value};

/// Keys a dedicated tool owns. `set_frontmatter` refuses all five;
/// `create_concept` refuses the first three, the stamps the server writes.
const MANAGED_KEYS: [&str; 5] = [
    "generated",
    "verified",
    "reviewed",
    "open_questions",
    "sources",
];

/// A concept to create (`create_concept`'s arguments).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewConcept<'a> {
    /// The requested path, with or without `.md` and a leading `/`.
    pub path: &'a str,
    /// The frontmatter keys and values; must hold a `type`, and must not
    /// hold `generated`, `verified` or `reviewed`.
    pub frontmatter: &'a Map<String, Json>,
    /// Everything after the closing `---`, written as given.
    pub body: &'a str,
    /// With a summary, the concept is listed in its folder's `index.md`.
    pub index_summary: Option<&'a str>,
    /// The heading to list it under (`index`'s `section`); refused without
    /// `index_summary`.
    pub index_section: Option<&'a str>,
}

/// A JSON value as frontmatter. Every scalar is a string in `okf.py`'s
/// model, so numbers and booleans are stored as their JSON text (and come
/// back quoted where YAML would read them as something else).
pub(crate) fn yaml_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(flag) => Value::Str(flag.to_string()),
        Json::Number(number) => Value::Str(number.to_string()),
        Json::String(text) => Value::Str(text.clone()),
        Json::Array(items) => Value::Seq(items.iter().map(yaml_value).collect()),
        Json::Object(map) => Value::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), yaml_value(value)))
                .collect(),
        ),
    }
}

/// A top-level key's lines. A scalar is quoted only when it must be; a
/// mapping, or a list of scalars, is one flow line (`{ by: x, at: y }`,
/// `[a, b]`, as stamps and tags are written); a list holding collections is
/// a block list of [`block_item`]s.
pub(crate) fn key_lines(key: &str, value: &Value) -> Vec<String> {
    match value {
        Value::Null => vec![format!("{key}:")],
        Value::Str(text) => vec![format!("{key}: {}", scalar(text, ScalarStyle::Block))],
        Value::Seq(items)
            if items
                .iter()
                .any(|item| matches!(item, Value::Seq(_) | Value::Map(_))) =>
        {
            let mut lines = vec![format!("{key}:")];
            lines.extend(items.iter().flat_map(|item| block_item(item, 2)));
            lines
        }
        Value::Seq(_) | Value::Map(_) => vec![format!("{key}: {}", flow_value(value))],
    }
}

/// `value` as a string that is not blank (Python's `str.strip()`), the
/// rule for a concept's `type` and for the `title` a listing uses.
///
/// # Returns
/// The string, or `None` for a missing, non-string or blank value.
fn non_blank(value: Option<&Json>) -> Option<&str> {
    // `and_then` chains two `Option` steps, like `x and f(x)` in Python.
    value
        .and_then(Json::as_str)
        .filter(|text| !py_strip(text).is_empty())
}

/// Refuses a key `Doc` could not find again.
fn checked_key(key: &str) -> Result<(), OkfError> {
    if is_top_key(key) {
        Ok(())
    } else {
        Err(refused(format!(
            "{} is not a valid frontmatter key",
            py_repr_str(key)
        )))
    }
}

/// Creates a concept (design "Write tools", `create_concept`).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `ctx` - the "now" and actor for the `generated` stamp.
/// * `new` - the path, frontmatter, body and optional listing.
///
/// # Returns
/// The outcome: the concept (a free edit), then any folder indexes created
/// and listings added, each with its replay ops. The message is
/// `created <path>`, plus the listing's line; the notes carry the
/// non-kebab-case warning and `ensure_index`'s "created" lines.
///
/// # Errors
/// [`OkfError::InvalidPath`] from [`new_concept_path`] (outside the catalog,
/// reserved, machinery, taken); [`OkfError::Refused`] without a non-blank
/// string `type`, for a stamp key, an invalid key, for `index_section`
/// without `index_summary`, or a listing without a non-blank `title`;
/// [`OkfError::FrontmatterParse`] if the rendered frontmatter would not
/// parse (a guard: the renderers always write parseable YAML); the
/// listing's refusals (see `index`).
///
/// Every refusal above comes before anything is written, except the
/// listing's: the concept and any new folder indexes are written before
/// the listing runs, and stay written when it is refused (a `section` that
/// is not a heading of the index). As for [`ensure_index`], run the op on a
/// scratch copy, as the write path does, and drop the copy on error.
pub fn create_concept(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    new: &NewConcept<'_>,
) -> Result<OpOutcome, OkfError> {
    let (path, note) = new_concept_path(workspace, new.path)?;
    if non_blank(new.frontmatter.get("type")).is_none() {
        return Err(refused(format!("{path}: frontmatter needs a type")));
    }
    let title = non_blank(new.frontmatter.get("title"));
    let listed_title = match (new.index_summary, new.index_section, title) {
        (None, Some(_), _) => {
            return Err(refused(format!(
                "{path}: index_section needs index_summary"
            )))
        }
        (Some(_), _, None) => {
            return Err(refused(format!(
                "{path} has no title to list it by; add one or leave out index_summary"
            )))
        }
        (_, _, title) => title,
    };
    let mut entries: Vec<(&str, Value)> = Vec::with_capacity(new.frontmatter.len() + 1);
    for (key, value) in new.frontmatter {
        checked_key(key)?;
        if MANAGED_KEYS[..3].contains(&key.as_str()) {
            return Err(refused(format!(
                "{key} is written by the server (stamp, review); leave it out"
            )));
        }
        entries.push((key.as_str(), yaml_value(value)));
    }
    entries.push(("generated", stamp_value(ctx)));
    // `sort_by_key` is stable: `KEY_ORDER` keys first, in that order, then
    // the others in the caller's order.
    entries.sort_by_key(|(key, _)| {
        KEY_ORDER
            .iter()
            .position(|k| k == key)
            .unwrap_or(KEY_ORDER.len())
    });
    let mut text = String::from("---\n");
    for line in entries
        .iter()
        .flat_map(|(key, value)| key_lines(key, value))
    {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str("---\n");
    text.push_str(new.body);
    Doc::from_text(&path, &text).parse_checked()?;
    workspace.write_atomic(&path, &text)?;
    let mut outcome = OpOutcome::default();
    outcome.touch(&path);
    // `Option` is iterable (zero or one item), so `extend` adds the note if any.
    outcome
        .notes
        .extend(note.map(|note| format!("note: {note}")));
    let mut lines = vec![format!("created {path}")];
    if let (Some(summary), Some(title)) = (new.index_summary, listed_title) {
        ensure_index(workspace, dirname(&path), &mut outcome)?;
        let listing = Listing {
            concept: &path,
            title,
            summary,
            section: new.index_section,
        };
        lines.push(list_concept(workspace, &listing, &mut outcome)?);
    }
    outcome.message = lines.join("\n");
    Ok(outcome)
}

/// An `InvalidPath` refusal of `path`.
fn invalid(path: &str, reason: &str) -> OkfError {
    OkfError::InvalidPath {
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

/// The normalized path `edit_document` may edit: a concept or an `index.md`.
///
/// [`check_path`] refuses a path outside the catalog and machinery (with
/// [`crate::okf::rules::is_machinery`]); `log.md` and every other file are
/// refused here.
fn editable_path(path: &str) -> Result<String, OkfError> {
    let normalized = check_path(path)?;
    let name = basename(&normalized);
    if name == "log.md" {
        return Err(invalid(&normalized, "is written by the log tool only"));
    }
    if !normalized.ends_with(".md") || (name != "index.md" && !is_concept(&normalized)) {
        return Err(invalid(
            &normalized,
            "edit_document edits concepts and index.md files only",
        ));
    }
    Ok(normalized)
}

/// Exact text replacement in a concept or an `index.md` (design "Write
/// tools", `edit_document`).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `path` - the document's path (a leading `/` is allowed).
/// * `old` - the exact text to find; not empty.
/// * `new` - its replacement.
/// * `replace_all` - replace every occurrence instead of requiring one.
///
/// # Returns
/// The outcome (a free edit, no replay); nothing is touched when `new`
/// equals `old`.
///
/// # Errors
/// [`OkfError::InvalidPath`] for `log.md`, machinery, any other file, or a
/// path outside the catalog; [`OkfError::NotFound`] for a missing file;
/// [`OkfError::Refused`] for an empty `old` or a match that touches the
/// frontmatter; [`OkfError::EditMismatch`] unless `old` occurs exactly once
/// (at least once with `replace_all`).
pub fn edit_document(
    workspace: &dyn Workspace,
    path: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<OpOutcome, OkfError> {
    let path = editable_path(path)?;
    if old.is_empty() {
        return Err(refused(format!("{path}: the old text is empty")));
    }
    // Byte-preserving read: every byte outside the matches is kept.
    let text = workspace.read_to_string(&path)?;
    // `match_indices` finds non-overlapping matches left to right, as
    // Python's `str.count` and `str.replace` do.
    let starts: Vec<usize> = text.match_indices(old).map(|(start, _)| start).collect();
    if starts.is_empty() || (!replace_all && starts.len() != 1) {
        return Err(OkfError::EditMismatch {
            path,
            matches: starts.len(),
        });
    }
    // The frontmatter, with both delimiters, is everything before the body.
    let body_start = text.len() - Doc::from_text(&path, &text).body().len();
    if let Some(&hit) = starts.iter().find(|&&start| start < body_start) {
        let line = text[..hit].matches('\n').count() + 1;
        return Err(refused(format!(
            "{path}: the match on line {line} touches the frontmatter; \
             use set_frontmatter or the bookkeeping tools"
        )));
    }
    let updated = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    if updated == text {
        return Ok(OpOutcome::unchanged(format!("{path}: no change")));
    }
    workspace.write_atomic(&path, &updated)?;
    let mut outcome = OpOutcome {
        message: format!("{path}: replaced {} occurrence(s)", starts.len()),
        ..OpOutcome::default()
    };
    outcome.touch(&path);
    Ok(outcome)
}

/// Replaces a concept's body, keeping its frontmatter byte for byte
/// (design "Write tools", `replace_body`).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `concept` - the concept reference.
/// * `body` - the new text after the closing `---` (the whole text for a
///   document without frontmatter).
///
/// # Returns
/// The outcome (a free edit); nothing is touched when the body is the same.
///
/// # Errors
/// As [`resolve_concept`]; the workspace's read or write error.
pub fn replace_body(
    workspace: &dyn Workspace,
    concept: &str,
    body: &str,
) -> Result<OpOutcome, OkfError> {
    let path = resolve_concept(workspace, concept)?;
    let text = workspace.read_to_string(&path)?;
    let doc = Doc::from_text(&path, &text);
    let mut updated = text[..text.len() - doc.body().len()].to_string();
    // A closing `---` at the very end has no newline to end it yet.
    if doc.has_frontmatter() && !updated.ends_with('\n') && !body.is_empty() {
        updated.push('\n');
    }
    updated.push_str(body);
    if updated == text {
        return Ok(OpOutcome::unchanged(format!("{path}: body unchanged")));
    }
    workspace.write_atomic(&path, &updated)?;
    let mut outcome = OpOutcome {
        message: format!("{path}: body replaced"),
        ..OpOutcome::default()
    };
    outcome.touch(&path);
    Ok(outcome)
}

/// Sets or deletes one descriptive frontmatter key with a textual edit
/// (design "Write tools", `set_frontmatter`): an existing key is replaced
/// where it is, a new one goes to its `KEY_ORDER` place, and every other
/// byte is kept.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `concept` - the concept reference.
/// * `key` - the top-level key.
/// * `value` - the new value; `None` or JSON null deletes the key.
///
/// # Returns
/// The outcome; nothing is touched when the key already reads that way.
///
/// # Errors
/// [`OkfError::Refused`] for a key a dedicated tool owns, for deleting
/// `type` or setting it to anything but a non-blank string, for an invalid key, and for a document without frontmatter;
/// [`OkfError::FrontmatterParse`] when the frontmatter does not parse, before
/// or after the edit; as [`resolve_concept`] otherwise.
pub fn set_frontmatter(
    workspace: &dyn Workspace,
    concept: &str,
    key: &str,
    value: Option<&Json>,
) -> Result<OpOutcome, OkfError> {
    checked_key(key)?;
    if MANAGED_KEYS.contains(&key) {
        return Err(refused(format!(
            "{key} has its own tool (stamp, review, question, unquestion, add_source); \
             set_frontmatter does not write it"
        )));
    }
    let value = value.filter(|v| !v.is_null());
    if key == "type" {
        // `create_concept`'s rule: `type` stays a non-blank string.
        match value {
            None => {
                return Err(refused(
                    "type cannot be deleted; every concept needs one".to_string(),
                ))
            }
            Some(kind) if non_blank(Some(kind)).is_none() => {
                return Err(refused(
                    "type must be a non-blank string; every concept needs one".to_string(),
                ))
            }
            Some(_) => {}
        }
    }
    let LoadedConcept { path, mut doc, .. } = load_concept(workspace, concept)?;
    // The key's current lines, borrowed from the document (no copy).
    let current = doc.block(key).map(|block| block.start..block.end);
    let verb = match value {
        None if current.is_none() => {
            return Ok(OpOutcome::unchanged(format!("{path}: {key} is not set")));
        }
        None => {
            doc.delete_block(key);
            "deleted"
        }
        Some(json) => {
            let lines = key_lines(key, &yaml_value(json));
            // `is_some_and` borrows `doc` only inside the closure, before the
            // `set_block` below needs it mutably.
            if current.is_some_and(|span| doc.frontmatter()[span] == lines[..]) {
                return Ok(OpOutcome::unchanged(format!("{path}: {key} unchanged")));
            }
            doc.set_block(key, lines);
            "set"
        }
    };
    let mut outcome = OpOutcome::default();
    save(workspace, &doc, &mut outcome)?;
    outcome.message = format!("{path}: {verb} {key}");
    Ok(outcome)
}
```

In `src/okf/ops/mod.rs`, add `pub mod content;` first among the `pub mod` lines, and:

```rust
pub use content::{create_concept, edit_document, replace_body, set_frontmatter, NewConcept};
```

In the module docs, change "the server's content edits (Task 4)" to "the server's content edits (`create_concept`, `edit_document`, `replace_body`, `set_frontmatter`)".

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib -- okf::ops okf::doc error http`
Expected: the 14 new `okf::ops::content` tests pass, along with `top_keys_follow_the_block_pattern`, `edit_mismatch_names_the_path_and_the_count` and every earlier test.

- [ ] **Step 7: Update the design**

In "Write tools":
- In the `create_concept` row, after "Renders frontmatter in `KEY_ORDER`.", add: "Other keys follow in the caller's order. Strings are quoted only when they must be; mappings and lists of scalars are written in flow style, lists of collections as block lists; JSON numbers and booleans are stored as their text."
- In the same row, after "as `ensure_index` does", add: "and lists it with its `title`; a summary without a title is refused. Index edits are recorded as replay operations, the concept itself as a free edit."
- In the `edit_document` row, after "must occur exactly once", add: "(`EditMismatch` gives the count); an empty `old_str` is refused".
- In the `set_frontmatter` row, add at the end: "Values render as in `create_concept`. A document without frontmatter is refused with `okf.py`'s 'has no frontmatter; add it with your editor first'."

In "Carried into M3 and later", in the bullet from the M2 plan's "Deliberately deferred" table, change "secret refusal on write, the no-frontmatter `set_block` error (M5)" to "secret refusal on write (M5 Task 5); the no-frontmatter `set_block` error (done in M5 Task 4: `set_frontmatter` refuses such a document)".

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-04-content-ops.md`:

```markdown
# M5 Task 4: `okf::ops`: the content ops (<ts>)

Task 4 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md` (milestone 5).

## Added

- `src/okf/ops/content.rs`: `create_concept` (path rules, required `type`, server-written
  `generated`, `KEY_ORDER` rendering, optional listing through `ensure_index` and
  `index`'s listing, recorded as replay ops), `edit_document` (exact replacement,
  `EditMismatch`, frontmatter and `log.md`/machinery refused), `replace_body`
  (frontmatter kept byte for byte) and `set_frontmatter` (textual `set_block` /
  `delete_block`, managed keys refused); `NewConcept`; 14 unit tests.
- `OkfError::EditMismatch { path, matches }`, 400 over HTTP, with `path` and `matches`
  in its JSON.
- `doc::is_top_key`.

## Changed

- Design, "Write tools": rendering, listing and refusal details; "Carried into M3 and
  later": the no-frontmatter `set_block` error is done.

## Notes

- `edit_document` checks machinery with Task 5's `rules::is_machinery`.
- JSON numbers and booleans become strings, as every scalar is in okf.py's model.
```

Run every gate. Commit:

```bash
git add src/okf/ops src/okf/doc.rs src/error.rs src/http/mod.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: okf::ops content ops: create, edit, replace_body, set_frontmatter (M5.04)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `okf::rules`: the write rules

> Interfaces note (written while planning, 2026-11-03): `OkfError::SecretDetected` carries `message: String` instead of `masked: String`.
> - **Why:** M2's `Finding` holds only the finding's message, which already shows the value masked to its first four characters (`possible secret ghp_...: GitHub token prefix ...`). The masking function is private to `scan_secrets`, and the raw value is never kept.
> - **What the message gives:** a new field would duplicate that text, or need a second scan that keeps the raw value. The message carries the masked value plus the scan's reason.
> - **Also changed:** the design's "Errors" table gets the same edit (Step 6).

`okf::rules` holds the checks every write passes before anything is stored (design "Rules enforced on every write"). The write path (Task 8) runs them on each file an op touched, and publish (Task 10) runs them again on replayed results. `revert` goes through the same path, so machinery cannot be reverted either.

- **`is_machinery`:** the files no write tool may change (Decision 4):
  - `AGENTS.md`, `CLAUDE.md` and `README.md` at the root;
  - anything under the root's `tools/`, `docs/` and `tests/`;
  - any `.claude/`, `install/` or `.git` directory at any depth, and any other `.git*` component (`.gitignore`, `.github/`).

  It is built from M1's `paths` constants. `SKIP_DIRS` (`.git`, `.claude`, `install`) applies at any depth, as the markdown walk skips those directories. `SKIP_TOP` (`docs`, `tests`) applies at the root only, as the walk does. The three root documents are `NON_CONCEPTS` minus the reserved `index.md` and `log.md`.
- **`check_path`:** normalizes a path and refuses anything outside the catalog, the root itself, and machinery.
- **`check_content`:** runs M2's `scan_secrets` on the text as lint reads it (universal newlines, so line numbers agree with `lint`). The first hit refuses the write as the new `OkfError::SecretDetected`. For a `.md` file, it also checks that any frontmatter still parses (`FrontmatterParse`, with line and message).

`rules::is_machinery` is stricter than `paths::new_concept_path`'s private machinery test, which follows `okf.py` and looks only at the first component. So a concept path such as `infra/install/x.md` passes `new_concept_path`, and the write path then refuses it here. The reverse holds for dot-directories: `new_concept_path` refuses every one (`.hidden/x.md`), while the write rules leave non-machinery ones writable, as Decision 4 lists only `.claude` and `.git*`.

**Files:**
- Create: `src/okf/rules.rs`
- Modify: `src/okf/mod.rs` (`pub mod rules;`)
- Modify: `src/error.rs` (`SecretDetected`)
- Modify: `src/http/mod.rs` (`AppError::status`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Rules enforced on every write", "Errors")
- Create: `changelog/<ts>-m5-task-05-write-rules.md`

- [ ] **Step 1: The error variant (test first)**

In `src/error.rs`, add this test to the `tests` module:

```rust
    #[test]
    fn secret_detected_names_the_rule_and_line_but_not_the_value() {
        let error = OkfError::SecretDetected {
            path: "notes.md".to_string(),
            line: 6,
            rule: "secret:assignment".to_string(),
            message: "possible secret: 'password' followed by a literal value Ab1A... \
                      (value mixes letters and digits)"
                .to_string(),
        };

        let body = error.to_json();

        assert!(!error.is_internal());
        assert_eq!(body["error"], "SecretDetected");
        assert_eq!(body["path"], "notes.md");
        assert_eq!(body["line"], 6);
        assert_eq!(body["rule"], "secret:assignment");
        assert_eq!(
            body["message"],
            "notes.md: line 6: write refused, possible secret: 'password' followed by a \
             literal value Ab1A... (value mixes letters and digits) (secret:assignment)"
        );
    }
```

Run: `cargo test --lib -- error::tests::secret_detected`
Expected: compile error (no variant `SecretDetected`).

Add the variant after `FrontmatterParse`:

```rust
    /// A write was refused by the secret scan. Only the rule, the line and
    /// the scan's message are kept; the message shows at most the value's
    /// first four characters, so the secret itself is never returned or logged.
    #[error("{path}: line {line}: write refused, {message} ({rule})")]
    SecretDetected {
        /// Catalog-relative path of the file.
        path: String,
        /// 1-based line of the first hit.
        line: usize,
        /// The scan rule, e.g. `secret:github-token`.
        rule: String,
        /// The scan's message, with the value masked.
        message: String,
    },
```

In `kind`, add `OkfError::SecretDetected { .. } => "SecretDetected",` after `FrontmatterParse`. In `to_json`'s `match`, add an arm before `CatalogNotSelected`:

```rust
            OkfError::SecretDetected {
                path, line, rule, ..
            } => {
                body["path"] = path.as_str().into();
                body["line"] = (*line).into();
                body["rule"] = rule.as_str().into();
            }
```

In `src/http/mod.rs` `AppError::status`, add `| OkfError::SecretDetected { .. }` to the `BAD_REQUEST` arm (after `FrontmatterParse`). MCP tool errors go through `to_json` (`mcp::tool_error`), which needs no other code. `tool_error` logs only the kind of a non-internal error, so the message never reaches the log.

Run: `cargo test --lib -- error::tests http::tests`
Expected: all pass.

- [ ] **Step 2: Write the failing tests**

Create `src/okf/rules.rs` with only the tests module first (the code goes above it in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A secret-shaped value built at runtime, so no source file holds one.
    fn secret_value() -> String {
        "Ab1".repeat(4)
    }

    #[test]
    fn machinery_follows_decision_4() {
        let machinery = [
            "AGENTS.md",
            "CLAUDE.md",
            "README.md",
            "tools",
            "tools/okf.py",
            ".claude/settings.json",
            "infra/.claude/notes.md",
            ".git/config",
            ".gitignore",
            ".github/workflows/ci.yml",
            "infra/.gitattributes",
            "install/setup.md",
            "infra/install/x.md",
            "docs/plan.md",
            "tests/x.md",
        ];
        let writable = [
            "index.md",
            "log.md",
            "infra/index.md",
            "infra/time-server.md",
            "infra/AGENTS.md",
            "infra/README.md",
            "infra/docs/x.md",
            "infra/tests/x.md",
            "infra/tools/x.md",
            ".hidden/notes.md",
            "gitlab.md",
            "toolsmith.md",
            "installation.md",
            "docs.md",
        ];

        let wrongly_writable: Vec<&str> =
            machinery.into_iter().filter(|p| !is_machinery(p)).collect();
        let wrongly_machinery: Vec<&str> =
            writable.into_iter().filter(|p| is_machinery(p)).collect();

        assert!(wrongly_writable.is_empty(), "{wrongly_writable:?}");
        assert!(wrongly_machinery.is_empty(), "{wrongly_machinery:?}");
    }

    #[test]
    fn check_path_normalizes_and_refuses_what_is_outside_or_machinery() {
        let refused = ["../x.md", "a/../../x.md", "", "/", ".", "tools/okf.py", "./AGENTS.md"];

        let normalized = [
            check_path("/infra/./hosts/../x.md").unwrap(),
            check_path("infra//x.md").unwrap(),
        ];
        let errors: Vec<OkfError> = refused
            .iter()
            .map(|path| check_path(path).unwrap_err())
            .collect();

        assert_eq!(normalized, ["infra/x.md", "infra/x.md"]);
        assert!(
            errors.iter().all(|e| matches!(e, OkfError::InvalidPath { .. })),
            "{errors:?}"
        );
        assert!(matches!(
            &errors[5],
            OkfError::InvalidPath { path, reason }
                if path == "tools/okf.py" && reason.contains("machinery")
        ));
    }

    #[test]
    fn check_content_refuses_a_secret_on_the_line_lint_reports() {
        // A lone `\r` ends a line for lint's universal-newline read.
        let text = format!("---\ntype: Note\n---\nold\rmac\npassword = {}\n", secret_value());

        let error = check_content("notes.md", &text).unwrap_err();

        let OkfError::SecretDetected {
            path,
            line,
            rule,
            message,
        } = &error
        else {
            panic!("expected SecretDetected, got {error:?}");
        };
        assert_eq!((path.as_str(), *line, rule.as_str()), ("notes.md", 6, "secret:assignment"));
        assert_eq!(
            message,
            "possible secret: 'password' followed by a literal value Ab1A... \
             (value mixes letters and digits)"
        );
        assert!(!error.to_json().to_string().contains(&secret_value()));
    }

    #[test]
    fn check_content_passes_placeholders_and_unparsed_non_markdown() {
        let placeholder = check_content("a.md", "---\ntype: Note\n---\npassword = changeme\n");
        let not_markdown = check_content("notes.txt", "---\nbroken: [\n---\n");
        let bare = check_content("b.md", "No frontmatter: at all\n");

        assert!(placeholder.is_ok());
        assert!(not_markdown.is_ok());
        assert!(bare.is_ok());
    }

    #[test]
    fn check_content_refuses_markdown_whose_frontmatter_does_not_parse() {
        let error = check_content("a.md", "---\ntype: Note\ntype: again\n---\n").unwrap_err();

        assert!(matches!(
            error,
            OkfError::FrontmatterParse { ref path, line: 3, .. } if path == "a.md"
        ));
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib -- okf::rules`
Expected: compile errors (`rules` is not a module of `okf`).

- [ ] **Step 4: Implement**

> **Corrected (issue #92), 2026-11-03.** `is_machinery` takes the first component with one `split_once` instead of `split('/').next().unwrap_or_default()` plus a separate `contains('/')`; behaviour is unchanged. Step 7 also marks the design's "Open question for M5" about the machinery list as decided. The rest of this task's code compiled and passed as written.

Add `pub mod rules;` to `src/okf/mod.rs` (alphabetical, after `pystr`). Put this above the tests in `src/okf/rules.rs`:

```rust
//! The rules every write obeys before anything is stored (design "Rules
//! enforced on every write"):
//! - paths stay inside the catalog;
//! - machinery is read-only;
//! - new content holds no likely secret;
//! - a markdown file's frontmatter still parses.
//!
//! The write path (`crate::write`) runs [`check_path`] and [`check_content`]
//! on every file an op touched, and publish runs them again on replayed
//! results. `revert` goes through the same path, so machinery cannot be
//! reverted either. Only import writes machinery.

use crate::error::OkfError;
use crate::okf::analysis::secrets::scan_secrets;
use crate::okf::doc::Doc;
use crate::okf::paths::{escapes_root, normpath, NON_CONCEPTS, RESERVED, SKIP_DIRS, SKIP_TOP};
use crate::okf::pystr::universal_newlines;

/// The catalog's own tooling directory, at the root (`okf.py` lives there).
const TOOLS: &str = "tools";

/// Is this catalog-relative path repository machinery, read-only to every
/// write tool (Decision 4)?
///
/// Machinery is:
/// - `AGENTS.md`, `CLAUDE.md` and `README.md` at the root ([`NON_CONCEPTS`]
///   without the reserved `index.md` and `log.md`);
/// - anything under the root's `tools/` and the [`SKIP_TOP`] directories
///   (`docs/`, `tests/`);
/// - any [`SKIP_DIRS`] directory (`.git`, `.claude`, `install`) at any
///   depth, and any other component starting with `.git` (`.gitignore`,
///   `.github/`).
///
/// # Arguments
/// * `path` - a normalized catalog-relative path (see [`check_path`]).
///
/// # Returns
/// `true` for machinery.
///
/// # Examples
/// ```
/// use okf_mcp::okf::rules::is_machinery;
/// assert!(is_machinery("tools/okf.py"));
/// assert!(is_machinery("infra/install/setup.md"));
/// assert!(!is_machinery("infra/docs/notes.md"));
/// ```
pub fn is_machinery(path: &str) -> bool {
    // `split_once` is Python's `str.partition` without the separator: `None`
    // when there is no `/`, i.e. a file at the catalog root.
    let (first, nested) = match path.split_once('/') {
        Some((first, _)) => (first, true),
        None => (path, false),
    };
    let root_doc = !nested && NON_CONCEPTS.contains(&path) && !RESERVED.contains(&path);
    root_doc
        || first == TOOLS
        || SKIP_TOP.contains(&first)
        || path
            .split('/')
            .any(|part| SKIP_DIRS.contains(&part) || part.starts_with(".git"))
}

/// Normalizes a path a write targets and checks that a write tool may
/// change it.
///
/// # Arguments
/// * `path` - catalog-relative; a leading `/` means the catalog root.
///
/// # Returns
/// The normalized path (`/infra/./x.md` becomes `infra/x.md`).
///
/// # Errors
/// [`OkfError::InvalidPath`] for a path outside the catalog (`..`), the
/// root itself, or machinery ([`is_machinery`]).
pub fn check_path(path: &str) -> Result<String, OkfError> {
    let normalized = normpath(path.trim_start_matches('/'));
    if normalized == "." || escapes_root(&normalized) {
        return Err(OkfError::InvalidPath {
            path: path.to_string(),
            reason: "not a path inside the catalog".to_string(),
        });
    }
    if is_machinery(&normalized) {
        return Err(OkfError::InvalidPath {
            path: normalized,
            reason: "is repository machinery, read-only to every write tool; only import \
                     writes it"
                .to_string(),
        });
    }
    Ok(normalized)
}

/// Checks a file's new content: no likely secret, and (for markdown)
/// frontmatter that parses.
///
/// # Arguments
/// * `path` - the normalized path, reported in errors.
/// * `text` - the complete new content.
///
/// # Errors
/// [`OkfError::SecretDetected`] for the first hit of the secret scan, on
/// the line `lint` would report (the scan reads universal-newline text);
/// [`OkfError::FrontmatterParse`] for a `.md` file whose frontmatter does
/// not parse.
pub fn check_content(path: &str, text: &str) -> Result<(), OkfError> {
    // `into_iter().next()` takes the first finding, by value.
    if let Some(hit) = scan_secrets(path, &universal_newlines(text)).into_iter().next() {
        return Err(OkfError::SecretDetected {
            path: path.to_string(),
            line: hit.line,
            rule: hit.rule,
            message: hit.message,
        });
    }
    if path.ends_with(".md") {
        let doc = Doc::from_text(path, text);
        if doc.has_frontmatter() {
            doc.parse_checked()?;
        }
    }
    Ok(())
}
```

- [ ] **Step 5: (none)**

Task 4 depends on this task and calls `rules::is_machinery` itself.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib -- okf::rules error http` and `cargo test --doc -- okf::rules`
Expected: 5 `okf::rules` tests, `secret_detected_names_the_rule_and_line_but_not_the_value` and the `is_machinery` example pass. If Step 5 applied, `cargo test --lib -- okf::ops::content` still passes.

- [ ] **Step 7: Update the design**

In "Rules enforced on every write, before anything is stored":
- In the "Secret scan" bullet, replace "reports the rule and line with the value masked." with "reports the rule and line, with the scan's message, which shows the value masked to its first four characters. The scan reads the text with universal newlines, as `lint` does, so both report the same line."
- Replace the "Machinery is read-only" bullet with:

  ```markdown
  - **Machinery is read-only** through every tool, `revert` included (`okf::rules::is_machinery`):
    `AGENTS.md`, `CLAUDE.md` and `README.md` at the root; the root's `tools/`, `docs/` and
    `tests/` (the template's own development docs and tests); `.claude/`, `install/` (the
    template's setup machinery) and `.git` at any depth, and any other `.git*` component. Only
    import writes them. (`install/`, `docs/` and `tests/`: M5 Decision 4, matching `okf.py`,
    which skips them as non-catalog content since 481357b.)
  ```

In "Errors", change the `SecretDetected { path, line, rule, masked }` row to `SecretDetected { path, line, rule, message }`, with the meaning "Write refused by the secret scan; `message` shows the value masked".

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-05-write-rules.md`:

```markdown
# M5 Task 5: `okf::rules`: the write rules (<ts>)

Task 5 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md` (milestone 5).

## Added

- `src/okf/rules.rs`: `is_machinery` (Decision 4, built from M1's `paths` constants),
  `check_path` (normalized, inside the catalog, not machinery) and `check_content` (the
  first secret-scan hit, then frontmatter parsing for `.md` files); 5 unit tests and a
  doc example.
- `OkfError::SecretDetected { path, line, rule, message }`, 400 over HTTP, with `path`,
  `line` and `rule` in its JSON.

## Changed

- Design: "Rules enforced on every write" (the machinery list for Decision 4, the masked
  message) and "Errors" (`message` instead of `masked`).

## Notes

- `message` replaces the design's `masked`: M2's `Finding` keeps only the scan's message,
  which already masks the value, and the raw value is never held.
- `rules::is_machinery` is stricter than `new_concept_path`'s okf.py check at depth
  (`infra/install/`), and looser for non-machinery dot-directories, as Decision 4 lists
  only `.claude` and `.git*`.
```

Run every gate. Commit:

```bash
git add src/okf/rules.rs src/okf/mod.rs src/error.rs src/http/mod.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git add src/okf/ops/content.rs 2>/dev/null || true  # only if Step 5 applied
git commit -m "feat: okf::rules: machinery, path and content checks for writes (M5.05)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: connection identity: the changeset label and the session binding

> **Corrected (issue #95), 2026-11-03.** This task's code compiled and passed as written; only `rustfmt` reflowed `ToolContext::from_parts`'s last expression. The File map credited `mcp::testing::app_at` to this task, but Task 7 adds it (its Interfaces note (6) and Files list), so the File map now says T7. `tests/server.rs`'s module doc also names the session binding.

> Interfaces note: (1) `selection` also exports `SESSION_HEADER` (`"mcp-session-id"`, the name `http` stores rmcp's `Mcp-Session-Id` under) and `DEFAULT_LABEL` (`"default"`), used by `http::sessions` (never by `connection_label`: a session id is not a label, Decision 1). (2) `ToolContext` gains `with_label(self, label) -> Self`, so tests (and Task 7, Task 12) can build a context on another label; `ToolContext::new` keeps its three arguments and the label `"default"`. (3) `SessionKeys` has `new(idle)`, `bind(session, key_id, now)`, `admits(session, key_id, now) -> bool` and `release(session)`, each taking an `Instant` from the caller so tests need no sleep. (4) A request whose `Mcp-Session-Id` has **no** binding also gets 404: every session rmcp issues passes through the middleware and is bound, so an unbound id is one whose binding idled out (or that never existed), and refusing it means pruning can never let a second key in. (5) A successful `DELETE /mcp` releases the binding. (6) `mcp::testing` gains `principal_as`, `store_key`, `PendingEdit` and `pending_entry`, used here and by Tasks 7 and 12. (7) **Dependency:** Task 7's `ToolContext::view` reads `label()`, so Task 7 needs this task too (add the edge T6 -> T7 to "Task dependencies").

Decision 1's label and Decision 2's binding. Every tool call's context carries its connection's changeset label: the `X-OKF-Changeset` header, else `?changeset=` on the MCP URL, else `default` (never the session id; see Decision 1). A new middleware, `http::sessions::bind_session`, sits on `/mcp` inside the key layer: it records which key's request opened each MCP session, and answers 404 (rmcp's own answer for an unknown session) to a request that names a session bound to another key, or bound to none. That closes M4's gap: a second key that learns a session id can no longer attach to its `GET /mcp` stream, resume it, or post on it.

**Files:**
- Modify: `src/selection.rs` (`CHANGESET_HEADER`, `CHANGESET_QUERY`, `SESSION_HEADER`, `DEFAULT_LABEL`, `connection_label`)
- Modify: `src/mcp/context.rs` (`label`, `with_label`)
- Create: `src/http/sessions.rs`; modify `src/http/mod.rs` (`pub mod sessions;`, the layer)
- Modify: `src/app.rs` (`App::sessions`)
- Modify: `src/mcp/testing.rs` (helpers)
- Modify: `tests/server.rs`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Decisions", "Process and routes", "Open questions for M5")
- Create: `changelog/<ts>-m5-task-06-identity.md`

- [ ] **Step 1: Write the failing tests**

In `src/selection.rs`'s test module:

```rust
    fn label_parts(uri: &str, headers: &[(&str, &str)]) -> Parts {
        let mut request = axum::http::Request::builder().uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.body(()).unwrap().into_parts().0
    }

    #[test]
    fn the_label_is_the_header_then_the_query_then_default_never_the_session() {
        let header = label_parts(
            "/mcp?changeset=q",
            &[(CHANGESET_HEADER, " drafts "), (SESSION_HEADER, "s1")],
        );
        let query = label_parts(
            "/mcp?catalogs=pg&changeset=&changeset=q",
            &[(CHANGESET_HEADER, " "), (SESSION_HEADER, "s1")],
        );
        let session = label_parts("/mcp?changeset=", &[(SESSION_HEADER, "s1")]);
        let nothing = label_parts("/mcp", &[]);

        let labels = [&header, &query, &session, &nothing].map(connection_label);

        // A session id alone is not a label: it would change on reconnect.
        assert_eq!(labels, ["drafts", "q", DEFAULT_LABEL, DEFAULT_LABEL]);
    }
```

In `src/mcp/context.rs`'s test module (add `use chrono::Utc;`, `use crate::store::keys::Access;`, `use crate::selection::{CHANGESET_HEADER, DEFAULT_LABEL};` and extend the `testing` import with `principal_as, store_key, pending_entry, PendingEdit`):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn the_context_takes_its_label_from_the_request() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let mut request = axum::http::Request::builder()
            .uri("/mcp")
            .header(CHANGESET_HEADER, "drafts")
            .body(())
            .unwrap();
        request.extensions_mut().insert(principal(&[], Some("pg")));
        let (parts, _) = request.into_parts();

        let from_request = super::ToolContext::from_parts(&test.app, &parts).unwrap();
        let plain = context(&test, principal(&[], Some("pg")), None);
        let relabelled = context(&test, principal(&[], Some("pg")), None).with_label("other");

        assert_eq!(from_request.label(), "drafts");
        assert_eq!(plain.label(), DEFAULT_LABEL);
        assert_eq!(relabelled.label(), "other");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_connection_s_key_and_label_pick_its_changeset() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        store_key(&test, "alice").await;
        store_key(&test, "bob").await;
        let alice = || context(&test, principal_as("alice", Access::Write, &[], Some("pg")), None);
        let bob = context(&test, principal_as("bob", Access::Write, &[], Some("pg")), None);
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some("# A\n"),
            at: Utc::now(),
        };

        let first = pending_entry(&test, &alice(), &edit).await;
        let again = pending_entry(&test, &alice(), &edit).await;
        let drafts = pending_entry(&test, &alice().with_label("drafts"), &edit).await;
        let other_key = pending_entry(&test, &bob, &edit).await;

        assert_eq!(again.id, first.id);
        assert_eq!(again.version, 2);
        assert_eq!(drafts.label, "drafts");
        assert_ne!(drafts.id, first.id);
        assert_eq!(other_key.key_id.as_deref(), Some("bob"));
        assert_ne!(other_key.id, first.id);
    }
```

Create `src/http/sessions.rs` with only its tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(60);

    #[test]
    fn a_session_admits_only_the_key_that_opened_it() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();

        keys.bind("s1", "alice", start);

        assert!(keys.admits("s1", "alice", start + Duration::from_secs(1)));
        assert!(!keys.admits("s1", "bob", start + Duration::from_secs(1)));
        assert!(!keys.admits("unknown", "alice", start));
    }

    #[test]
    fn use_keeps_a_binding_and_idling_drops_it() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();
        keys.bind("busy", "alice", start);
        keys.bind("idle", "alice", start);

        // `busy` is used just inside the timeout, which restarts its clock.
        let used = keys.admits("busy", "alice", start + IDLE);
        let later = start + IDLE * 2;
        let busy_later = keys.admits("busy", "alice", later);
        let idle_later = keys.admits("idle", "alice", later);

        assert!(used);
        assert!(busy_later);
        assert!(!idle_later, "an idle binding admits no one");
    }

    #[test]
    fn binding_a_new_session_prunes_idle_ones_and_release_drops_one() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();
        keys.bind("old", "alice", start);
        keys.bind("closed", "alice", start + IDLE * 2);

        keys.bind("new", "bob", start + IDLE * 2);
        keys.release("closed");

        assert_eq!(keys.len(), 1);
        assert!(keys.admits("new", "bob", start + IDLE * 2));
    }
}
```

In `tests/server.rs`, add:

```rust
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// Opens an MCP session with `key`.
///
/// # Returns
/// The `Mcp-Session-Id` the server issued.
async fn open_session(server: &TestServer, key: &str) -> String {
    let response = reqwest::Client::new()
        .post(server.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .bearer_auth(key)
        .body(INITIALIZE)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response
        .headers()
        .get("mcp-session-id")
        .expect("a session id")
        .to_str()
        .unwrap()
        .to_string()
}

/// Posts `notifications/initialized` on `session` with `key`.
///
/// # Returns
/// The status: 202 when rmcp accepts it.
async fn notify_on(server: &TestServer, key: &str, session: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(server.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-11-25")
        .bearer_auth(key)
        .body(INITIALIZED)
        .send()
        .await
        .unwrap()
        .status()
}

/// Opens `session`'s event stream (`GET /mcp`) with `key`.
///
/// # Returns
/// The status (the body, a stream, is never read).
async fn stream_on(server: &TestServer, key: &str, session: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(server.mcp_url())
        .header("accept", "text/event-stream")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", "2025-11-25")
        .bearer_auth(key)
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_answers_only_the_key_that_opened_it() {
    let server = TestServer::start(pg(), Config::default()).await;
    let owner = server.key("owner", Access::Read, &[], Some("pg")).await;
    let intruder = server.key("intruder", Access::Read, &[], Some("pg")).await;
    let session = open_session(&server, &owner).await;

    let foreign_post = notify_on(&server, &intruder, &session).await;
    let foreign_stream = stream_on(&server, &intruder, &session).await;
    let unknown = notify_on(&server, &owner, "not-a-session").await;
    let own = notify_on(&server, &owner, &session).await;

    assert_eq!(foreign_post, 404);
    assert_eq!(foreign_stream, 404);
    assert_eq!(unknown, 404);
    assert_eq!(own, 202);
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib -- selection:: mcp::context http::sessions` and `cargo test --test server`
Expected: compile errors (`connection_label`, `label`, `SessionKeys`, `principal_as` ... not found).

- [ ] **Step 3: The label**

In `src/selection.rs`, extend the module doc with a paragraph ("A connection's changeset label (Decision 1 of M5): the `X-OKF-Changeset` header, else `?changeset=`, else `default`. Never the MCP session id, which changes on every reconnect."), and add after `CATALOGS_QUERY`:

```rust
/// The header naming a connection's changeset label (M5 Decision 1).
pub const CHANGESET_HEADER: &str = "x-okf-changeset";

/// The MCP URL's query parameter with the same meaning.
pub const CHANGESET_QUERY: &str = "changeset";

/// rmcp's session header (`Mcp-Session-Id`), as `http` stores header names:
/// lowercase. Lookups by it match any case on the wire.
pub const SESSION_HEADER: &str = "mcp-session-id";

/// The label of a connection that names none: its key's one unnamed
/// changeset per catalog, the same across reconnects and restarts.
pub const DEFAULT_LABEL: &str = "default";
```

and after `connection_catalogs`:

```rust
/// A label as given, trimmed; `None` when blank.
fn label_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A header's value as text (lossily, as for [`connection_catalogs`]),
/// trimmed; `None` when blank.
fn header_label(value: &axum::http::HeaderValue) -> Option<String> {
    label_text(&String::from_utf8_lossy(value.as_bytes()))
}

/// The changeset label of a connection (M5 Decision 1): the first of the
/// `X-OKF-Changeset` header and the `changeset` query parameter that is not
/// blank, else [`DEFAULT_LABEL`]. Never the `Mcp-Session-Id`: it changes on
/// every reconnect, which would strand the earlier changeset.
///
/// With the key, the label picks the connection's changeset in each catalog,
/// so a client keeps one changeset across requests, reconnects and restarts,
/// and an agent that wants several names them.
///
/// # Arguments
/// * `parts` - the HTTP request (rmcp passes it to every tool).
///
/// # Returns
/// The label, trimmed.
pub fn connection_label(parts: &Parts) -> String {
    // `find_map` returns the first `Some` the closure gives, like
    // `next((f(x) for x in xs if f(x) is not None), None)` in Python.
    parts
        .headers
        .get_all(CHANGESET_HEADER)
        .iter()
        .find_map(header_label)
        .or_else(|| {
            let query = parts.uri.query()?;
            form_urlencoded::parse(query.as_bytes())
                .filter(|(key, _)| key == CHANGESET_QUERY)
                .find_map(|(_, value)| label_text(&value))
        })
        .unwrap_or_else(|| DEFAULT_LABEL.to_string())
}
```

In `src/mcp/context.rs`: import `connection_label` and `DEFAULT_LABEL` from `crate::selection`; add the field `label: String` (comment: "The connection's changeset label (M5 Decision 1)."); in `new`, `label: DEFAULT_LABEL.to_string(),`; and change `from_parts`'s last expression to:

```rust
        Ok(Self::new(Arc::clone(app), principal, connection_catalogs(parts))
            .with_label(connection_label(parts)))
```

Add the methods after `principal`:

```rust
    /// The connection's changeset label (M5 Decision 1).
    ///
    /// # Returns
    /// The label: from the request in [`ToolContext::from_parts`],
    /// [`DEFAULT_LABEL`] from [`ToolContext::new`].
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The same context on another label.
    ///
    /// # Arguments
    /// * `label` - the label; `impl Into<String>` takes a `&str` or a `String`.
    ///
    /// # Returns
    /// The context, consumed and returned (`self` by value), so calls chain.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
```

Update the module doc: "the principal, the connection's catalogs and changeset label, and one view of the catalog map".

- [ ] **Step 4: The session binding**

Put this above the test module in `src/http/sessions.rs`:

```rust
//! MCP sessions bound to the key that opened them (M5 Decision 2).
//!
//! rmcp knows a session only by its `Mcp-Session-Id`, and each request is
//! authorized by its own key, so without this a second key that learned a
//! session id could post on the session or attach to its `GET /mcp` stream.
//! [`bind_session`] records the key of the request that opens a session and
//! answers any later request on it with another key, or on a session it
//! never bound, with rmcp's own 404 for an unknown session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::App;
use crate::auth::Principal;
use crate::selection::SESSION_HEADER;

/// The body rmcp answers an unknown session with; a refused key gets the
/// same, so it learns nothing about the session.
const SESSION_NOT_FOUND: &str = "Not Found: Session not found";

/// One session's key, and when it was last used.
#[derive(Debug)]
struct Binding {
    key_id: String,
    seen: Instant,
}

/// Every live session's key, by session id.
#[derive(Debug)]
pub struct SessionKeys {
    idle: Duration,
    // A `std` mutex: held only for a map lookup or insert, never across an
    // `.await`, so it never blocks an async thread for long.
    bindings: Mutex<HashMap<String, Binding>>,
}

impl SessionKeys {
    /// An empty map whose bindings expire after `idle` without use.
    ///
    /// # Arguments
    /// * `idle` - `session_idle_timeout`, the same bound rmcp's sessions have.
    ///
    /// # Returns
    /// The map.
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            bindings: Mutex::new(HashMap::new()),
        }
    }

    /// Records that `key_id` opened `session`, first dropping every binding
    /// idle past the timeout (so the map holds live sessions only).
    ///
    /// # Arguments
    /// * `session` - the `Mcp-Session-Id` rmcp issued.
    /// * `key_id` - the key of the request that opened it.
    /// * `now` - the time of that request.
    pub fn bind(&self, session: &str, key_id: &str, now: Instant) {
        let mut bindings = self.lock();
        // `retain` keeps the entries the closure accepts: Python's
        // `{k: v for k, v in d.items() if keep(v)}`, in place.
        bindings.retain(|_, binding| now.saturating_duration_since(binding.seen) <= self.idle);
        bindings.insert(
            session.to_string(),
            Binding {
                key_id: key_id.to_string(),
                seen: now,
            },
        );
    }

    /// May `key_id` use `session`? Using it restarts its idle clock.
    ///
    /// # Arguments
    /// * `session` - the request's `Mcp-Session-Id`.
    /// * `key_id` - the request's key.
    /// * `now` - the time of the request.
    ///
    /// # Returns
    /// `true` only if `key_id` opened the session and it has not idled out;
    /// an idled-out binding is dropped.
    pub fn admits(&self, session: &str, key_id: &str, now: Instant) -> bool {
        let mut bindings = self.lock();
        let Some(binding) = bindings.get_mut(session) else {
            return false;
        };
        if now.saturating_duration_since(binding.seen) > self.idle {
            bindings.remove(session);
            return false;
        }
        if binding.key_id != key_id {
            return false;
        }
        binding.seen = now;
        true
    }

    /// Forgets a session (after its `DELETE /mcp`).
    ///
    /// # Arguments
    /// * `session` - its id.
    pub fn release(&self, session: &str) {
        self.lock().remove(session);
    }

    /// How many sessions are bound.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Binding>> {
        // Poisoned only if a thread panicked while holding it; nothing here
        // can panic with it held.
        self.bindings.lock().expect("session binding lock poisoned")
    }
}

/// The `/mcp` middleware of M5 Decision 2. It runs after `require_key`, so
/// the request carries its [`Principal`].
///
/// - A request naming a session is refused with 404 unless that session is
///   bound to this request's key.
/// - A response that issues a session (`Mcp-Session-Id` on the answer to a
///   request that named none) binds it to the request's key.
/// - A successful `DELETE` releases the binding.
///
/// # Arguments
/// * `app` - the shared state ([`App::sessions`]).
/// * `request` - the request.
/// * `next` - the rest of the stack (rmcp's service).
///
/// # Returns
/// rmcp's response, or 404.
pub async fn bind_session(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let key_id = request
        .extensions()
        .get::<Arc<Principal>>()
        .map(|principal| principal.key_id.clone());
    // Never logged: a session id lets its holder use the session.
    let session = request
        .headers()
        .get(SESSION_HEADER)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let now = Instant::now();
    if let Some(session) = &session {
        let admitted = key_id
            .as_deref()
            .is_some_and(|key| app.sessions().admits(session, key, now));
        if !admitted {
            tracing::debug!("refused a request on another key's or an unbound MCP session");
            return (StatusCode::NOT_FOUND, SESSION_NOT_FOUND).into_response();
        }
    }
    let closing = request.method() == Method::DELETE;
    let response = next.run(request).await;
    match (&session, &key_id) {
        (None, Some(key)) => {
            if let Some(issued) = response.headers().get(SESSION_HEADER) {
                app.sessions()
                    .bind(&String::from_utf8_lossy(issued.as_bytes()), key, now);
            }
        }
        (Some(session), _) if closing && response.status().is_success() => {
            app.sessions().release(session);
        }
        // A request on a bound session (already refreshed by `admits`), or
        // one without a key (only a router without `require_key`).
        _ => {}
    }
    response
}
```

In `src/http/mod.rs`: add `pub mod sessions;` beside `mod archives;` and `use crate::http::sessions::bind_session;`, extend the module doc ("... and the key layer, and on `/mcp` the session binding (`sessions`)"), and build `keyed` as:

```rust
    // Layers wrap what is added before them, so a layer added later runs
    // first on the way in: `require_key` (on `keyed`) runs before
    // `bind_session` (on `mcp`), which can then read the principal.
    let mcp = Router::new()
        .route_service("/mcp", mcp_service(&app, shutdown))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&app),
            bind_session,
        ));
    // `route_layer` applies to the routes added before it, merged ones
    // included, so every keyed route goes above it.
    let keyed = Router::new()
        .merge(mcp)
        .merge(archives::routes(&app))
        .route_layer(middleware::from_fn_with_state(keys, require_key));
```

In `src/app.rs`: add `use crate::http::sessions::SessionKeys;`, the field

```rust
    // Which key opened each MCP session (M5 Decision 2).
    sessions: SessionKeys,
```

in `load`, before building `Self`, `let sessions = SessionKeys::new(config.session_idle_timeout);` and `sessions,` in the struct literal, and the accessor after `keys`:

```rust
    /// Which key opened each MCP session (the `/mcp` session binding).
    pub fn sessions(&self) -> &SessionKeys {
        &self.sessions
    }
```

- [ ] **Step 5: Test helpers for the tasks that follow**

In `src/mcp/testing.rs` (new imports: `chrono::DateTime`, `crate::store::changesets::{self, ChangesetKey, ChangesetRow, EntryRow}`, `crate::store::keys::{self as key_rows, Grants, NewKey}`), replace `principal` with these two:

```rust
/// A key with this id and access, reaching `reach` (every catalog when
/// empty), with a default.
///
/// # Arguments
/// * `key_id` - its id (also its actor).
/// * `access` - its access level.
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal_as(
    key_id: &str,
    access: Access,
    reach: &[&str],
    default: Option<&str>,
) -> Arc<Principal> {
    Arc::new(Principal {
        key_id: key_id.to_string(),
        access,
        reach: if reach.is_empty() {
            Reach::All
        } else {
            Reach::Catalogs(reach.iter().map(|name| name.to_string()).collect())
        },
        default_catalog: default.map(str::to_string),
        actor: key_id.to_string(),
    })
}

/// A `read` key `tester` reaching `reach` (every catalog when empty), with
/// a default.
///
/// # Arguments
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal(reach: &[&str], default: Option<&str>) -> Arc<Principal> {
    principal_as("tester", Access::Read, reach, default)
}
```

and add:

```rust
/// Stores a key row, so changesets can name it (`changesets.key_id` is a
/// foreign key). Its stored access does not matter: tests build the
/// principal with [`principal_as`].
///
/// # Arguments
/// * `test` - the app.
/// * `key_id` - the key's id.
///
/// # Panics
/// If the store refuses it.
pub(crate) async fn store_key(test: &TestApp, key_id: &str) {
    let new = NewKey {
        id: key_id,
        access: Access::Admin,
        grants: Grants::All,
        default_catalog: None,
        actor: key_id,
    };
    key_rows::create(test.app.pool(), &new, Utc::now())
        .await
        .expect("store the test key");
}

/// One pending edit, for [`pending_entry`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingEdit<'a> {
    /// The catalog's name.
    pub(crate) catalog: &'a str,
    /// Catalog-relative path.
    pub(crate) path: &'a str,
    /// The new text; `None` deletes the path.
    pub(crate) text: Option<&'a str>,
    /// The time of the edit.
    pub(crate) at: DateTime<Utc>,
}

/// Stores an edit in the changeset of `ctx`'s key and label, in the lock
/// order (opening the changeset if need be), as a write tool would.
///
/// # Arguments
/// * `test` - the app.
/// * `ctx` - the connection: its key and label pick the changeset.
/// * `edit` - the catalog, path, text and time.
///
/// # Returns
/// The changeset after the edit.
///
/// # Panics
/// If the catalog is not loaded or the store fails.
pub(crate) async fn pending_entry(
    test: &TestApp,
    ctx: &ToolContext,
    edit: &PendingEdit<'_>,
) -> ChangesetRow {
    let state = test.app.catalog(edit.catalog).expect("a loaded catalog");
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &ctx.principal().key_id,
        label: ctx.label(),
    };
    let entry = EntryRow {
        path: edit.path.to_string(),
        base_revision: None,
        content: edit.text.map(|text| text.as_bytes().to_vec()),
        executable: false,
        replay: None,
    };
    let mut tx = test.app.pool().begin().await.expect("begin");
    changesets::lock_catalog(&mut tx, state.id())
        .await
        .expect("lock the catalog");
    let changeset = changesets::get_or_create(&mut tx, &key, &ctx.principal().actor, edit.at)
        .await
        .expect("open the changeset");
    changesets::upsert_entry(&mut tx, changeset.id, &entry, edit.at)
        .await
        .expect("store the entry");
    tx.commit().await.expect("commit");
    changesets::by_id(test.app.pool(), changeset.id)
        .await
        .expect("read the changeset back")
        .expect("the changeset exists")
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib -- selection:: mcp::context http::sessions` and `cargo test --test server`
Expected: the new unit tests pass (1 in `selection`, 2 in `mcp::context`, 3 in `http::sessions`), and `tests/server.rs` passes with `a_session_answers_only_the_key_that_opened_it`; the older end-to-end tests, whose rmcp clients open sessions, now go through the binding too. Then `cargo test`.

- [ ] **Step 7: Update the design**

1. "Decisions", the "Auth" row: append "; an MCP session answers only the key that opened it".
2. "Process and routes", the `/mcp` row: append "Each `Mcp-Session-Id` is bound to the key whose request opened it; a request naming it with another key, or naming a session with no binding (idle past `session_idle_timeout`, or never issued), gets 404, rmcp's answer for an unknown session". After the table, in the middleware paragraph: "`/mcp` also has the session binding, inside the auth layer."
3. "Open questions for M5": delete the "Binding it to a key" bullet.

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-06-identity.md`: the label's sources and `ToolContext::{label, with_label}`; `SessionKeys` and `bind_session` (404 for another key's or an unbound session, release on `DELETE`, idle pruning on bind); the router's layer order; the testing helpers; the tests; the Interfaces notes above (including the T6 -> T7 dependency). Run every gate. Commit:

```bash
git add src/selection.rs src/mcp/context.rs src/mcp/testing.rs src/http/ src/app.rs \
    tests/server.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "$(cat <<'EOF'
feat: changeset labels per connection; MCP sessions bound to their key (M5.06)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: overlays, and the read tools through them

> Interfaces note: (1) Two small additions under the overlay: `MemWorkspace::with_changes(&self, changes) -> Result<MemWorkspace, OkfError>` (a copy with writes and deletes applied, checked as `from_stored` checks; `MemWorkspace` has no `Clone` and no delete), and `CommitTimes::with_dirty(&self, dirty) -> CommitTimes` (`CommitTimes`' maps are private, so `CommitTimes::new(times, dirty)` cannot be rebuilt from a `Loaded`). Task 8's "op runs on a clone of the overlay workspace" is `overlay.workspace().with_changes([])`. (2) `Overlay` also has `changeset_id()` and `version()`; `Overlays::new(idle)` drops entries unused for `session_idle_timeout` when it stores one, so closed changesets do not pile up. (3) `ToolContext::view` returns `View::Published` without a query for a `read` key (it can never hold a changeset), so plain readers pay nothing. (4) `get_concept` and `read_document` become `async`. (5) The `lint` tool takes `LintParams { catalog, pending }` instead of `CatalogParam`, and `Lint` gains `pending: bool`. (6) `mcp::testing::app_with` delegates to a new `app_at(catalogs, config, imported, clock)`. (7) **Dependency:** this task needs Task 6 (`ToolContext::label`, the testing helpers) as well as Task 1.

M5 Decision 6. `Loaded` keeps the published `MemWorkspace` (which `catalog::build` used to drop) and each path's revision. An `Overlay` is that workspace with a changeset's entries applied (a `content: None` entry deletes its path) plus a full `Snapshot::build` of it, made on a blocking thread. `Overlays` caches one per changeset id, valid while the changeset's `version` and the base's `generation` are both unchanged. `ToolContext::view` gives a tool the published state, or, when this key and label have an open changeset in the catalog (one index lookup), its overlay. Every read tool that reads a snapshot goes through it, so an agent sees its own pending edits and nobody else's. `lint` gains `pending`: lint the overlay, with the changeset's paths counted as uncommitted for `generated-after-commit`. `history` reads revisions from the database, which a pending edit does not have yet, so it is unchanged; so is `list_catalogs`, which reports published counts.

**Files:**
- Modify: `src/okf/workspace.rs` (`MemWorkspace::with_changes`)
- Modify: `src/okf/analysis/lint.rs` (`CommitTimes::with_dirty`)
- Modify: `src/catalog.rs` (`Loaded::{workspace, revision}`)
- Create: `src/overlay.rs`; modify `src/lib.rs` (`pub mod overlay;`)
- Modify: `src/app.rs` (`App::overlays`)
- Modify: `src/mcp/context.rs` (`ToolContext::view`)
- Modify: `src/mcp/search.rs`, `src/mcp/documents.rs`, `src/mcp/reports.rs`
- Modify: `src/mcp/testing.rs` (`app_at`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("In-memory snapshot", "Read tools", "Data flow")
- Create: `changelog/<ts>-m5-task-07-overlays.md`

- [ ] **Step 1: Write the failing tests**

`src/okf/workspace.rs`, in the test module:

```rust
    #[test]
    fn with_changes_copies_the_files_and_applies_writes_and_deletes() {
        let base = MemWorkspace::from_stored([
            ("a.md".to_string(), stored(b"# A\n", false)),
            ("b.md".to_string(), stored(b"# B\n", false)),
            ("run.sh".to_string(), stored(b"#!/bin/sh\n", true)),
        ])
        .unwrap();

        let changed = base
            .with_changes([
                ("a.md".to_string(), Some(stored(b"# A2\n", false))),
                ("b.md".to_string(), None),
                ("new/c.md".to_string(), Some(stored(b"# C\n", false))),
            ])
            .unwrap();
        let clash = base.with_changes([("a.md/x.md".to_string(), Some(stored(b"x", false)))]);
        let escape = base.with_changes([("../x.md".to_string(), None)]);

        assert_eq!(changed.read_to_string("a.md").unwrap(), "# A2\n");
        assert!(!changed.is_file("b.md"));
        assert_eq!(changed.read_to_string("new/c.md").unwrap(), "# C\n");
        assert_eq!(changed.stored("run.sh"), Some(stored(b"#!/bin/sh\n", true)));
        // The original is a separate copy, unchanged.
        assert_eq!(base.read_to_string("a.md").unwrap(), "# A\n");
        assert!(base.is_file("b.md"));
        assert!(matches!(clash, Err(OkfError::InvalidPath { .. })));
        assert!(matches!(escape, Err(OkfError::InvalidPath { .. })));
    }
```

`src/okf/analysis/lint.rs`, in the test module (it already has `chrono` in scope; add `use std::collections::{HashMap, HashSet};` there if it is not):

```rust
    #[test]
    fn with_dirty_marks_more_paths_uncommitted_and_leaves_the_original() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-03T12:00:00+00:00").unwrap();
        let times: HashMap<String, _> = [("a.md".to_string(), at), ("b.md".to_string(), at)]
            .into_iter()
            .collect();
        let clean = CommitTimes::new(times, HashSet::new());

        let marked = clean.with_dirty(["a.md".to_string()]);

        assert_eq!(marked.committed_clean("a.md"), None);
        assert_eq!(marked.committed_clean("b.md"), Some(at));
        assert_eq!(clean.committed_clean("a.md"), Some(at));
    }
```

`src/catalog.rs`, in the test module:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn loaded_keeps_the_workspace_and_each_path_s_revision() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        // Typed, so the byte strings of different lengths share one type.
        let first: &[(&str, &[u8])] = &[("a.md", b"# A\n"), ("logo.png", b"\x89PNG\0")];
        let second: &[(&str, &[u8])] = &[("a.md", b"# A2\n"), ("logo.png", b"\x89PNG\0")];
        import_into(&db.pool, first, false).await;
        import_into(&db.pool, second, true).await;

        let loaded = CatalogState::load(&db.pool, "pg").await.unwrap().current();

        assert_eq!(loaded.workspace().read_to_string("a.md").unwrap(), "# A2\n");
        assert!(loaded.workspace().is_file("logo.png"));
        assert_eq!(loaded.revision("a.md"), Some(2));
        assert_eq!(loaded.revision("logo.png"), Some(1));
        assert_eq!(loaded.revision("gone.md"), None);
    }
```

(add `use crate::okf::workspace::Workspace;` to that test module for `read_to_string` and `is_file`).

Create `src/overlay.rs` with only its tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::files;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, pending_entry, principal_as, store_key, PendingEdit};
    use crate::okf::workspace::Workspace;
    use crate::store::changesets::ChangesetState;
    use crate::store::import::{import, ImportOptions};
    use crate::store::keys::Access;

    fn entry(path: &str, text: Option<&str>) -> EntryRow {
        EntryRow {
            path: path.to_string(),
            base_revision: None,
            content: text.map(|text| text.as_bytes().to_vec()),
            executable: false,
            replay: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_overlay_shows_the_entries_over_the_published_files() {
        let test = app_with(
            &[("pg", &[("a.md", "# Old\n"), ("b.md", "# B\n")])],
            Config::default(),
        )
        .await;
        let base = test.app.catalog("pg").unwrap().current();
        let changeset = ChangesetRow {
            id: 7,
            catalog_id: 1,
            label: "default".to_string(),
            key_id: None,
            actor: "t".to_string(),
            state: ChangesetState::Open,
            version: 3,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let entries = vec![
            entry("a.md", Some("# New\n")),
            entry("b.md", None),
            entry("c.md", Some("# C\n")),
        ];

        let overlay = build_overlay(Arc::clone(&base), &changeset, entries)
            .await
            .unwrap();

        let snapshot = overlay.snapshot();
        assert_eq!(snapshot.get("a.md").unwrap().text(), "# New\n");
        assert!(snapshot.get("b.md").is_none(), "a None entry deletes");
        assert_eq!(snapshot.get("c.md").unwrap().text(), "# C\n");
        let dirty: Vec<&str> = overlay.dirty().iter().map(String::as_str).collect();
        assert_eq!(dirty, ["a.md", "b.md", "c.md"]);
        assert_eq!((overlay.changeset_id(), overlay.version()), (7, 3));
        assert!(Arc::ptr_eq(overlay.base(), &base));
        // The published state is untouched.
        assert_eq!(base.snapshot().get("a.md").unwrap().text(), "# Old\n");
        assert!(base.workspace().is_file("b.md"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cache_is_reused_until_the_version_or_the_generation_moves() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        store_key(&test, "writer").await;
        let ctx = context(&test, principal_as("writer", Access::Write, &[], Some("pg")), None);
        let state = test.app.catalog("pg").unwrap();
        let pool = test.app.pool();
        let overlays = test.app.overlays();
        let edit = |text: &'static str| PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some(text),
            at: Utc::now(),
        };
        let first_row = pending_entry(&test, &ctx, &edit("# One\n")).await;

        let first = overlays.get(pool, &state, &first_row).await.unwrap();
        let again = overlays.get(pool, &state, &first_row).await.unwrap();
        let second_row = pending_entry(&test, &ctx, &edit("# Two\n")).await;
        let after_edit = overlays.get(pool, &state, &second_row).await.unwrap();
        let options = ImportOptions {
            replace: true,
            actor: "t",
            now: Utc::now(),
        };
        let republished = files(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
        import(pool, "pg", republished, &options).await.unwrap();
        state.refresh(pool).await.unwrap();
        let after_import = overlays.get(pool, &state, &second_row).await.unwrap();
        overlays.forget(second_row.id);
        let after_forget = overlays.get(pool, &state, &second_row).await.unwrap();

        assert!(Arc::ptr_eq(&first, &again), "same version: the cached overlay");
        assert!(!Arc::ptr_eq(&again, &after_edit));
        assert_eq!(after_edit.snapshot().get("a.md").unwrap().text(), "# Two\n");
        assert!(!Arc::ptr_eq(&after_edit, &after_import));
        assert!(after_import.snapshot().get("b.md").is_some(), "rebuilt on the new base");
        assert_eq!(after_import.snapshot().get("a.md").unwrap().text(), "# Two\n");
        assert!(!Arc::ptr_eq(&after_import, &after_forget));
    }
}
```

`src/mcp/context.rs`, in the test module (extend the imports with `crate::store::keys::Access` and the Task 6 helpers):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn the_view_is_pending_only_for_the_key_and_label_with_an_open_changeset() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        store_key(&test, "writer").await;
        let writer = || context(&test, principal_as("writer", Access::Write, &[], Some("pg")), None);
        let reader = context(&test, principal(&[], Some("pg")), None);
        let state = test.app.catalog("pg").unwrap();
        let before = writer().view(&state).await.unwrap();
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some("# Pending\n"),
            at: Utc::now(),
        };

        pending_entry(&test, &writer(), &edit).await;
        let mine = writer().view(&state).await.unwrap();
        let other_label = writer().with_label("other").view(&state).await.unwrap();
        let read_key = reader.view(&state).await.unwrap();

        assert!(!before.pending());
        assert!(mine.pending());
        assert_eq!(mine.snapshot().get("a.md").unwrap().text(), "# Pending\n");
        assert!(!other_label.pending());
        assert!(!read_key.pending());
        assert_eq!(read_key.snapshot().get("a.md").unwrap().text(), "# A\n");
    }
```

`src/mcp/documents.rs`, in the test module (imports: `chrono::Utc`, `crate::store::keys::Access`, and `pending_entry, principal_as, store_key, PendingEdit` from `crate::mcp::testing`):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_sees_a_pending_edit_only_on_the_key_and_label_that_made_it() {
        let test = app_with(&[("pg", &[("hub.md", HUB)])], Config::default()).await;
        store_key(&test, "alice").await;
        store_key(&test, "bob").await;
        let alice = || context(&test, principal_as("alice", Access::Write, &[], Some("pg")), None);
        let bob = context(&test, principal_as("bob", Access::Write, &[], Some("pg")), None);
        let edited = HUB.replace("The hub.", "The edited hub.");
        let edit = PendingEdit {
            catalog: "pg",
            path: "hub.md",
            text: Some(&edited),
            at: Utc::now(),
        };
        pending_entry(&test, &alice(), &edit).await;

        let mine = get_concept(&alice(), concept_params("hub")).await.unwrap();
        let other_label = get_concept(&alice().with_label("other"), concept_params("hub"))
            .await
            .unwrap();
        let other_key = get_concept(&bob, concept_params("hub")).await.unwrap();

        assert_eq!(mine.frontmatter["description"], "The edited hub.");
        assert_eq!(other_label.frontmatter["description"], "The hub.");
        assert_eq!(other_key.frontmatter["description"], "The hub.");
    }
```

`src/mcp/reports.rs`, in the test module (imports: `std::sync::Arc`, `chrono::{TimeDelta, TimeZone, Utc}`, `crate::clock::FixedClock`, `crate::store::keys::Access`, and `app_at, pending_entry, principal_as, store_key, PendingEdit` from `crate::mcp::testing`):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn pending_lint_counts_the_changeset_s_paths_as_uncommitted() {
        let imported = Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap();
        // Stamped after the import: `generated-after-commit` on a clean file.
        let stamped = "---\ntype: Note\ngenerated: { by: t, at: 2026-11-03T12:10:00Z }\n---\nBody\n";
        let clock = Arc::new(FixedClock(imported + TimeDelta::minutes(30)));
        let test = app_at(
            &[("pg", &[("later.md", stamped), ("other.md", stamped)])],
            Config::default(),
            imported,
            clock,
        )
        .await;
        store_key(&test, "writer").await;
        let ctx = context(&test, principal_as("writer", Access::Write, &[], Some("pg")), None);
        let edited = format!("{stamped}More.\n");
        let edit = PendingEdit {
            catalog: "pg",
            path: "later.md",
            text: Some(&edited),
            at: imported + TimeDelta::minutes(20),
        };
        pending_entry(&test, &ctx, &edit).await;
        let params = |pending| LintParams {
            catalog: None,
            pending,
        };

        let published = lint_report(&ctx, params(None)).await.unwrap();
        let pending = lint_report(&ctx, params(Some(true))).await.unwrap();

        let flagged = |lint: &Lint| -> Vec<String> {
            lint.report
                .warnings
                .iter()
                .filter(|finding| finding.rule == "generated-after-commit")
                .map(|finding| finding.file.clone())
                .collect()
        };
        assert!(!published.pending);
        assert!(pending.pending);
        assert_eq!(flagged(&published), ["later.md", "other.md"]);
        assert_eq!(flagged(&pending), ["other.md"]);
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib -- workspace:: lint:: catalog:: overlay:: mcp::`
Expected: compile errors (`with_changes`, `with_dirty`, `workspace`, `build_overlay`, `view`, `LintParams`, `app_at` ... not found).

- [ ] **Step 3: The workspace copy and the dirty set**

In `src/okf/workspace.rs`, in `impl MemWorkspace` after `into_stored`:

```rust
    /// A copy of this workspace with `changes` applied: a changeset's view of
    /// the published files (an overlay).
    ///
    /// The copy is checked as [`MemWorkspace::from_stored`] checks its input,
    /// so it is always a consistent tree; this workspace is not changed.
    ///
    /// # Arguments
    /// * `changes` - `(catalog-relative path, file)` pairs, applied in order:
    ///   `Some` writes the file (bytes and execute bit as given), `None`
    ///   deletes the path (a missing path is fine).
    ///
    /// # Returns
    /// The new workspace.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if a path escapes the catalog or is the root,
    /// or if the result has a file that is also a directory of another.
    pub fn with_changes(
        &self,
        changes: impl IntoIterator<Item = (String, Option<StoredFile>)>,
    ) -> Result<Self, OkfError> {
        // `clone()` on the read guard copies the map it guards (the guard
        // itself is not `Clone`, so method lookup reaches the map); the
        // guard, and the lock, end with this statement.
        let mut map = self.files().clone();
        for (path, file) in changes {
            let rel = checked_file(&path)?;
            match file {
                Some(file) => {
                    map.insert(rel, file);
                }
                None => {
                    map.remove(&rel);
                }
            }
        }
        if let Some(path) = files_that_are_directories(&map).next() {
            return Err(OkfError::InvalidPath {
                path: path.clone(),
                reason: "is a file and a directory".to_string(),
            });
        }
        Ok(Self {
            files: RwLock::new(map),
        })
    }
```

In `src/okf/analysis/lint.rs`, in `impl CommitTimes` after `new`:

```rust
    /// The same commit times with more paths marked uncommitted: for
    /// `lint pending`, a changeset's paths, as `git status` would list a
    /// working tree's edits for `okf.py`.
    ///
    /// # Arguments
    /// * `dirty` - the paths to add (spelled as [`CommitTimes::new`] says).
    ///
    /// # Returns
    /// A new record; this one is unchanged.
    pub fn with_dirty(&self, dirty: impl IntoIterator<Item = String>) -> Self {
        let mut marked = self.clone();
        marked.dirty.extend(dirty);
        marked
    }
```

- [ ] **Step 4: `Loaded` keeps its workspace and revisions**

In `src/catalog.rs` (add `use std::collections::HashMap;`):

1. Add two fields to `Loaded`:

```rust
    // The published files themselves: an overlay copies this and applies
    // its changeset's entries (M5 Decision 6).
    workspace: MemWorkspace,
    // Each live path's current revision: a new changeset entry's base.
    revisions: HashMap<String, i32>,
```

2. Add the accessors after `last_change`:

```rust
    /// The published files, as the snapshot was built from them.
    pub fn workspace(&self) -> &MemWorkspace {
        &self.workspace
    }

    /// A live document's current revision.
    ///
    /// # Arguments
    /// * `path` - normalized, catalog-relative.
    ///
    /// # Returns
    /// Its revision, or `None` if no live document has that path.
    pub fn revision(&self, path: &str) -> Option<i32> {
        self.revisions.get(path).copied()
    }
```

3. In `build`, collect the revisions with the other per-row values and keep the workspace the blocking task builds:

```rust
    let revisions: HashMap<String, i32> = rows
        .iter()
        .map(|row| (row.path.clone(), row.revision))
        .collect();
    // ...`commit_times`, `files`, `last_change` as before...
    let (workspace, snapshot) = tokio::task::spawn_blocking(move || {
        let files = rows.into_iter().map(|row| {
            let file = StoredFile {
                content: row.content,
                executable: row.executable,
            };
            (row.path, file)
        });
        let workspace = MemWorkspace::from_stored(files)?;
        let snapshot = Snapshot::build(&workspace)?;
        // `Ok::<_, OkfError>`: the closure's error type, which `?` above
        // cannot infer on its own.
        Ok::<_, OkfError>((workspace, snapshot))
    })
    .await
    .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))?;
    Ok(Loaded {
        generation,
        snapshot,
        commit_times,
        files,
        last_change,
        workspace,
        revisions,
    })
```

Holding the workspace beside the snapshot keeps each file's bytes once more in memory (about 450 KB for the real catalog).

- [ ] **Step 5: The overlay**

Put this above the test module in `src/overlay.rs`:

```rust
//! Overlays (M5 Decision 6): a connection's view of a catalog with its open
//! changeset applied, so its reads see its own pending edits.
//!
//! An overlay is the published workspace with the changeset's entries
//! applied and a full snapshot of the result (about 10 ms for the real
//! catalog), built on a blocking thread. [`Overlays`] caches one per
//! changeset, valid while the changeset's `version` and the published
//! `generation` stay the same; no incremental snapshot.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::catalog::{CatalogState, Loaded};
use crate::error::OkfError;
use crate::mcp::blocking;
use crate::okf::analysis::lint::{lint, CommitTimes, LintReport};
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::changesets::{self, ChangesetRow, EntryRow};

/// A catalog's published state with one changeset's entries applied.
/// Immutable.
#[derive(Debug)]
pub struct Overlay {
    base: Arc<Loaded>,
    changeset_id: i64,
    version: i64,
    workspace: MemWorkspace,
    snapshot: Snapshot,
    dirty: BTreeSet<String>,
    // The base's commit times with `dirty` marked uncommitted, made once.
    commit_times: CommitTimes,
}

impl Overlay {
    /// The snapshot of the overlaid files.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The overlaid files (what a write op reads, through a copy).
    pub fn workspace(&self) -> &MemWorkspace {
        &self.workspace
    }

    /// The published state it was built on.
    pub fn base(&self) -> &Arc<Loaded> {
        &self.base
    }

    /// The changeset's paths, sorted (deleted ones included).
    pub fn dirty(&self) -> &BTreeSet<String> {
        &self.dirty
    }

    /// The changeset it shows.
    pub fn changeset_id(&self) -> i64 {
        self.changeset_id
    }

    /// The changeset `version` it was built at.
    pub fn version(&self) -> i64 {
        self.version
    }

    /// Lints the overlaid snapshot, the changeset's paths counted as
    /// uncommitted (`lint pending`).
    ///
    /// # Arguments
    /// * `now` - the current time (server clock).
    ///
    /// # Returns
    /// The findings, as [`lint`] returns them.
    pub fn lint(&self, now: DateTime<Utc>) -> LintReport {
        lint(&self.snapshot, now, Some(&self.commit_times))
    }
}

/// Builds an overlay on a blocking thread.
///
/// # Arguments
/// * `base` - the published state.
/// * `changeset` - the changeset (its id and `version` are recorded).
/// * `entries` - its entries; an entry with `content: None` deletes its path.
///
/// # Returns
/// The overlay.
///
/// # Errors
/// [`OkfError::InvalidPath`] if the entries do not form a tree with the
/// published files; a snapshot error, as for a published catalog.
pub async fn build_overlay(
    base: Arc<Loaded>,
    changeset: &ChangesetRow,
    entries: Vec<EntryRow>,
) -> Result<Overlay, OkfError> {
    let changeset_id = changeset.id;
    let version = changeset.version;
    // `move` hands `base` and `entries` to the blocking thread, which must
    // own what it reads.
    blocking(move || {
        let dirty: BTreeSet<String> = entries.iter().map(|entry| entry.path.clone()).collect();
        let changes = entries.into_iter().map(|entry| {
            let executable = entry.executable;
            let file = entry.content.map(|content| StoredFile {
                content,
                executable,
            });
            (entry.path, file)
        });
        let workspace = base.workspace().with_changes(changes)?;
        let snapshot = Snapshot::build(&workspace)?;
        let commit_times = base.commit_times().with_dirty(dirty.iter().cloned());
        Ok(Overlay {
            base,
            changeset_id,
            version,
            workspace,
            snapshot,
            dirty,
            commit_times,
        })
    })
    .await
}

/// A cached overlay and when it was last used.
#[derive(Debug)]
struct Cached {
    overlay: Arc<Overlay>,
    used: Instant,
}

/// Every connection's overlay, by changeset id.
#[derive(Debug)]
pub struct Overlays {
    idle: Duration,
    // A `std` mutex: held for a lookup or an insert, never across `.await`.
    cached: Mutex<HashMap<i64, Cached>>,
}

impl Overlays {
    /// An empty cache whose entries are dropped after `idle` without use.
    ///
    /// # Arguments
    /// * `idle` - `session_idle_timeout`.
    ///
    /// # Returns
    /// The cache.
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            cached: Mutex::new(HashMap::new()),
        }
    }

    /// The overlay of `changeset` on `state`'s current published state:
    /// the cached one if it was built at this `version` and `generation`,
    /// else a new one (its entries read now).
    ///
    /// # Arguments
    /// * `pool` - the database, for the entries.
    /// * `state` - the changeset's catalog.
    /// * `changeset` - an open changeset, as just read.
    ///
    /// # Returns
    /// The overlay, shared.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the entries; as [`build_overlay`].
    pub async fn get(
        &self,
        pool: &PgPool,
        state: &CatalogState,
        changeset: &ChangesetRow,
    ) -> Result<Arc<Overlay>, OkfError> {
        let base = state.current();
        let now = Instant::now();
        if let Some(overlay) = self.lookup(changeset, base.generation(), now) {
            return Ok(overlay);
        }
        let entries = changesets::entries(pool, changeset.id).await?;
        let overlay = Arc::new(build_overlay(base, changeset, entries).await?);
        self.store(Arc::clone(&overlay), now);
        Ok(overlay)
    }

    /// Drops a changeset's overlay (after a write, publish or discard).
    ///
    /// # Arguments
    /// * `changeset_id` - the changeset.
    pub fn forget(&self, changeset_id: i64) {
        self.lock().remove(&changeset_id);
    }

    /// The cached overlay, if it is current; marks it used.
    fn lookup(&self, changeset: &ChangesetRow, generation: i64, now: Instant) -> Option<Arc<Overlay>> {
        let mut cached = self.lock();
        let entry = cached.get_mut(&changeset.id)?;
        let current = entry.overlay.version == changeset.version
            && entry.overlay.base.generation() == generation;
        if !current {
            return None;
        }
        entry.used = now;
        Some(Arc::clone(&entry.overlay))
    }

    /// Caches `overlay`, unless a newer one for the same changeset is there
    /// (a slower build finishing last), first dropping every idle entry.
    fn store(&self, overlay: Arc<Overlay>, now: Instant) {
        let mut cached = self.lock();
        cached.retain(|_, entry| now.saturating_duration_since(entry.used) <= self.idle);
        // Tuples compare field by field, like Python's: generation first.
        let built_at = |overlay: &Overlay| (overlay.base.generation(), overlay.version);
        let newer_cached = cached
            .get(&overlay.changeset_id)
            .is_some_and(|entry| built_at(&entry.overlay) > built_at(&overlay));
        if !newer_cached {
            cached.insert(overlay.changeset_id, Cached { overlay, used: now });
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<i64, Cached>> {
        self.cached.lock().expect("overlay cache lock poisoned")
    }
}

/// What one tool call reads of one catalog.
// Both arms hold an `Arc`, so a `View` is cheap to move to a blocking thread.
#[derive(Debug, Clone)]
pub enum View {
    /// The published state: no open changeset for this key and label.
    Published(Arc<Loaded>),
    /// This connection's overlay.
    Pending(Arc<Overlay>),
}

impl View {
    /// The snapshot to read.
    pub fn snapshot(&self) -> &Snapshot {
        match self {
            View::Published(loaded) => loaded.snapshot(),
            View::Pending(overlay) => overlay.snapshot(),
        }
    }

    /// Lints what this view shows, with its commit times.
    ///
    /// # Arguments
    /// * `now` - the current time (server clock).
    ///
    /// # Returns
    /// The findings.
    pub fn lint(&self, now: DateTime<Utc>) -> LintReport {
        match self {
            View::Published(loaded) => loaded.lint(now),
            View::Pending(overlay) => overlay.lint(now),
        }
    }

    /// Does this view show pending edits?
    pub fn pending(&self) -> bool {
        matches!(self, View::Pending(_))
    }
}
```

Add `pub mod overlay;` to `src/lib.rs` (and "the per-connection overlays (`overlay`)" to the crate doc).

In `src/app.rs`: `use crate::overlay::Overlays;`; the field

```rust
    // Each open changeset's overlay, cached (M5 Decision 6).
    overlays: Overlays,
```

in `load`, `let overlays = Overlays::new(config.session_idle_timeout);` before building `Self` and `overlays,` in the literal; and the accessor:

```rust
    /// The overlay cache.
    pub fn overlays(&self) -> &Overlays {
        &self.overlays
    }
```

- [ ] **Step 6: `ToolContext::view`**

In `src/mcp/context.rs` (imports: `crate::overlay::View`, `crate::store::changesets::{self, ChangesetKey}`, `crate::store::keys::Access`), after `one`:

```rust
    /// What this call reads of `state`: this key and label's overlay if they
    /// have an open changeset in the catalog, else the published state.
    ///
    /// One lookup in the open-changeset index per call and catalog. A `read`
    /// key can never write, so it never has a changeset and skips it.
    ///
    /// # Arguments
    /// * `state` - a catalog this call selected.
    ///
    /// # Returns
    /// The view.
    ///
    /// # Errors
    /// [`OkfError::Database`]; an overlay build error ([`crate::overlay::build_overlay`]).
    pub async fn view(&self, state: &CatalogState) -> Result<View, OkfError> {
        if self.principal.access < Access::Write {
            return Ok(View::Published(state.current()));
        }
        let key = ChangesetKey {
            catalog_id: state.id(),
            key_id: &self.principal.key_id,
            label: &self.label,
        };
        match changesets::find_open(self.app.pool(), &key).await? {
            None => Ok(View::Published(state.current())),
            Some(changeset) => {
                let overlay = self
                    .app
                    .overlays()
                    .get(self.app.pool(), state, &changeset)
                    .await?;
                Ok(View::Pending(overlay))
            }
        }
    }
```

- [ ] **Step 7: The read tools**

`src/mcp/search.rs`: replace the `loaded` block of `run` (and import `crate::catalog::CatalogState` alone, plus `crate::overlay::View`, instead of `{CatalogState, Loaded}`):

```rust
    // Each catalog's view (published, or this connection's overlay), taken
    // once here. Both hold `Arc`s, so the blocking thread can own them.
    let mut views: Vec<(Arc<CatalogState>, View)> = Vec::with_capacity(states.len());
    for state in states {
        let view = ctx.view(&state).await?;
        views.push((state, view));
    }
    // `move` hands `views` and `query` to the closure, which runs on
    // another thread; the borrows below live only inside it.
    let hits = blocking(move || {
        let catalogs: Vec<(&str, &Snapshot)> = views
            .iter()
            .map(|(state, view)| (state.name(), view.snapshot()))
            .collect();
        search_catalogs(&catalogs, &query)
    })
    .await;
```

and add to `run`'s errors doc: "[`OkfError::Database`] looking up the connection's changeset".

`src/mcp/documents.rs`: `get_concept` and `read_document` become `pub(crate) async fn`, each reading through the view (errors doc as above):

```rust
    for state in ctx.read_set(params.catalog.as_deref())? {
        let view = ctx.view(&state).await?;
        let snapshot = view.snapshot();
        match snapshot.resolve_concept(&params.path) {
            // ...the arms as before...
        }
    }
```

```rust
    for state in ctx.read_set(params.catalog.as_deref())? {
        if let Some(doc) = ctx.view(&state).await?.snapshot().get(&path) {
            return Ok(Document {
                catalog: state.name().to_string(),
                path: format!("/{path}"),
                text: doc.raw(),
            });
        }
    }
```

The tool methods call `reply(get_concept(&ctx, params).await)` and `reply(read_document(&ctx, params).await)`. In the existing tests, add `.await` after every `get_concept(...)` call and every `read(...)` call (the `read` closure returns `read_document`'s future, which borrows `ctx`).

`src/mcp/reports.rs` (imports: `std::sync::Arc`, `crate::catalog::CatalogState`, `crate::overlay::View`):

```rust
/// `lint`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct LintParams {
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
    /// Lint this connection's pending changes (its changeset applied to the
    /// published catalog) instead of the published catalog; the changed
    /// paths count as uncommitted for `generated-after-commit`.
    pub pending: Option<bool>,
}
```

Add to `Lint`, after `catalog`:

```rust
    /// Whether this connection's pending changes were linted: `pending` was
    /// asked for and the connection has an open changeset in the catalog.
    pub pending: bool,
```

Add the helper:

```rust
/// The one catalog a report runs on, and this connection's view of it.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `catalog` - the call's `catalog` argument.
///
/// # Returns
/// The catalog's state and view.
///
/// # Errors
/// A selection error; as [`ToolContext::view`].
async fn one_view(
    ctx: &ToolContext,
    catalog: Option<&str>,
) -> Result<(Arc<CatalogState>, View), OkfError> {
    let state = ctx.one(catalog)?;
    let view = ctx.view(&state).await?;
    Ok((state, view))
}
```

In `open_questions`, `oldest_report`, `sprawl_report` and `links`, replace the `ctx.one(...)` and `state.current()` lines with `let (state, view) = one_view(ctx, params.catalog.as_deref()).await?;` and `loaded.snapshot()` with `view.snapshot()` inside each `blocking` closure. `lint_report` becomes:

```rust
/// `okf.py lint`, on the published catalog with its commit times, or with
/// `pending` on this connection's view of it.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog, if named, and `pending`.
///
/// # Returns
/// The errors and warnings, and whether pending changes were linted.
///
/// # Errors
/// A selection error; as [`ToolContext::view`].
pub(crate) async fn lint_report(ctx: &ToolContext, params: LintParams) -> Result<Lint, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let view = if params.pending.unwrap_or(false) {
        ctx.view(&state).await?
    } else {
        View::Published(state.current())
    };
    let pending = view.pending();
    let now = ctx.app().clock().now();
    let report = blocking(move || view.lint(now)).await;
    Ok(Lint {
        catalog: state.name().to_string(),
        pending,
        report,
    })
}
```

The `lint` tool takes `Parameters<LintParams>`, with the description:

```rust
    #[tool(
        description = "Lint a catalog (okf.py lint --json): errors and warnings with file, \
                          line, rule and message. Without `pending`, the published catalog; with \
                          `pending: true`, this connection's pending changes applied to it, the \
                          changed paths counted as uncommitted."
    )]
```

In the existing tests, `lint_report(&ctx, CatalogParam { catalog: None })` becomes `lint_report(&ctx, LintParams { catalog: None, pending: None })`, and the `server.lint(...)` call passes `Parameters(LintParams { catalog: Some("work".to_string()), pending: None })`.

`src/mcp/history.rs` and `src/mcp/catalogs.rs` stay as they are (revisions and published counts).

`src/mcp/testing.rs`: make `app_with` delegate (imports: `chrono::DateTime`, `crate::clock::Clock`):

```rust
/// Creates each catalog, imports its files now, and loads the app with the
/// system clock.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// As [`app_at`].
pub(crate) async fn app_with(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> TestApp {
    app_at(catalogs_and_files, config, Utc::now(), Arc::new(SystemClock)).await
}

/// Creates each catalog, imports its files at `imported`, and loads the app
/// with `clock`.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
/// * `imported` - the creation and import time (each document's revision time).
/// * `clock` - the server's clock.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// If the database, a create or an import fails: a broken test setup.
pub(crate) async fn app_at(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
    imported: DateTime<Utc>,
    clock: Arc<dyn Clock>,
) -> TestApp {
    let db = crate::db::test_db().await;
    for (name, entries) in catalogs_and_files {
        catalogs::create(&db.pool, name, "", imported)
            .await
            .expect("create the test catalog");
        let options = ImportOptions {
            replace: false,
            actor: "tester",
            now: imported,
        };
        import(&db.pool, name, files(entries), &options)
            .await
            .expect("import the test files");
    }
    let app = App::load(db.pool.clone(), config, clock)
        .await
        .expect("load the app");
    TestApp { app, _db: db }
}
```

- [ ] **Step 8: Run the tests**

Run: `cargo test --lib -- workspace:: lint:: catalog:: overlay:: mcp::`
Expected: the eight new tests pass (`with_changes_copies_...`, `with_dirty_...`, `loaded_keeps_...`, the two `overlay` tests, `the_view_is_pending_...`, `get_concept_sees_a_pending_edit_...`, `pending_lint_...`), with every older test there. Then `cargo test`; `tests/server.rs` still passes (`read` keys never query changesets).

- [ ] **Step 9: Update the design**

1. "In-memory snapshot", the overlay paragraph: "A connection with an open changeset in a catalog (its key and label, see "Write grouping") reads that catalog through an overlay: the published workspace with the changeset's entries applied, and a full snapshot of it, built on a blocking thread and cached per changeset while the changeset's `version` and the catalog's `generation` are unchanged. Each read looks the changeset up (one index lookup; `read` keys, which cannot write, skip it), so an agent sees its own pending edits and no one else's. `history` and `list_catalogs` report the published state."
2. "Read tools", the `lint` row: "Port of `lint --json`, on the published catalog or (with `pending`) on this connection's overlay, with a `pending` flag saying which"; and the paragraph after the table: "paths in the connection's changeset count as uncommitted".
3. "Data flow", "Read", step 4: "or of this connection's overlay if its key and label have an open changeset there".

- [ ] **Step 10: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-07-overlays.md`: `Loaded::{workspace, revision}` (and the memory it costs); `MemWorkspace::with_changes`; `CommitTimes::with_dirty`; `Overlay`, `build_overlay`, `Overlays` (cache validity, idle pruning, never replacing a newer build); `View` and `ToolContext::view` (the read-key shortcut); the read tools changed and the two left alone, and why; `lint`'s `pending` and `Lint::pending`; `app_at`; the tests; the Interfaces notes above. Run every gate. Commit:

```bash
git add src/okf/workspace.rs src/okf/analysis/lint.rs src/catalog.rs src/overlay.rs src/lib.rs \
    src/app.rs src/mcp/ docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "$(cat <<'EOF'
feat: overlays; reads and lint pending see the connection's changeset (M5.07)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: the write path (`src/write.rs`) and the content write tools

One function, `write::apply`, carries every write tool's call into the database:
- it resolves the target catalog and requires `write` on it;
- it gets or creates the connection's changeset (catalog, key, label) and locks it;
- it runs the `okf::ops` function on a copy of the connection's overlay, on a blocking thread;
- it checks the write rules on every touched path;
- it stores each touched path as an entry with its base revision and replay operations, in one transaction.

The four content tools (`create_concept`, `edit_document`, `replace_body`, `set_frontmatter`) are thin wrappers over it. Nothing is visible to other connections until `publish` (Task 10).

> **Interfaces note (Task 8), 2026-11-03.** Four changes to the frame's Interfaces block, made here because Task 8 owns `apply`:
> 1. **`apply`'s closure gets `&MemWorkspace`, not `&dyn Workspace`.** Ops still take `&dyn Workspace`: `&MemWorkspace` coerces to it at the call (`ops::stamp(ws, ...)` compiles unchanged). The concrete type lets `revert` (Task 11) delete a file through `MemWorkspace::remove`, which the `Workspace` trait lacks. A touched path that is no longer a file after the op becomes a deletion entry (`content: None`), so `OpOutcome` needs no `deleted` field. M6's `delete_concept` uses the same rule.
> 2. **`apply` takes `by: Option<&str>`.** `OpContext::by` borrows a string that must outlive the blocking closure, so `apply` owns it: the call's `by`, else the key's actor. The full signature is `apply(ctx, catalog, by, op)`.
> 3. **`store::changesets::lock(conn, id) -> Result<Option<ChangesetRow>, OkfError>`** is added here. It takes the changeset row `FOR UPDATE` and reads it again. Two writes to one changeset then run one at a time; without it, both would read the same overlay version and the second `upsert_entry` would drop the first's edit. Publish (Task 10) and discard (Task 11) take it too. Lock order everywhere: advisory lock (publish only), then the catalog row (`lock_catalog`), then the changeset row. `catalogs::delete` takes the same order.
> 4. **`WriteReport` derives `Serialize` and `JsonSchema`** (it is the write tools' output), and its `touched` paths are `/`-prefixed like every read tool's.
>
> Two facts from Tasks 6 and 7 are used as they are:
> - **Copying the overlay.** `MemWorkspace` has no `Clone`. "A copy of the overlay workspace" is Task 7's `overlay.workspace().with_changes(std::iter::empty())`.
> - **Test keys.** `changesets.key_id` is a foreign key, so a test key that writes must have an `api_keys` row. Step 2's `writer` helper stores one with Task 6's `store_key`, and a second label on the same key is `ToolContext::with_label`.
>
> Task 1's `lock_catalog` answers a deleted catalog with `Refused` (`CATALOG_GONE`). Its `upsert_entry` bumps `version` (so a cached overlay is stale at once; `forget` just frees it early) and answers a closed changeset with `Busy`.

**Files:**
- Create: `src/write.rs`; modify `src/lib.rs` (`pub mod write;` and the module list in its doc)
- Modify: `src/store/changesets.rs` (add `lock`)
- Create: `src/mcp/writes.rs`; modify `src/mcp/mod.rs` (`mod writes;` and `+ Self::writes_router()`)
- Modify: `src/mcp/testing.rs` (`writer`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Data flow", "Write")
- Create: `changelog/<ts>-m5-task-08-write-path.md`

- [ ] **Step 1: The changeset lock**

Add to `src/store/changesets.rs`, after `by_id`:

```rust
/// Locks one changeset's row for the rest of the transaction, then reads it
/// again.
///
/// Every transaction that changes a changeset's entries or state takes this
/// lock after [`lock_catalog`], so writes to one changeset, its publish and
/// its discard run one at a time. The row is read *after* the lock is
/// granted: under Postgres' default isolation (read committed) each statement
/// sees everything committed before it started, so the `version` and
/// `state` returned are those the previous holder committed.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `id` - the changeset's id.
///
/// # Returns
/// The row as it is now, or `None` if it no longer exists (its catalog was
/// deleted).
///
/// # Errors
/// [`OkfError::Database`] (a deadlock maps to [`OkfError::Busy`], as for
/// every query here).
pub async fn lock(conn: &mut PgConnection, id: i64) -> Result<Option<ChangesetRow>, OkfError> {
    // `fetch_optional` gives `None` when no row matched, so a vanished
    // changeset is not an error here; the caller decides.
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM changesets WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    if locked.is_none() {
        return Ok(None);
    }
    // `&mut *conn` reborrows the connection for one more query.
    by_id(&mut *conn, id).await
}
```

Add a test to its `tests` module:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_waits_for_the_holder_and_reads_its_commit() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        // `changesets.key_id` is a foreign key: the key needs a row.
        let new_key = crate::store::keys::NewKey {
            id: "writer",
            access: crate::store::keys::Access::Write,
            grants: crate::store::keys::Grants::All,
            default_catalog: None,
            actor: "writer",
        };
        crate::store::keys::create(&db.pool, &new_key, Utc::now()).await.unwrap();
        let key = ChangesetKey { catalog_id: catalog.id, key_id: "writer", label: "default" };
        let mut first = db.pool.begin().await.unwrap();
        lock_catalog(&mut first, catalog.id).await.unwrap();
        let row = get_or_create(&mut first, &key, "writer", Utc::now()).await.unwrap();
        first.commit().await.unwrap();
        let mut holder = db.pool.begin().await.unwrap();
        lock(&mut holder, row.id).await.unwrap();
        let pool = db.pool.clone();
        // The second locker blocks until `holder` commits.
        let waiting = tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let seen = lock(&mut tx, row.id).await.unwrap();
            tx.commit().await.unwrap();
            seen
        });
        catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        set_state(&mut holder, row.id, ChangesetState::Discarded, Utc::now()).await.unwrap();
        holder.commit().await.unwrap();
        let seen = waiting.await.unwrap().expect("the row still exists");

        assert!(matches!(seen.state, ChangesetState::Discarded));
        assert!(lock(&mut db.pool.begin().await.unwrap(), i64::MAX).await.unwrap().is_none());
    }
```

(Use the test module's existing imports. If it lacks `catalogs` or `test_db`, add `use crate::store::catalogs;` and `use crate::db::test_db;`.)

Run: `cargo test --lib -- store::changesets::tests::lock_waits`
Expected: 1 passed.

- [ ] **Step 2: A test helper for write keys**

Add to `src/mcp/testing.rs`, beside Task 6's `principal_as` and `store_key`:

```rust
/// A tool context for a `write` key reaching every catalog: the key's row
/// is stored first (a changeset's `key_id` is a foreign key), and the
/// connection has the label `default`.
///
/// # Arguments
/// * `test` - the app.
/// * `key_id` - the key's id, also its actor (call once per key and test).
/// * `default` - its default catalog.
///
/// # Returns
/// The context a write tool would build for that key.
///
/// # Panics
/// If the key row cannot be stored (already stored in this test, say).
pub(crate) async fn writer(test: &TestApp, key_id: &str, default: &str) -> ToolContext {
    store_key(test, key_id).await;
    context(test, principal_as(key_id, Access::Write, &[], Some(default)), None)
}
```

A second label on the same key is `writer(...).await.with_label("draft")`, or `context(test, principal_as(...), None).with_label(...)` once the key is stored.

- [ ] **Step 3: Write the failing tests of the write path**

Create `src/write.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal, principal_as, writer, TestApp};
    use crate::store::keys::Access;
    use crate::okf::ops::replay::ReplayOp;
    use crate::okf::workspace::Workspace;
    use crate::store::changesets::{self, ChangesetState, EntryRow};
    use std::collections::BTreeMap;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";

    async fn pg() -> TestApp {
        app_with(&[("pg", &[("a.md", NOTE), ("AGENTS.md", "# Agents\n")])], Config::default()).await
    }

    /// An outcome that touched `paths` and recorded no replay.
    fn touched(paths: &[&str]) -> OpOutcome {
        OpOutcome {
            touched: paths.iter().map(|path| path.to_string()).collect(),
            replay: BTreeMap::new(),
            message: "test".to_string(),
            notes: Vec::new(),
        }
    }

    /// Writes `text` to `path`, as an op would.
    async fn write(ctx: &ToolContext, path: &'static str, text: String) -> Result<WriteReport, OkfError> {
        // `move` hands `text` to the closure, which `apply` runs on another thread.
        apply(ctx, None, None, move |ws, _| {
            ws.write_atomic(path, &text)?;
            Ok(touched(&[path]))
        })
        .await
    }

    async fn entries(test: &TestApp, changeset: i64) -> Vec<EntryRow> {
        changesets::entries(test.app.pool(), changeset).await.unwrap()
    }

    /// A `SELECT count(*)` query's answer (sqlx 0.9 takes only literal SQL).
    async fn count(test: &TestApp, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(test.app.pool()).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_opens_a_changeset_and_stores_an_entry() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = write(&ctx, "a.md", format!("{NOTE}More.\n")).await.unwrap();

        assert_eq!((report.catalog.as_str(), report.label.as_str()), ("pg", "default"));
        assert_eq!(report.touched, ["/a.md"]);
        let row = changesets::by_id(test.app.pool(), report.changeset).await.unwrap().unwrap();
        assert!(matches!(row.state, ChangesetState::Open));
        assert_eq!((row.key_id.as_deref(), row.actor.as_str()), (Some("writer-a"), "writer-a"));
        let stored = entries(&test, report.changeset).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].path, "a.md");
        assert_eq!(stored[0].base_revision, Some(1));
        assert_eq!(stored[0].content.as_deref(), Some(format!("{NOTE}More.\n").as_bytes()));
        assert_eq!(stored[0].replay, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_read_key_is_refused_before_anything_is_stored() {
        let test = pg().await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let error = write(&ctx, "a.md", "x".to_string()).await.unwrap_err();

        assert_eq!(error.kind(), "Forbidden");
        assert_eq!(count(&test, "SELECT count(*) FROM changesets").await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_write_keeps_the_first_base_revision() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let first = write(&ctx, "a.md", format!("{NOTE}One.\n")).await.unwrap();
        // Someone publishes a.md meanwhile (an import stands in for it), and
        // the server notices.
        let options = crate::store::import::ImportOptions { replace: true, actor: "t", now: chrono::Utc::now() };
        let files = crate::mcp::testing::files(&[("a.md", "# Changed\n"), ("AGENTS.md", "# Agents\n")]);
        crate::store::import::import(test.app.pool(), "pg", files, &options).await.unwrap();
        test.app.catalog("pg").unwrap().refresh(test.app.pool()).await.unwrap();

        let second = write(&ctx, "a.md", format!("{NOTE}Two.\n")).await.unwrap();

        assert_eq!(second.changeset, first.changeset);
        let stored = entries(&test, second.changeset).await;
        // Still the revision the first edit was made on: publish must see
        // that the published document moved on since.
        assert_eq!(stored[0].base_revision, Some(1));
        assert_eq!(stored[0].content.as_deref(), Some(format!("{NOTE}Two.\n").as_bytes()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn machinery_and_secrets_are_refused_and_nothing_is_stored() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // Built at run time, so no source file holds a secret-shaped string.
        let secret = format!("{NOTE}password = {}\n", "Ab1".repeat(4));

        let machinery = write(&ctx, "AGENTS.md", "# Mine now\n".to_string()).await.unwrap_err();
        let leaked = write(&ctx, "a.md", secret).await.unwrap_err();

        assert_eq!(machinery.kind(), "InvalidPath");
        assert!(matches!(
            leaked,
            OkfError::SecretDetected { ref path, line: 7, ref rule, .. }
                if path == "a.md" && rule == "secret:assignment"
        ));
        assert_eq!(count(&test, "SELECT count(*) FROM changeset_entries").await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn by_defaults_to_the_keys_actor_and_now_comes_from_the_clock() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // A plain function (no captures), so it is `'static` as `apply` needs.
        fn record(ws: &MemWorkspace, op_ctx: &OpContext<'_>) -> Result<OpOutcome, OkfError> {
            ws.write_atomic("a.md", &format!("{} {}", op_ctx.by, op_ctx.now.timestamp()))?;
            Ok(touched(&["a.md"]))
        }

        let before = chrono::Utc::now().timestamp();
        let defaulted = apply(&ctx, None, None, record).await.unwrap();
        let text = String::from_utf8(entries(&test, defaulted.changeset).await[0].content.clone().unwrap()).unwrap();
        apply(&ctx, None, Some("human:pgrace"), record).await.unwrap();
        let overridden = entries(&test, defaulted.changeset).await[0].content.clone().unwrap();

        let (by, at) = text.split_once(' ').unwrap();
        assert_eq!(by, "writer-a");
        assert!(at.parse::<i64>().unwrap() >= before);
        assert!(overridden.starts_with(b"human:pgrace "));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_overlay_shows_the_edit_to_this_key_and_label_only() {
        let test = pg().await;
        let author = writer(&test, "writer-a", "pg").await;
        let other_key = writer(&test, "writer-b", "pg").await;
        // The same key (stored above), on another label.
        let other_label = context(&test, principal_as("writer-a", Access::Write, &[], Some("pg")), None).with_label("draft");
        let state = test.app.catalog("pg").unwrap();
        // The view before the write, to show the write invalidates the cache.
        author.view(&state).await.unwrap();

        write(&author, "a.md", format!("{NOTE}Pending.\n")).await.unwrap();

        let text = |view: crate::overlay::View| view.snapshot().get("a.md").unwrap().text().to_string();
        assert_eq!(text(author.view(&state).await.unwrap()), format!("{NOTE}Pending.\n"));
        assert_eq!(text(other_key.view(&state).await.unwrap()), NOTE);
        assert_eq!(text(other_label.view(&state).await.unwrap()), NOTE);
    }

    #[test]
    fn replay_operations_accumulate_until_a_free_edit() {
        let log = |bullet: &str| ReplayOp::Log { date: "2026-11-03".to_string(), bullet: bullet.to_string() };
        let entry = |replay: Option<serde_json::Value>| EntryRow {
            path: "log.md".to_string(),
            base_revision: Some(1),
            content: Some(Vec::new()),
            executable: false,
            replay,
        };
        let first = vec![log("* one")];
        let second = vec![log("* two")];

        let new = merged_replay(None, Some(&first));
        let appended = merged_replay(Some(&entry(new.clone())), Some(&second));
        let freed = merged_replay(Some(&entry(appended.clone())), None);
        let stays_free = merged_replay(Some(&entry(None)), Some(&second));
        let free_from_the_start = merged_replay(None, None);

        assert_eq!(new, Some(serde_json::json!([{"op": "log", "date": "2026-11-03", "bullet": "* one"}])));
        assert_eq!(appended.unwrap().as_array().unwrap().len(), 2);
        assert_eq!(freed, None);
        assert_eq!(stays_free, None);
        assert_eq!(free_from_the_start, None);
    }
}
```

Run: `cargo test --lib -- write::tests`
Expected: compile errors (`apply`, `WriteReport`, `merged_replay` not defined).

- [ ] **Step 4: Implement the write path**

Above the tests in `src/write.rs`:

```rust
//! The write path (design "Data flow", Write): the one function every write
//! tool calls.
//!
//! [`apply`] resolves the target catalog, requires `write` on it, and in one
//! transaction:
//! - takes the catalog row (`FOR KEY SHARE`, plan Decision 3);
//! - gets or creates the connection's changeset and locks its row;
//! - runs the edit on a copy of the connection's overlay, on a blocking
//!   thread, and checks the write rules on every path it touched;
//! - stores each touched path as a changeset entry.
//!
//! Then it commits and drops the cached overlay, so the connection's next
//! read sees the edit. A refused edit stores nothing.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::OkfError;
use crate::mcp::blocking;
use crate::mcp::context::ToolContext;
use crate::okf::ops::replay::ReplayOp;
use crate::okf::ops::{OpContext, OpOutcome};
use crate::okf::rules;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::overlay::Overlay;
use crate::store::changesets::{self, ChangesetKey, ChangesetState, EntryRow};
use crate::store::keys::Access;

/// What a write did: the write tools' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WriteReport {
    /// The catalog written to.
    pub catalog: String,
    /// The changeset holding the edit (what `adopt_changeset` takes).
    pub changeset: i64,
    /// The changeset's label (`X-OKF-Changeset`, `?changeset=`, the MCP session, or `default`).
    pub label: String,
    /// The `/`-prefixed paths the edit wrote or deleted, in order.
    pub touched: Vec<String>,
    /// What `okf.py` would have printed, e.g. `infra/x.md: generated { by: a, at: ... }`.
    pub message: String,
    /// `okf.py`'s notes (a non-kebab-case name, a folder index created).
    pub notes: Vec<String>,
}

/// One path the edit touched: its normalized name and what it holds now
/// (`None`: the edit removed it).
struct Touched {
    path: String,
    file: Option<StoredFile>,
}

/// Applies an edit to the connection's changeset in the target catalog.
///
/// # Arguments
/// * `ctx` - the call's context: key, label, catalogs.
/// * `catalog` - the call's `catalog` argument; else the connection's write
///   target (the first selected catalog).
/// * `by` - the call's `by` argument; else the key's actor.
/// * `op` - the edit. It gets a copy of the overlay workspace (published
///   files plus this changeset's entries) and an [`OpContext`] with the
///   server clock's `now` and `by`, and returns what it touched. A touched
///   path that is not a file afterwards is stored as a deletion.
///
/// # Returns
/// The changeset, the touched paths and the op's message and notes.
///
/// # Errors
/// - a selection error ([`OkfError::CatalogNotSelected`], [`OkfError::Refused`]);
/// - [`OkfError::Refused`] (`CATALOG_GONE`) if the catalog was deleted while this write waited;
/// - [`OkfError::Forbidden`] for a key below `write`;
/// - the op's own error;
/// - [`OkfError::InvalidPath`] for a touched path outside the catalog or in machinery;
/// - [`OkfError::SecretDetected`] or [`OkfError::FrontmatterParse`] from the content rules;
/// - [`OkfError::Busy`] if the changeset was published or discarded while this write waited for it;
/// - [`OkfError::Database`].
// `F: FnOnce(...) + Send + 'static`: the edit is a closure called once, that
// may move to another thread (`Send`) and owns everything it uses
// (`'static`), because it runs on a blocking thread that may outlive this
// call's borrows. `&OpContext<'_>` lets it borrow `by` only during the call.
pub async fn apply<F>(
    ctx: &ToolContext,
    catalog: Option<&str>,
    by: Option<&str>,
    op: F,
) -> Result<WriteReport, OkfError>
where
    F: FnOnce(&MemWorkspace, &OpContext<'_>) -> Result<OpOutcome, OkfError> + Send + 'static,
{
    let state = ctx.one(catalog)?;
    let principal = ctx.principal();
    principal.require(Access::Write, state.name())?;
    let app = ctx.app();
    let now = app.clock().now();
    // Owned, so the blocking closure can take it.
    let by = by.unwrap_or(&principal.actor).to_string();
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &principal.key_id,
        label: ctx.label(),
    };

    // Dropping `tx` without `commit()` (any `?` below) rolls everything back:
    // a refused edit leaves no changeset and no entry behind.
    let mut tx = app.pool().begin().await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    let opened = changesets::get_or_create(&mut tx, &key, &principal.actor, now).await?;
    let changeset = changesets::lock(&mut tx, opened.id)
        .await?
        // `filter` keeps the row only while it is still open: a publish or
        // discard that held the lock may have closed it.
        .filter(|row| matches!(row.state, ChangesetState::Open))
        .ok_or_else(|| OkfError::Busy {
            reason: format!(
                "changeset {} was published or discarded while this write waited; retry",
                opened.id
            ),
        })?;
    // Read after the lock, so the overlay holds every committed entry.
    let overlay = app.overlays().get(app.pool(), &state, &changeset).await?;
    let existing: HashMap<String, EntryRow> = changesets::entries(&mut *tx, changeset.id)
        .await?
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    // `Arc::clone` gives the blocking thread its own handle on the overlay.
    let work = std::sync::Arc::clone(&overlay);
    let (outcome, files) = blocking(move || run(&work, &by, now, op)).await?;

    let paths: Vec<String> = files.iter().map(|touched| format!("/{}", touched.path)).collect();
    for Touched { path, file } in files {
        let previous = existing.get(&path);
        // The first edit's base stays: publish compares it with the
        // document's revision then, whatever later edits saw.
        let base_revision = match previous {
            Some(entry) => entry.base_revision,
            None => overlay.base().revision(&path),
        };
        let replay = merged_replay(previous, outcome.replay.get(&path));
        // `map_or` / `is_some_and` read the `Option` without unwrapping it;
        // the content moves out of `file` (no copy).
        let executable = file.as_ref().is_some_and(|file| file.executable);
        let entry = EntryRow {
            path,
            base_revision,
            content: file.map(|file| file.content),
            executable,
            replay,
        };
        changesets::upsert_entry(&mut tx, changeset.id, &entry, now).await?;
    }
    tx.commit().await?;
    // The version moved, so the cached overlay is stale; dropping it now
    // also frees its memory before the next read rebuilds it.
    app.overlays().forget(changeset.id);
    Ok(WriteReport {
        catalog: state.name().to_string(),
        changeset: changeset.id,
        label: changeset.label,
        touched: paths,
        message: outcome.message,
        notes: outcome.notes,
    })
}

/// Runs the edit on a copy of the overlay and checks the rules on every
/// path it touched (on a blocking thread: the secret scan is CPU work).
///
/// # Errors
/// The op's error; a rule's error for the first refused path.
fn run<F>(
    overlay: &Overlay,
    by: &str,
    now: DateTime<Utc>,
    op: F,
) -> Result<(OpOutcome, Vec<Touched>), OkfError>
where
    F: FnOnce(&MemWorkspace, &OpContext<'_>) -> Result<OpOutcome, OkfError>,
{
    // A copy (Task 7's `with_changes` with no changes): the cached overlay
    // is shared with readers and never changes.
    let workspace = overlay.workspace().with_changes(std::iter::empty())?;
    let outcome = op(&workspace, &OpContext { now, by })?;
    // `collect` into `Result<Vec<_>, _>` stops at the first refusal.
    let files = outcome
        .touched
        .iter()
        .map(|path| touched_file(&workspace, path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((outcome, files))
}

/// Checks one touched path: inside the catalog and not machinery, and, if
/// it still exists, its content passes the secret scan and (for markdown)
/// its frontmatter parses.
///
/// # Errors
/// [`OkfError::InvalidPath`], [`OkfError::SecretDetected`],
/// [`OkfError::FrontmatterParse`]; [`OkfError::Refused`] for content that is
/// not UTF-8 (no op writes any).
fn touched_file(workspace: &MemWorkspace, path: &str) -> Result<Touched, OkfError> {
    let path = rules::check_path(path)?;
    let file = workspace.stored(&path);
    if let Some(stored) = &file {
        let text = std::str::from_utf8(&stored.content).map_err(|_| OkfError::Refused {
            reason: format!("{path}: the write tools only write UTF-8 text"),
        })?;
        rules::check_content(&path, text)?;
    }
    Ok(Touched { path, file })
}

/// The `replay` an entry stores after one more edit (plan Interfaces, Task 8).
///
/// While every edit to an entry came from bookkeeping, its replay is their
/// operations in order, so publish can redo them on newer text. One free
/// edit (no operations for this path) clears it for good: publish then
/// treats the entry like any other edit.
///
/// # Arguments
/// * `previous` - the path's entry before this edit, if any.
/// * `new_ops` - this edit's operations for the path, if it was bookkeeping.
///
/// # Returns
/// The JSON array to store, or `None` (a free edit).
fn merged_replay(previous: Option<&EntryRow>, new_ops: Option<&Vec<ReplayOp>>) -> Option<serde_json::Value> {
    let new_ops = new_ops?;
    let mut ops = match previous {
        None => Vec::with_capacity(new_ops.len()),
        // Stored by this function, so always an array; anything else (a
        // hand edit) is treated as a free edit, which is the safe side.
        Some(entry) => match &entry.replay {
            Some(serde_json::Value::Array(stored)) => stored.clone(),
            Some(_) | None => return None,
        },
    };
    ops.extend(
        new_ops
            .iter()
            .map(|op| serde_json::to_value(op).expect("a ReplayOp (strings and a bool) always serializes")),
    );
    Some(serde_json::Value::Array(ops))
}
```

Add `pub mod write;` to `src/lib.rs` (after `pub mod store;`), and name `write` in its module doc ("the write path every write tool goes through (`write`)").

Run: `cargo test --lib -- write::tests store::changesets`
Expected: all pass (7 in `write::tests`).

- [ ] **Step 5: Write the failing tool tests**

Create `src/mcp/writes.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::store::changesets;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n\nOld text.\n";

    async fn pg() -> TestApp {
        app_with(
            &[("pg", &[("a.md", NOTE), ("infra/index.md", "# Infra\n"), ("log.md", "# Log\n")])],
            Config::default(),
        )
        .await
    }

    /// The text of `path` as this connection sees it (its overlay).
    async fn seen(ctx: &ToolContext, path: &str) -> String {
        let state = ctx.app().catalog("pg").unwrap();
        let view = ctx.view(&state).await.unwrap();
        view.snapshot().get(path).map(|doc| doc.text().to_string()).unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_content_tools_edit_through_the_overlay() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let frontmatter = serde_json::json!({"type": "System", "title": "NTP", "description": "Hosts sync."});

        let created = create_concept(&ctx, CreateConceptParams {
            path: "/infra/ntp.md".to_string(),
            frontmatter: frontmatter.as_object().unwrap().clone(),
            body: "# NTP\n\nSee [A](/a.md).\n".to_string(),
            index_summary: None,
            index_section: None,
            by: None,
            catalog: None,
        })
        .await
        .unwrap();
        let edited = edit_document(&ctx, EditDocumentParams {
            path: "/a.md".to_string(),
            old_str: "Old text.".to_string(),
            new_str: "New text.".to_string(),
            replace_all: false,
            catalog: None,
        })
        .await
        .unwrap();
        replace_body(&ctx, ReplaceBodyParams {
            path: "/infra/ntp".to_string(),
            body: "# NTP\n\nRewritten.\n".to_string(),
            catalog: None,
        })
        .await
        .unwrap();
        set_frontmatter(&ctx, SetFrontmatterParams {
            path: "/a.md".to_string(),
            key: "status".to_string(),
            value: serde_json::json!("draft"),
            catalog: None,
        })
        .await
        .unwrap();
        let mismatch = edit_document(&ctx, EditDocumentParams {
            path: "/a.md".to_string(),
            old_str: "not there".to_string(),
            new_str: "x".to_string(),
            replace_all: false,
            catalog: None,
        })
        .await
        .unwrap_err();

        assert_eq!(created.touched, ["/infra/ntp.md"]);
        assert_eq!(edited.changeset, created.changeset);
        let ntp = seen(&ctx, "infra/ntp.md").await;
        // `by` defaulted to the key's actor in the `generated` stamp.
        assert!(ntp.contains("generated:") && ntp.contains("writer-a"), "{ntp}");
        assert!(ntp.ends_with("# NTP\n\nRewritten.\n"), "{ntp}");
        let a = seen(&ctx, "a.md").await;
        assert!(a.contains("status: draft") && a.contains("New text."), "{a}");
        assert!(matches!(mismatch, OkfError::EditMismatch { matches: 0, .. }));
        let entries = changesets::entries(test.app.pool(), created.changeset).await.unwrap();
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "infra/ntp.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn edit_document_refuses_log_md() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let error = edit_document(&ctx, EditDocumentParams {
            path: "/log.md".to_string(),
            old_str: "# Log".to_string(),
            new_str: "# Mine".to_string(),
            replace_all: false,
            catalog: None,
        })
        .await
        .unwrap_err();

        assert_eq!(error.kind(), "InvalidPath");
    }

    #[test]
    fn the_write_tools_have_object_output_schemas_and_say_who_by_defaults_to() {
        let tools = OkfServer::writes_router().list_all();

        for name in ["create_concept", "edit_document", "replace_body", "set_frontmatter"] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap_or_else(|| panic!("{name}"));
            let output = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(output["type"], "object", "{name}");
        }
        let create = tools.iter().find(|tool| tool.name == "create_concept").unwrap();
        let by = create.input_schema["properties"]["by"]["description"].as_str().unwrap();
        // The phrase sits on one doc-comment line (schemars keeps line breaks).
        assert!(by.contains("default: the key's actor"), "{by}");
    }
}
```

Run: `cargo test --lib -- mcp::writes`
Expected: compile errors (the params and functions are not defined).

- [ ] **Step 6: Implement the content tools**

Above the tests in `src/mcp/writes.rs`:

```rust
//! The write tools (design "Write tools"): the content tools here (Task 8),
//! the bookkeeping tools below them (Task 9). Each goes through
//! [`crate::write::apply`], so it acts on this connection's changeset in one
//! catalog, the write rules apply, and nothing is visible to other
//! connections until `publish`.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::okf::ops::{self, NewConcept};
use crate::write::{apply, WriteReport};

/// `create_concept`'s arguments. Doc comments are the parameters'
/// descriptions in the tool's input schema.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct CreateConceptParams {
    /// The new concept's path, e.g. `/infra/ntp.md` (the `.md` is optional); it must not exist.
    pub path: String,
    /// Frontmatter keys as a JSON object; `type` is required, and `generated`, `verified` and `reviewed` are refused (the server writes `generated`).
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
    /// The markdown body after the frontmatter.
    pub body: String,
    /// List the concept in its folder's index.md with this summary, creating folder indexes as needed.
    pub index_summary: Option<String>,
    /// The index.md section to list it under (default: where `okf.py index` puts it).
    pub index_section: Option<String>,
    /// Who wrote it, for the `generated` stamp (default: the key's actor).
    pub by: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `edit_document`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct EditDocumentParams {
    /// A concept or an index.md, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The exact text to replace; without `replace_all` it must occur exactly once, outside the frontmatter.
    pub old_str: String,
    /// The replacement text.
    pub new_str: String,
    /// Replace every occurrence instead of exactly one.
    #[serde(default)]
    pub replace_all: bool,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `replace_body`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReplaceBodyParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The new body; the frontmatter is kept byte for byte.
    pub body: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `set_frontmatter`'s arguments.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetFrontmatterParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// A descriptive key such as `title`, `status` or `tags`; the stamps, `open_questions` and `sources` have their own tools.
    pub key: String,
    /// The new value as JSON; null deletes the key (`type` cannot be deleted).
    // `default`: an absent `value` is JSON null, a delete.
    #[serde(default)]
    pub value: serde_json::Value,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// Creates a concept (and, with `index_summary`, its listing).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::InvalidPath`] (taken, reserved,
/// machinery) or refusal of a missing `type` or a stamp key.
pub(crate) async fn create_concept(
    ctx: &ToolContext,
    params: CreateConceptParams,
) -> Result<WriteReport, OkfError> {
    // Destructuring moves each field into its own variable, so the closure
    // can take the ones it needs while `catalog` and `by` stay here.
    let CreateConceptParams { path, frontmatter, body, index_summary, index_section, by, catalog } =
        params;
    apply(ctx, catalog.as_deref(), by.as_deref(), move |ws, op_ctx| {
        let new = NewConcept {
            path: &path,
            frontmatter: &frontmatter,
            body: &body,
            index_summary: index_summary.as_deref(),
            index_section: index_section.as_deref(),
        };
        ops::create_concept(ws, op_ctx, &new)
    })
    .await
}

/// Replaces exact text in a concept body or an `index.md`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the document and the replacement.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; [`OkfError::EditMismatch`] when `old_str` is found 0 or
/// (without `replace_all`) more than 1 times; [`OkfError::InvalidPath`] for
/// `log.md` or a match touching the frontmatter.
pub(crate) async fn edit_document(
    ctx: &ToolContext,
    params: EditDocumentParams,
) -> Result<WriteReport, OkfError> {
    let EditDocumentParams { path, old_str, new_str, replace_all, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::edit_document(ws, &path, &old_str, &new_str, replace_all)
    })
    .await
}

/// Replaces a concept's body, keeping its frontmatter byte for byte.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and its new body.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`] or [`OkfError::InvalidPath`].
pub(crate) async fn replace_body(
    ctx: &ToolContext,
    params: ReplaceBodyParams,
) -> Result<WriteReport, OkfError> {
    let ReplaceBodyParams { path, body, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| ops::replace_body(ws, &path, &body)).await
}

/// Sets or deletes one descriptive frontmatter key, as a textual edit.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, the key and the value (null deletes).
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's refusal of a dedicated key or of deleting `type`.
pub(crate) async fn set_frontmatter(
    ctx: &ToolContext,
    params: SetFrontmatterParams,
) -> Result<WriteReport, OkfError> {
    let SetFrontmatterParams { path, key, value, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        // `then_some` turns `true` into `Some(&value)`: null means delete.
        let new_value = (!value.is_null()).then_some(&value);
        ops::set_frontmatter(ws, &path, &key, new_value)
    })
    .await
}

// The content tools; `OkfServer::tools` joins `writes_router()` with the rest.
#[tool_router(router = writes_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `create_concept` tool.
    #[tool(
        description = "Create a concept in this connection's changeset (nothing is visible to \
                       others until `publish`). Needs `type` in `frontmatter`; the server writes \
                       `generated: { by, at }` itself. With `index_summary`, also lists it in its \
                       folder's index.md (creating folder indexes as needed). Write access."
    )]
    async fn create_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CreateConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(create_concept(&ctx, params).await)
    }

    /// The `edit_document` tool.
    #[tool(
        description = "Replace exact text in a concept body or an index.md, in this connection's \
                       changeset. `old_str` must occur exactly once unless `replace_all`, and may \
                       not touch the frontmatter. log.md and machinery are refused (use `log`). \
                       Write access."
    )]
    async fn edit_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<EditDocumentParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(edit_document(&ctx, params).await)
    }

    /// The `replace_body` tool.
    #[tool(
        description = "Replace a concept's body, keeping its frontmatter byte for byte, in this \
                       connection's changeset. Write access."
    )]
    async fn replace_body(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReplaceBodyParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(replace_body(&ctx, params).await)
    }

    /// The `set_frontmatter` tool.
    #[tool(
        description = "Set (or, with a null value, delete) one descriptive frontmatter key of a \
                       concept, in this connection's changeset. The stamps, open_questions and \
                       sources have their own tools; `type` cannot be deleted. Write access."
    )]
    async fn set_frontmatter(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SetFrontmatterParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(set_frontmatter(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, add `mod writes;` (with the other `mod` lines) and `+ Self::writes_router()` at the end of `OkfServer::tools()`. Extend the `INSTRUCTIONS` text with: "Write tools edit this connection's changeset in one catalog; nothing is visible to others until `publish`."

Run: `cargo test --lib -- mcp::writes write::tests`
Expected: all pass (3 in `mcp::writes`, 7 in `write::tests`).

- [ ] **Step 7: The design**

In "Data flow", replace the "Write" list with:

```markdown
### Write

1. Auth, then `write` access on a single target catalog: the `catalog` argument, else
   the connection's write target.
2. One transaction takes the catalog row (`FOR KEY SHARE`), gets or creates the open
   changeset for (catalog, key, label), and locks its row (`FOR UPDATE`), so writes to
   one changeset, its publish and its discard run one at a time. Every changeset
   transaction takes these locks in that order; `catalogs::delete` takes the row
   `FOR UPDATE` first, so neither side can deadlock the other.
3. On a blocking thread, the `okf::ops` function runs on a copy of the connection's
   overlay workspace (the published files plus the changeset's entries). Every path it
   touched then passes the write rules: inside the catalog, not machinery, secret scan,
   frontmatter parses. A touched path the op removed is a deletion.
4. Each touched path becomes an entry (insert or replace):
   - **base revision:** the existing entry's base (the first edit's), else the live
     revision in the overlay's published snapshot (NULL if there is none);
   - **replay:** edits to `log.md` and `index.md` that come from bookkeeping are also
     recorded as operations: `log` adds a log entry; `index`, `create_concept` (with
     `index_summary`) and the listing changes of `move_concept`, `split_concept` and
     `delete_concept` add, rename or remove an `index.md` bullet. The entry keeps the
     existing replay plus the new operations while every edit to it came from one. Any
     other edit (an `edit_document` of an `index.md`, say) clears it for good, so that
     entry conflicts like any other on a stale base.
5. Commit, and drop the connection's cached overlay; its next read rebuilds it. A
   refused edit stores nothing.
```

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-08-write-path.md`:

```markdown
# M5 Task 8: the write path and the content write tools (<ts>)

Issue #<N>, Task 8 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md`.

## Added

- `src/write.rs`: `apply(ctx, catalog, by, op)` and `WriteReport`. In one transaction
  it takes the catalog row, gets or creates the connection's changeset and locks it,
  runs the op on a copy of the overlay (blocking thread), checks the write rules on every
  touched path, and upserts the entries (first base revision kept; replay accumulated
  until a free edit). It commits and forgets the cached overlay.
- `store::changesets::lock`: `FOR UPDATE` on a changeset row, then a fresh read.
- `src/mcp/writes.rs`: `create_concept`, `edit_document`, `replace_body`,
  `set_frontmatter` (`writes_router`).
- `mcp::testing::writer` (stores the key row, then a write context).
- Tests: <list them>.

## Changed

- `apply`'s closure takes `&MemWorkspace` and `apply` takes `by` (plan Interfaces
  note, Task 8). The op runs on `with_changes(std::iter::empty())`, Task 7's copy.
- The design's "Data flow", Write.

## Notes

- The changeset row lock serializes writes to one changeset. Without it, two
  concurrent writes would read the same overlay version, and the second upsert would
  drop the first edit.
- A write commits its changeset even when the op touched nothing (an idempotent `log`).
  The empty changeset holds no work and the 24-hour prune closes it.
```

Run every gate. Commit:

```bash
git add src/write.rs src/lib.rs src/store/changesets.rs src/mcp/ \
    docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: the write path and the content write tools (M5.08)" \
    -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: the bookkeeping write tools

The ports of `okf.py`'s concept commands and its `log` and `index` (design "Write tools"): `stamp`, `review`, `question`, `unquestion`, `add_source`, `log` and `index`. Each calls its `okf::ops` function (Tasks 2 and 3) through `write::apply`. The replay operations an op returns for `log.md` and `index.md` then land in the entry's `replay`, and publish can redo them on newer text (Task 10).

**Router:** a second block, `bookkeeping_router`, in `src/mcp/writes.rs`, below Task 8's `writes_router`, and not an extension of it. Task 8's block stays untouched, so this task's diff is one new section plus one line in `OkfServer::tools()`. The two groups also match the design's split between content edits and `okf.py` ports. `OkfServer` already has one `#[tool_router]` block per module; two in one file work the same way, each producing its own `fn ..._router()`.

**Files:**
- Modify: `src/mcp/writes.rs` (the bookkeeping section and its tests)
- Modify: `src/mcp/mod.rs` (`+ Self::bookkeeping_router()`)
- Create: `changelog/<ts>-m5-task-09-bookkeeping-tools.md`

- [ ] **Step 1: Write the failing tests**

Add a second test module at the end of `src/mcp/writes.rs` (a separate module keeps Task 8's tests untouched):

```rust
#[cfg(test)]
mod bookkeeping_tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::store::changesets::{self, EntryRow};

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
    const NTP: &str = "---\ntype: System\ntitle: NTP\ndescription: Hosts sync.\n---\n# NTP\n";

    async fn pg() -> TestApp {
        app_with(
            &[(
                "pg",
                &[
                    ("a.md", NOTE),
                    ("infra/index.md", "# Infra\n\n* [Time server](time.md) - the GPS clock\n"),
                    ("infra/time.md", NOTE),
                    ("infra/ntp.md", NTP),
                    ("log.md", "# Log\n"),
                ],
            )],
            Config::default(),
        )
        .await
    }

    async fn entry(test: &TestApp, changeset: i64, path: &str) -> EntryRow {
        changesets::entries(test.app.pool(), changeset)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("no entry for {path}"))
    }

    async fn seen(ctx: &ToolContext, path: &str) -> String {
        let state = ctx.app().catalog("pg").unwrap();
        let view = ctx.view(&state).await.unwrap();
        view.snapshot().get(path).map(|doc| doc.text().to_string()).unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn log_records_its_replay_on_the_log_md_entry() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = log(&ctx, LogParams { text: "hello   world".to_string(), kind: None, catalog: None })
            .await
            .unwrap();

        assert_eq!(report.touched, ["/log.md"]);
        let stored = entry(&test, report.changeset, "log.md").await;
        let replay = stored.replay.expect("a log entry is replayable");
        // `kind` defaulted to Update; okf.py collapses the whitespace.
        assert_eq!(replay[0]["op"], "log");
        assert_eq!(replay[0]["bullet"], "* **Update:** hello world");
        assert!(seen(&ctx, "log.md").await.contains("* **Update:** hello world"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_free_edit_of_an_index_after_index_clears_its_replay() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let listed = index(&ctx, IndexParams {
            path: "/infra/ntp.md".to_string(),
            summary: "how hosts sync".to_string(),
            section: None,
            title: None,
            catalog: None,
        })
        .await
        .unwrap();
        let after_index = entry(&test, listed.changeset, "infra/index.md").await;
        crate::mcp::writes::edit_document(&ctx, EditDocumentParams {
            path: "/infra/index.md".to_string(),
            old_str: "the GPS clock".to_string(),
            new_str: "the GPS stratum 1 clock".to_string(),
            replace_all: false,
            catalog: None,
        })
        .await
        .unwrap();
        let after_edit = entry(&test, listed.changeset, "infra/index.md").await;

        assert_eq!(after_index.replay.expect("index is replayable")[0]["op"], "list_bullet");
        assert_eq!(after_edit.replay, None);
        let text = seen(&ctx, "infra/index.md").await;
        assert!(text.contains("[NTP](ntp.md) - how hosts sync") && text.contains("stratum 1"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_concept_commands_edit_frontmatter_as_free_edits() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let path = || "/a.md".to_string();

        stamp(&ctx, StampParams { path: path(), field: StampFieldParam::Verified, by: Some("human:pgrace".to_string()), catalog: None })
            .await
            .unwrap();
        question(&ctx, QuestionParams { path: path(), question: "Who owns it?".to_string(), catalog: None }).await.unwrap();
        question(&ctx, QuestionParams { path: path(), question: "Is it backed up?".to_string(), catalog: None }).await.unwrap();
        // A number is the question's 1-based position, as `open_questions` numbers it.
        unquestion(&ctx, UnquestionParams { path: path(), question: QuestionRef::Number(1), catalog: None }).await.unwrap();
        review(&ctx, ReviewParams {
            path: path(),
            outcome: "current".to_string(),
            note: "checked".to_string(),
            questions: None,
            by: None,
            catalog: None,
        })
        .await
        .unwrap();
        let last = add_source(&ctx, AddSourceParams {
            path: path(),
            id: "runbook".to_string(),
            resource: "https://example.com/runbook".to_string(),
            title: Some("Runbook".to_string()),
            author: None,
            catalog: None,
        })
        .await
        .unwrap();

        let text = seen(&ctx, "a.md").await;
        assert!(text.contains("verified:") && text.contains("human:pgrace"), "{text}");
        assert!(text.contains("reviewed:") && text.contains("writer-a"), "{text}");
        assert!(text.contains("Is it backed up?") && !text.contains("Who owns it?"), "{text}");
        assert!(text.contains("https://example.com/runbook"), "{text}");
        // Frontmatter edits carry no replay: they conflict on a stale base.
        assert_eq!(entry(&test, last.changeset, "a.md").await.replay, None);
    }

    #[test]
    fn the_bookkeeping_tools_are_listed_with_their_defaults() {
        let tools = OkfServer::bookkeeping_router().list_all();
        // `&*` borrows the name (a `Cow<str>`) as a plain `&str`.
        let names: Vec<&str> = tools.iter().map(|tool| &*tool.name).collect();
        let log = tools.iter().find(|tool| tool.name == "log").unwrap();
        let kind = log.input_schema["properties"]["kind"]["description"].as_str().unwrap();

        for name in ["stamp", "review", "question", "unquestion", "add_source", "log", "index"] {
            assert!(names.contains(&name), "{name} is registered");
        }
        assert!(kind.contains("default Update"), "{kind}");
        assert!(tools.iter().all(|tool| tool.output_schema.is_some()));
    }
}
```

Run: `cargo test --lib -- mcp::writes::bookkeeping_tests`
Expected: compile errors (the params and functions are not defined).

- [ ] **Step 2: Implement the tools**

Add below `writes_router`'s block in `src/mcp/writes.rs`, and extend the file's `ops` import to `ops::{self, LogKind, NewConcept, SourceEntry, StampField}`:

```rust
// ---------------------------------------------------------------------------
// The bookkeeping tools (Task 9): ports of okf.py's concept commands, `log`
// and `index`.
// ---------------------------------------------------------------------------

/// Which stamp `stamp` writes.
// `rename_all = "lowercase"`: the JSON spells them `generated` and `verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StampFieldParam {
    /// The content was (re)written.
    Generated,
    /// The content was checked against the real system.
    Verified,
}

/// A log entry's kind, as `okf.py log --kind` takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
pub enum LogKindParam {
    /// An update to existing concepts (the default).
    #[default]
    Update,
    /// New concepts.
    Creation,
    /// A change to a real system.
    Change,
    /// An import.
    Import,
}

/// A queued question, by 1-based position or by its exact text.
// `untagged`: serde tries each variant in order, so `2` is a number and
// `"Who owns it?"` is text, with no wrapper object in the JSON.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum QuestionRef {
    /// Its 1-based position, as `open_questions` numbers it.
    Number(u64),
    /// Its exact text.
    Text(String),
}

/// `stamp`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct StampParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// `generated` (content written) or `verified` (checked against the real system).
    pub field: StampFieldParam,
    /// Who did it (default: the key's actor); say `human:<name>` only when a person really did.
    pub by: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `review`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReviewParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The review's outcome, as `okf.py review` takes it.
    pub outcome: String,
    /// What the review found.
    pub note: String,
    /// Questions to queue in `open_questions`.
    pub questions: Option<Vec<String>>,
    /// Who reviewed (default: the key's actor).
    pub by: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `question`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct QuestionParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The question to queue in `open_questions`.
    pub question: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `unquestion`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct UnquestionParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The question to remove: its 1-based number (as `open_questions` shows) or its exact text.
    pub question: QuestionRef,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `add_source`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct AddSourceParams {
    /// The concept, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The source's id, unique in the concept's `sources`.
    pub id: String,
    /// Where it is: a URL or a path.
    pub resource: String,
    /// Its title.
    pub title: Option<String>,
    /// Its author.
    pub author: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `log`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct LogParams {
    /// The entry; whitespace is collapsed to single spaces.
    pub text: String,
    /// Update, Creation, Change or Import (default Update).
    pub kind: Option<LogKindParam>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `index`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct IndexParams {
    /// The concept to list, e.g. `/infra/ntp.md`; its folder's index.md must exist.
    pub path: String,
    /// The summary after the link.
    pub summary: String,
    /// The index.md section to list it under (default: where `okf.py index` puts it).
    pub section: Option<String>,
    /// The link text (default: the concept's title).
    pub title: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

// `From` lets `field.into()` turn the parameter into the op's type. Each
// `match` is exhaustive: a new variant on either side fails to compile here.
impl From<StampFieldParam> for StampField {
    fn from(field: StampFieldParam) -> Self {
        match field {
            StampFieldParam::Generated => StampField::Generated,
            StampFieldParam::Verified => StampField::Verified,
        }
    }
}

impl From<LogKindParam> for LogKind {
    fn from(kind: LogKindParam) -> Self {
        match kind {
            LogKindParam::Update => LogKind::Update,
            LogKindParam::Creation => LogKind::Creation,
            LogKindParam::Change => LogKind::Change,
            LogKindParam::Import => LogKind::Import,
        }
    }
}

impl QuestionRef {
    /// The text `okf.py unquestion` takes: a number as its digits, or the text.
    fn into_argument(self) -> String {
        match self {
            QuestionRef::Number(number) => number.to_string(),
            QuestionRef::Text(text) => text,
        }
    }
}

/// `okf.py stamp`: writes `generated` or `verified` as `{ by, at: now }`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, the field and `by`.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's refusal of a concept without (parseable) frontmatter.
pub(crate) async fn stamp(ctx: &ToolContext, params: StampParams) -> Result<WriteReport, OkfError> {
    let StampParams { path, field, by, catalog } = params;
    apply(ctx, catalog.as_deref(), by.as_deref(), move |ws, op_ctx| {
        ops::stamp(ws, op_ctx, &path, field.into())
    })
    .await
}

/// `okf.py review`: writes `reviewed` and queues any questions.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, outcome, note, questions and `by`.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`].
pub(crate) async fn review(ctx: &ToolContext, params: ReviewParams) -> Result<WriteReport, OkfError> {
    let ReviewParams { path, outcome, note, questions, by, catalog } = params;
    apply(ctx, catalog.as_deref(), by.as_deref(), move |ws, op_ctx| {
        // `unwrap_or_default` gives an empty list when no questions were sent.
        ops::review(ws, op_ctx, &path, &outcome, &note, &questions.unwrap_or_default())
    })
    .await
}

/// `okf.py question`: queues a question in `open_questions`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the question.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`].
pub(crate) async fn question(ctx: &ToolContext, params: QuestionParams) -> Result<WriteReport, OkfError> {
    let QuestionParams { path, question, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, op_ctx| ops::question(ws, op_ctx, &path, &question)).await
}

/// `okf.py unquestion`: removes a queued question by number or text.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the question.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`]; the op's refusal of an unknown question.
pub(crate) async fn unquestion(ctx: &ToolContext, params: UnquestionParams) -> Result<WriteReport, OkfError> {
    let UnquestionParams { path, question, catalog } = params;
    let question = question.into_argument();
    apply(ctx, catalog.as_deref(), None, move |ws, op_ctx| ops::unquestion(ws, op_ctx, &path, &question)).await
}

/// `okf.py source`: adds an entry to `sources`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept and the source.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`stamp`]; [`OkfError::Shape`] if `sources` is not a list.
pub(crate) async fn add_source(ctx: &ToolContext, params: AddSourceParams) -> Result<WriteReport, OkfError> {
    let AddSourceParams { path, id, resource, title, author, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, op_ctx| {
        let entry = SourceEntry {
            id: &id,
            resource: &resource,
            title: title.as_deref(),
            author: author.as_deref(),
        };
        ops::source(ws, op_ctx, &path, &entry)
    })
    .await
}

/// `okf.py log`: adds a bullet under today's heading in `log.md`
/// (replayable).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the text and kind (default `Update`).
///
/// # Returns
/// The write's report; nothing touched if the bullet is already there today.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`] for a catalog without `log.md`.
pub(crate) async fn log(ctx: &ToolContext, params: LogParams) -> Result<WriteReport, OkfError> {
    let LogParams { text, kind, catalog } = params;
    let kind = LogKind::from(kind.unwrap_or_default());
    apply(ctx, catalog.as_deref(), None, move |ws, op_ctx| ops::log(ws, op_ctx, kind, &text)).await
}

/// `okf.py index`: lists a concept in its folder's `index.md` (replayable).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, summary, section and title.
///
/// # Returns
/// The write's report; nothing touched if the index already links it.
///
/// # Errors
/// As [`stamp`]; the op's refusal of a folder without `index.md` or a
/// concept without a title.
pub(crate) async fn index(ctx: &ToolContext, params: IndexParams) -> Result<WriteReport, OkfError> {
    let IndexParams { path, summary, section, title, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, op_ctx| {
        ops::index(ws, op_ctx, &path, &summary, section.as_deref(), title.as_deref())
    })
    .await
}

// The bookkeeping tools; `OkfServer::tools` joins `bookkeeping_router()`.
#[tool_router(router = bookkeeping_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `stamp` tool.
    #[tool(
        description = "okf.py stamp: set a concept's `generated` (content written) or `verified` \
                       (checked against the real system) to { by, at: now }, in this connection's \
                       changeset. `by` defaults to the key's actor. Write access."
    )]
    async fn stamp(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<StampParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(stamp(&ctx, params).await)
    }

    /// The `review` tool.
    #[tool(
        description = "okf.py review: record a review (outcome, note, by, at: now) in a concept's \
                       `reviewed`, queueing any `questions`, in this connection's changeset. \
                       Write access."
    )]
    async fn review(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReviewParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(review(&ctx, params).await)
    }

    /// The `question` tool.
    #[tool(
        description = "okf.py question: queue a question in a concept's open_questions, in this \
                       connection's changeset. Write access."
    )]
    async fn question(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<QuestionParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(question(&ctx, params).await)
    }

    /// The `unquestion` tool.
    #[tool(
        description = "okf.py unquestion: remove a queued question, by its number in \
                       `open_questions` (from 1) or its exact text, in this connection's \
                       changeset. Write access."
    )]
    async fn unquestion(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<UnquestionParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(unquestion(&ctx, params).await)
    }

    /// The `add_source` tool.
    #[tool(
        description = "okf.py source: add an entry (id, resource, title, author) to a concept's \
                       `sources`, in this connection's changeset. Write access."
    )]
    async fn add_source(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<AddSourceParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(add_source(&ctx, params).await)
    }

    /// The `log` tool.
    #[tool(
        description = "okf.py log: add `* **<kind>:** <text>` under today's UTC date in log.md, \
                       the only way to write log.md. Replayed at publish onto a log.md that \
                       changed meanwhile, so concurrent entries never conflict. Write access."
    )]
    async fn log(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<LogParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(log(&ctx, params).await)
    }

    /// The `index` tool.
    #[tool(
        description = "okf.py index: list a concept in its folder's index.md as \
                       `* [Title](name.md) - summary`; nothing happens if it is already listed. \
                       Replayed at publish onto an index.md that changed meanwhile. Write access."
    )]
    async fn index(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<IndexParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(index(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, add `+ Self::bookkeeping_router()` after `+ Self::writes_router()`.

Run: `cargo test --lib -- mcp::writes`
Expected: all pass (3 in `tests`, 4 in `bookkeeping_tests`).

- [ ] **Step 3: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-09-bookkeeping-tools.md`. It needs Added (the seven tools, `StampFieldParam`, `LogKindParam`, `QuestionRef`, `bookkeeping_router`, the four tests), Changed (`OkfServer::tools`) and Notes. The Notes say:
- why it is a separate router;
- that `unquestion` takes a number or text (`untagged`);
- that the frontmatter commands store no replay, so they conflict on a stale base like any free edit.

Run every gate. Commit:

```bash
git add src/mcp/ changelog/
git commit -m "feat: the bookkeeping write tools: stamp, review, question, unquestion, add_source, log, index (M5.09)" \
    -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: `publish`

Publish turns a changeset into the catalog's published documents, following the design's "Publish" steps 1 to 6:
1. nothing to publish;
2. the lock and the generation;
3. base revisions, replay or `Conflict`;
4. the rules, lint and links on the result;
5. documents, revisions and the generation;
6. the snapshot.

It runs under the catalog's publish lock: the in-process `CatalogState::publish_lock` mutex, then M3's advisory lock (`store::catalogs::lock`, the `pg_advisory_xact_lock` import takes), then the catalog row (`lock_catalog`) and the changeset row (`changesets::lock`, Task 8). The document-writing SQL that M3's import keeps private moves to `store::documents`, so publish and import share it.

> **Interfaces note (Task 10), 2026-11-03.**
> - **`PublishReport::revisions` is `Vec<PublishedRevision { path, revision, op }>`, not `Vec<(String, i32)>`.** A tuple serializes as a JSON array, which an agent reads less reliably than an object, and the op (`create`, `update`, `delete`) is worth reporting.
> - **`PublishReport` derives `Default`, `Serialize` and `JsonSchema`** (it is part of the tool's output), and its paths are `/`-prefixed.
> - **`store::documents` gains `DocumentWrite`, `RevisionOp`, `RevisionRecord`, `write_document`, `delete_document` and `record_revision`.** They are moved out of `store::import`, which now uses them. The refactor is clean: import's private `Write` struct carried the same fields plus `&ImportOptions`, and only the revision's op, message and `changeset_id` differ between the two callers.
> - **`publish` takes `actor: &str`** (`publish(app, state, changeset, actor, message)`): the publishing key's actor goes on every document and revision. Task 12's `adopt` keeps a changeset's original `actor`, so `ChangesetRow::actor` would credit the opener rather than the key that published.
> - **Publish's database tests live in `src/mcp/changesets.rs`** (`publish_tests`), not in `src/store/publish.rs`. They need an `App` and write tools to make entries, which `mcp::testing` provides, and store tests do not reach into `mcp` (PR #84 review).

**Files:**
- Modify: `src/error.rs` (`LintFailed`, `Conflict`, `ConflictFile`)
- Modify: `src/http/mod.rs` (their statuses)
- Modify: `src/catalog.rs` (`publish_lock`)
- Modify: `src/store/documents.rs` (the shared writers), `src/store/import.rs` (use them)
- Create: `src/store/publish.rs`; modify `src/store/mod.rs` (`pub mod publish;`)
- Create: `src/mcp/changesets.rs`; modify `src/mcp/mod.rs` (`mod changesets;`, `+ Self::publish_router()`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Publish", "Errors")
- Create: `changelog/<ts>-m5-task-10-publish.md`

- [ ] **Step 1: The errors (test first)**

Add to `src/error.rs`'s tests:

```rust
    #[test]
    fn publish_refusals_carry_their_findings_and_files() {
        let finding = |file: &str, rule: &str| Finding {
            file: file.to_string(),
            line: 3,
            rule: rule.to_string(),
            message: "m".to_string(),
        };
        let lint = OkfError::LintFailed {
            errors: vec![finding("a.md", "broken-link")],
            warnings: vec![finding("b.md", "orphan")],
        };
        let conflict = OkfError::Conflict {
            files: vec![ConflictFile {
                path: "/a.md".to_string(),
                base_revision: Some(1),
                current_revision: Some(2),
            }],
        };

        let lint_body = lint.to_json();
        let conflict_body = conflict.to_json();

        assert!(!lint.is_internal() && !conflict.is_internal());
        assert_eq!(lint_body["error"], "LintFailed");
        assert_eq!(lint_body["errors"][0]["rule"], "broken-link");
        assert_eq!(lint_body["warnings"][0]["file"], "b.md");
        assert_eq!(
            conflict_body["files"],
            serde_json::json!([{"path": "/a.md", "base_revision": 1, "current_revision": 2}])
        );
        assert!(conflict_body["message"].as_str().unwrap().contains("re-apply"));
    }
```

Add to `src/http/mod.rs`'s tests:

```rust
    #[test]
    fn publish_refusals_have_their_statuses() {
        let conflict = AppError(OkfError::Conflict { files: Vec::new() });
        let lint = AppError(OkfError::LintFailed { errors: Vec::new(), warnings: Vec::new() });

        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(lint.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
```

Run: `cargo test --lib -- error::tests http::tests`
Expected: compile errors (no such variants).

Add the variants to `OkfError` (after Task 5's `SecretDetected`, or at the end of the enum when merging):

```rust
    /// Publish was refused: lint errors in the files the changeset changes,
    /// or links from other documents into paths it deletes.
    #[error("publish refused: {} error(s) in the changed files; fix them and publish again", .errors.len())]
    LintFailed {
        /// The refusing findings (`file` is catalog-relative, no leading `/`).
        errors: Vec<Finding>,
        /// Advice: errors in files the changeset does not change, and
        /// warnings in those it does.
        warnings: Vec<Finding>,
    },

    /// Publish found documents changed since the changeset read them.
    #[error(
        "publish conflict: {} file(s) changed since this changeset read them; re-read, \
         re-apply and publish again",
        .files.len()
    )]
    Conflict {
        /// Each conflicting file, with the revision its entry was based on
        /// and the published one.
        files: Vec<ConflictFile>,
    },
```

After the enum:

```rust
/// One file of a [`OkfError::Conflict`] (a record, so its fields are public).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ConflictFile {
    /// `/`-prefixed path.
    pub path: String,
    /// The published revision the changeset's entry was made on (`None`: the
    /// path had no live document then).
    pub base_revision: Option<i32>,
    /// The published revision now (`None`: no live document).
    pub current_revision: Option<i32>,
}
```

with `use crate::okf::analysis::Finding;` at the top. In `kind`, add
`OkfError::LintFailed { .. } => "LintFailed",` and `OkfError::Conflict { .. } => "Conflict",`. In `to_json`'s `match`, add:

```rust
            OkfError::LintFailed { errors, warnings } => {
                body["errors"] = json_list(errors);
                body["warnings"] = json_list(warnings);
            }
            OkfError::Conflict { files } => body["files"] = json_list(files),
```

and, after `impl OkfError`:

```rust
/// A list as JSON, for an error body. `Finding` and `ConflictFile` are plain
/// records that always serialize; the `Null` fallback only keeps this free
/// of `unwrap`.
fn json_list<T: serde::Serialize>(items: &[T]) -> serde_json::Value {
    serde_json::to_value(items).unwrap_or_default()
}
```

Neither is internal: `is_internal` stays as it is.

In `AppError::status` (`src/http/mod.rs`), add `| OkfError::Conflict { .. }` to the `CONFLICT` arm (beside `Refused`) and a new arm `OkfError::LintFailed { .. } => StatusCode::UNPROCESSABLE_ENTITY,`. Extend its doc: "409 for a refused request or a publish conflict, 422 for a publish whose result fails lint".

**Why 422 for `LintFailed`:** the request is well formed and every argument is valid. What fails is the catalog state it would produce. RFC 9110 gives 422 to exactly that: "the server understands the content type of the request content, and the syntax of the request content is correct, but it was unable to process the contained instructions". 400 already means a bad argument here (`InvalidPath`, `FrontmatterParse`, `ImportRefused`), and the agent's fix is different: edit the documents, not the call. 409 stays for `Conflict`, where the state moved under the request. No route publishes today (publish is a tool), so the status is for completeness and any future route.

Run: `cargo test --lib -- error::tests http::tests`
Expected: all pass.

- [ ] **Step 2: The in-process publish lock**

In `src/catalog.rs`, add a field to `CatalogState`, after `rebuilding`:

```rust
    // The in-process half of the publish lock (design "Publish"): one
    // publish of this catalog at a time in this server, held through the
    // snapshot refresh after commit, so the next publish checks against the
    // new snapshot. The other half is the advisory lock in the publish
    // transaction, which also serializes against a CLI import.
    publishing: tokio::sync::Mutex<()>,
```

initialize it in `load` (`publishing: tokio::sync::Mutex::new(()),`), and add after `current`:

```rust
    /// The in-process publish lock (design "Publish"). `store::publish`
    /// holds it for a whole publish, refresh included.
    ///
    /// # Returns
    /// The mutex; lock it with `.lock().await`.
    pub fn publish_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.publishing
    }
```

- [ ] **Step 3: The shared document writers, and import on them**

In `src/store/documents.rs`, add (imports: `sqlx::PgConnection`, `crate::okf::workspace::StoredFile`):

```rust
/// Which document one write touches, at which revision, by whom and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentWrite<'a> {
    /// The catalog.
    pub catalog_id: i64,
    /// Catalog-relative path, as stored.
    pub path: &'a str,
    /// The new revision (the old one + 1, tombstones included; 1 for a new path).
    pub revision: i32,
    /// Recorded as `updated_by` and the revision's `actor`.
    pub actor: &'a str,
    /// Recorded as `updated_at` (and `deleted_at`) and the revision's `created_at`.
    pub now: DateTime<Utc>,
}

/// A revision's operation (the schema's `CHECK` list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionOp {
    /// A publish wrote a path that had no live document.
    Create,
    /// A publish changed a live document.
    Update,
    /// A publish or an import removed a live document.
    Delete,
    /// An import added or changed a document.
    Import,
}

impl RevisionOp {
    /// The spelling stored in `revisions.op`.
    ///
    /// # Returns
    /// `"create"`, `"update"`, `"delete"` or `"import"`.
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionOp::Create => "create",
            RevisionOp::Update => "update",
            RevisionOp::Delete => "delete",
            RevisionOp::Import => "import",
        }
    }
}

/// What a `revisions` row records besides the document's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionRecord<'a> {
    /// The operation.
    pub op: RevisionOp,
    /// The content written (`None` for a delete: the row's content is NULL).
    pub file: Option<&'a StoredFile>,
    /// The changeset published (`None` for an import).
    pub changeset_id: Option<i64>,
    /// The publish message (`import` for an import).
    pub message: &'a str,
}

impl<'a> RevisionRecord<'a> {
    /// An import's record: `import` with the content, or `delete` without.
    ///
    /// # Arguments
    /// * `file` - the imported file, or `None` for a file the import drops.
    ///
    /// # Returns
    /// The record, with the message `import` and no changeset.
    pub fn import(file: Option<&'a StoredFile>) -> Self {
        Self {
            op: if file.is_some() { RevisionOp::Import } else { RevisionOp::Delete },
            file,
            changeset_id: None,
            message: "import",
        }
    }
}

/// Inserts or overwrites a document (reviving a tombstone), with its
/// derived columns.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `write` - path, revision, actor and time.
/// * `file` - the content and execute bit.
/// * `derived` - [`Derived::of`] the content's snapshot document.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn write_document(
    conn: &mut PgConnection,
    write: &DocumentWrite<'_>,
    file: &StoredFile,
    derived: &Derived,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO documents (catalog_id, path, content, executable, revision, deleted_at, \
                                updated_at, updated_by, kind, concept_type, title, status, tags) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (catalog_id, path) DO UPDATE SET \
             content = EXCLUDED.content, executable = EXCLUDED.executable, \
             revision = EXCLUDED.revision, deleted_at = NULL, \
             updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by, \
             kind = EXCLUDED.kind, concept_type = EXCLUDED.concept_type, \
             title = EXCLUDED.title, status = EXCLUDED.status, tags = EXCLUDED.tags",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(&file.content)
    .bind(file.executable)
    .bind(write.revision)
    .bind(write.now)
    .bind(write.actor)
    .bind(derived.kind)
    .bind(&derived.concept_type)
    .bind(&derived.title)
    .bind(&derived.status)
    .bind(&derived.tags)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Turns a live document into a tombstone (empty content, no derived columns).
///
/// # Arguments
/// * `conn` - the transaction's connection.
/// * `write` - path, revision, actor and time (also `deleted_at`).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn delete_document(conn: &mut PgConnection, write: &DocumentWrite<'_>) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE documents SET content = ''::bytea, executable = false, revision = $3, \
             deleted_at = $4, updated_at = $4, updated_by = $5, kind = NULL, \
             concept_type = NULL, title = NULL, status = NULL, tags = '{}' \
         WHERE catalog_id = $1 AND path = $2",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(write.revision)
    .bind(write.now)
    .bind(write.actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Appends a history row.
///
/// # Arguments
/// * `conn` - the transaction's connection.
/// * `write` - path, revision, actor and time.
/// * `record` - op, content, changeset and message.
///
/// # Errors
/// [`OkfError::Database`]: a unique violation if the revision already
/// exists (which the publish lock rules out; publish maps it to a conflict).
pub async fn record_revision(
    conn: &mut PgConnection,
    write: &DocumentWrite<'_>,
    record: &RevisionRecord<'_>,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                changeset_id, message, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(write.revision)
    .bind(record.op.as_str())
    .bind(record.file.map(|file| &file.content))
    .bind(record.file.is_some_and(|file| file.executable))
    .bind(write.actor)
    .bind(record.changeset_id)
    .bind(record.message)
    .bind(write.now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
```

In `src/store/import.rs`, delete the private `Write` struct and its `write_document`, `delete_document` and `record_revision`. Import `use crate::store::documents::{self, Derived, DocumentWrite, RevisionRecord};` and write the two loops' bodies as:

```rust
        let write = DocumentWrite {
            catalog_id: found.id,
            path,
            revision,
            actor: options.actor,
            now,
        };
        documents::write_document(&mut tx, &write, file, &derived).await?;
        documents::record_revision(&mut tx, &write, &RevisionRecord::import(Some(file))).await?;
```

```rust
        let write = DocumentWrite {
            catalog_id: found.id,
            path: &row.path,
            revision: row.revision + 1,
            actor: options.actor,
            now,
        };
        documents::delete_document(&mut tx, &write).await?;
        documents::record_revision(&mut tx, &write, &RevisionRecord::import(None)).await?;
```

Import's behaviour is unchanged, so its tests are the check. Add one for the record's op:

```rust
    #[test]
    fn an_import_record_is_import_or_delete_with_the_message_import() {
        let file = StoredFile { content: b"x".to_vec(), executable: false };

        let added = RevisionRecord::import(Some(&file));
        let dropped = RevisionRecord::import(None);

        assert_eq!((added.op.as_str(), added.message, added.changeset_id), ("import", "import", None));
        assert_eq!(dropped.op, RevisionOp::Delete);
    }
```

(in `store::documents`' tests, with `use crate::okf::workspace::StoredFile;`).

Run: `cargo test --lib -- store::import store::documents store::history`
Expected: all pass (the import tests unchanged).

- [ ] **Step 4: Write the failing publish tests**

Create `src/mcp/changesets.rs` with Task 10's test module (Task 11 and 12 add their own beside it):

```rust
#[cfg(test)]
mod publish_tests {
    use super::*;
    use chrono::Utc;

    use crate::config::Config;
    use crate::error::ConflictFile;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::mcp::writes::{index, log, replace_body, IndexParams, LogParams, ReplaceBodyParams};
    use crate::store::changesets::{ChangesetState, EntryRow};
    use crate::store::publish::PublishedRevision;
    use crate::store::{catalogs, history};
    use crate::write::WriteReport;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
    const X: &str = "---\ntype: Note\ntitle: X\ndescription: X.\n---\n# X\n";
    const Y: &str = "---\ntype: Note\ntitle: Y\ndescription: Y.\n---\n# Y\n";
    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\nSee [X](/infra/x.md).\n";

    async fn pg() -> TestApp {
        let files: &[(&str, &str)] = &[
            ("a.md", NOTE),
            ("hub.md", HUB),
            ("infra/index.md", "# Infra\n"),
            ("infra/x.md", X),
            ("infra/y.md", Y),
            ("log.md", "# Log\n"),
        ];
        app_with(&[("pg", files)], Config::default()).await
    }

    async fn body(ctx: &ToolContext, path: &str, text: &str) -> WriteReport {
        let params = ReplaceBodyParams { path: path.to_string(), body: text.to_string(), catalog: None };
        replace_body(ctx, params).await.unwrap()
    }

    async fn publish_as(ctx: &ToolContext, message: &str) -> Result<Published, OkfError> {
        publish(ctx, PublishParams { message: message.to_string(), catalog: None }).await
    }

    fn published_text(test: &TestApp, path: &str) -> String {
        let loaded = test.app.catalog("pg").unwrap().current();
        loaded.snapshot().get(path).map(|doc| doc.text().to_string()).unwrap_or_default()
    }

    async fn state_of(test: &TestApp, changeset: i64) -> ChangesetState {
        changesets::by_id(test.app.pool(), changeset).await.unwrap().unwrap().state
    }

    /// Stores an entry directly, as no write tool would (a rule changed, a
    /// hand edit), to show publish checks again.
    async fn insert_entry(test: &TestApp, key_id: &str, entry: EntryRow) {
        let state = test.app.catalog("pg").unwrap();
        let mut tx = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut tx, state.id()).await.unwrap();
        let key = ChangesetKey { catalog_id: state.id(), key_id, label: "default" };
        let row = changesets::get_or_create(&mut tx, &key, key_id, Utc::now()).await.unwrap();
        changesets::upsert_entry(&mut tx, row.id, &entry, Utc::now()).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn publish_writes_revisions_bumps_the_generation_and_swaps_the_snapshot() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        let before = catalogs::find(test.app.pool(), "pg").await.unwrap().generation;
        let written = body(&ctx, "/a.md", "# A\n\nNew.\n").await;

        let published = publish_as(&ctx, "edit a").await.unwrap();

        assert!(published.report.published);
        assert_eq!(published.changeset, Some(written.changeset));
        assert_eq!(
            published.report.revisions,
            [PublishedRevision { path: "/a.md".to_string(), revision: 2, op: "update" }]
        );
        assert!(published.report.replayed.is_empty());
        assert_eq!(catalogs::find(test.app.pool(), "pg").await.unwrap().generation, before + 1);
        assert!(published_text(&test, "a.md").ends_with("# A\n\nNew.\n"));
        let rows = history::revisions(test.app.pool(), state.id(), "a.md", 5).await.unwrap();
        assert_eq!(
            (rows[0].revision, rows[0].op.as_str(), rows[0].actor.as_str(), rows[0].message.as_str()),
            (2, "update", "writer-a", "edit a")
        );
        let changeset_id: Option<i64> = sqlx::query_scalar(
            "SELECT changeset_id FROM revisions WHERE path = 'a.md' AND revision = 2",
        )
        .fetch_one(test.app.pool())
        .await
        .unwrap();
        assert_eq!(changeset_id, Some(written.changeset));
        assert!(matches!(state_of(&test, written.changeset).await, ChangesetState::Published));
        // With no open changeset left, this connection reads the published state.
        assert!(!ctx.view(&state).await.unwrap().pending());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_to_publish_is_a_result_and_a_message_is_required() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let nothing = publish_as(&ctx, "nothing").await.unwrap();
        let blank = publish_as(&ctx, "  ").await.unwrap_err();

        assert!(!nothing.report.published);
        assert_eq!(nothing.changeset, None);
        assert_eq!(blank.kind(), "Refused");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_free_edit_conflicts_and_leaves_the_changeset_open() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        body(&first, "/a.md", "From a.\n").await;
        let mine = body(&second, "/a.md", "From b.\n").await;
        publish_as(&first, "a first").await.unwrap();

        let error = publish_as(&second, "b second").await.unwrap_err();

        assert!(matches!(
            error,
            OkfError::Conflict { ref files } if files == &[ConflictFile {
                path: "/a.md".to_string(),
                base_revision: Some(1),
                current_revision: Some(2),
            }]
        ));
        assert!(matches!(state_of(&test, mine.changeset).await, ChangesetState::Open));
        assert_eq!(changesets::entries(test.app.pool(), mine.changeset).await.unwrap().len(), 1);
        assert!(published_text(&test, "a.md").ends_with("From a.\n"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_log_entries_both_publish_and_keep_both_bullets() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        for (ctx, text) in [(&first, "from a"), (&second, "from b")] {
            log(ctx, LogParams { text: text.to_string(), kind: None, catalog: None }).await.unwrap();
        }

        // Both at once: the publish lock runs them one after the other, and
        // the second replays its entry onto the first's log.md.
        let (one, two) = tokio::join!(publish_as(&first, "a"), publish_as(&second, "b"));

        let (one, two) = (one.unwrap(), two.unwrap());
        assert!(one.report.published && two.report.published);
        let replayed: Vec<&String> = one.report.replayed.iter().chain(&two.report.replayed).collect();
        assert_eq!(replayed, ["/log.md"]);
        let text = published_text(&test, "log.md");
        assert!(text.contains("* **Update:** from a") && text.contains("* **Update:** from b"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_index_listing_is_replayed() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        for (ctx, path, summary) in [(&first, "/infra/x.md", "the x"), (&second, "/infra/y.md", "the y")] {
            let params = IndexParams {
                path: path.to_string(),
                summary: summary.to_string(),
                section: None,
                title: None,
                catalog: None,
            };
            index(ctx, params).await.unwrap();
        }
        publish_as(&first, "list x").await.unwrap();

        let published = publish_as(&second, "list y").await.unwrap();

        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        let text = published_text(&test, "infra/index.md");
        assert!(text.contains("[X](x.md) - the x") && text.contains("[Y](y.md) - the y"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lint_error_in_a_changed_file_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let written = body(&ctx, "/a.md", "See [gone](/gone.md).\n").await;

        let error = publish_as(&ctx, "broken").await.unwrap_err();

        // `&error`: the pattern borrows, so the `else` branch can still print it.
        let OkfError::LintFailed { errors, .. } = &error else { panic!("not LintFailed: {error:?}") };
        assert!(errors.iter().any(|finding| finding.file == "a.md" && finding.rule == "broken-link"));
        assert!(matches!(state_of(&test, written.changeset).await, ChangesetState::Open));
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deletion_still_linked_from_elsewhere_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let deletion = EntryRow { path: "infra/x.md".to_string(), base_revision: Some(1), content: None, executable: false, replay: None };
        insert_entry(&test, "writer-a", deletion).await;

        let error = publish_as(&ctx, "drop x").await.unwrap_err();

        let OkfError::LintFailed { errors, .. } = &error else { panic!("not LintFailed: {error:?}") };
        assert!(errors.iter().any(|finding| finding.file == "hub.md" && finding.rule == "link-into-deleted"));
        assert_eq!(published_text(&test, "infra/x.md"), X);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_secret_is_refused_at_publish_too() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // Built at run time, so no source file holds a secret-shaped string.
        let leaked = format!("{NOTE}password = {}\n", "Ab1".repeat(4));
        let entry = EntryRow { path: "a.md".to_string(), base_revision: Some(1), content: Some(leaked.into_bytes()), executable: false, replay: None };
        insert_entry(&test, "writer-a", entry).await;

        let error = publish_as(&ctx, "leak").await.unwrap_err();

        assert_eq!(error.kind(), "SecretDetected");
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }
}
```

Run: `cargo test --lib -- mcp::changesets`
Expected: compile errors (`publish`, `PublishParams`, `Published`, `store::publish` not defined).

- [ ] **Step 5: Implement publish**

Create `src/store/publish.rs`:

```rust
//! Publish (design "Data flow", Publish): a changeset's entries become the
//! catalog's published documents, under the catalog's publish lock.
//!
//! The lock is two locks, taken in this order. First the in-process mutex
//! ([`CatalogState::publish_lock`]), held until the snapshot is swapped.
//! Then the advisory lock in the transaction, which a CLI import takes too.
//! The transaction then takes the catalog row (`lock_catalog`) and the
//! changeset row (`changesets::lock`), in the order every changeset
//! transaction uses. While the locks are held, nothing else can change the
//! catalog's documents or this changeset's entries, so the checks see
//! exactly what is written.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::PgConnection;

use crate::app::App;
use crate::catalog::{CatalogState, Loaded};
use crate::error::{ConflictFile, OkfError};
use crate::mcp::blocking;
use crate::okf::analysis::lint::LintReport;
use crate::okf::analysis::Finding;
use crate::okf::ops::replay::{replay, ReplayOp};
use crate::okf::rules;
use crate::okf::snapshot::Snapshot;
use crate::overlay::{build_overlay, Overlay};
use crate::store::catalogs::{self, is_unique_violation};
use crate::store::changesets::{self, ChangesetRow, ChangesetState, EntryRow};
use crate::store::documents::{self, Derived, DocumentWrite, RevisionOp, RevisionRecord};

/// What a publish did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PublishReport {
    /// `false` when the changeset held nothing to publish.
    pub published: bool,
    /// The revisions written, in path order. A file whose content ended up as published gets none.
    pub revisions: Vec<PublishedRevision>,
    /// Lint advice: errors in files this publish does not change, and warnings in those it does.
    pub warnings: Vec<Finding>,
    /// `/`-prefixed paths whose bookkeeping (log, index listings) was replayed onto newer text.
    pub replayed: Vec<String>,
}

/// One revision a publish wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PublishedRevision {
    /// `/`-prefixed path.
    pub path: String,
    /// Its new revision.
    pub revision: i32,
    /// `create`, `update` or `delete`.
    pub op: &'static str,
}

/// A changed path's row in `documents`, tombstone or live.
struct CurrentRow {
    content: Vec<u8>,
    executable: bool,
    revision: i32,
    live: bool,
}

/// The entries once replayed onto current text.
struct Merged {
    entries: Vec<EntryRow>,
    replayed: Vec<String>,
}

/// The publish's identity: what every row it writes records.
struct Publication<'a> {
    changeset: &'a ChangesetRow,
    actor: &'a str,
    message: &'a str,
    now: DateTime<Utc>,
}

/// Publishes a changeset (design "Publish", steps 1 to 6).
///
/// # Arguments
/// * `app` - the shared state (pool, clock, overlays).
/// * `state` - the changeset's catalog.
/// * `changeset` - the open changeset (read without its lock; it is locked
///   and read again here).
/// * `actor` - the publishing key's actor, recorded on every document and
///   revision (an adopted changeset keeps its opener's `actor`).
/// * `message` - recorded on every revision.
///
/// # Returns
/// The revisions written, lint advice and the replayed paths;
/// `published: false` if the changeset held no entries.
///
/// # Errors
/// - [`OkfError::Conflict`] for entries whose base is stale and that cannot be replayed;
/// - [`OkfError::SecretDetected`] or [`OkfError::FrontmatterParse`] from the content rules;
/// - [`OkfError::LintFailed`] for lint errors in changed files or links into deleted paths;
/// - [`OkfError::Refused`] if the changeset is no longer open;
/// - [`OkfError::CatalogNotSelected`] if the catalog was deleted;
/// - [`OkfError::Busy`] if the snapshot could not be brought up to date;
/// - [`OkfError::Database`].
///
/// On every error the transaction rolls back and the changeset stays as it was.
pub async fn publish(
    app: &App,
    state: &CatalogState,
    changeset: &ChangesetRow,
    actor: &str,
    message: &str,
) -> Result<PublishReport, OkfError> {
    // Step 1, before any lock: an empty changeset never waits for one.
    if changesets::entries(app.pool(), changeset.id).await?.is_empty() {
        return Ok(PublishReport::default());
    }
    // Step 2. The guard (`_publishing`) releases the mutex when it goes out
    // of scope at the end of the function, after the refresh.
    let _publishing = state.publish_lock().lock().await;
    let now = app.clock().now();
    let mut tx = app.pool().begin().await?;
    catalogs::lock(&mut tx, state.id()).await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    let locked = changesets::lock(&mut tx, changeset.id)
        .await?
        .filter(|row| matches!(row.state, ChangesetState::Open))
        .ok_or_else(|| OkfError::Refused {
            reason: format!("changeset {} is no longer open", changeset.id),
        })?;
    // Again under the lock: a discard may have emptied it meanwhile.
    let entries = changesets::entries(&mut *tx, locked.id).await?;
    if entries.is_empty() {
        return Ok(PublishReport::default());
    }
    let base = current_base(app, state, &mut tx).await?;

    // Step 3: base revisions against the documents as stored now.
    let paths: Vec<String> = entries.iter().map(|entry| entry.path.clone()).collect();
    let current = current_rows(&mut tx, state.id(), &paths).await?;
    let merged = merge(entries, &current)?;

    // Step 4: the rules, lint and links on the result.
    let (overlay, warnings) = check(base, &locked, merged.entries, now).await?;

    // Step 5: documents, revisions, generation, state; one commit.
    let publication = Publication { changeset: &locked, actor, message, now };
    let revisions = write_all(&mut tx, &publication, &overlay, &current, &paths).await?;
    if !revisions.is_empty() {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(state.id())
            .execute(&mut *tx)
            .await?;
    }
    changesets::set_state(&mut tx, locked.id, ChangesetState::Published, now).await?;
    tx.commit().await?;

    // Step 6, still under the in-process lock (the advisory lock ended with
    // the transaction): the next publish of this catalog checks against
    // this one's result. A failed refresh does not undo a committed publish;
    // the poll loop retries it, and the next publish refreshes first.
    if let Err(error) = state.refresh(app.pool()).await {
        tracing::error!(catalog = %state.name(), %error, "refresh after publish failed; the poll loop retries");
    }
    app.overlays().forget(locked.id);
    Ok(PublishReport {
        published: true,
        revisions,
        warnings,
        replayed: merged.replayed,
    })
}

/// The snapshot at the catalog's current generation (design "Publish",
/// step 2): rebuilt first if the database moved past it.
///
/// # Errors
/// As [`CatalogState::refresh`]; [`OkfError::Busy`] if the snapshot still
/// does not match (only a refresh failing silently could cause it).
async fn current_base(app: &App, state: &CatalogState, conn: &mut PgConnection) -> Result<Arc<Loaded>, OkfError> {
    let generation: i64 = sqlx::query_scalar("SELECT generation FROM catalogs WHERE id = $1")
        .bind(state.id())
        .fetch_one(&mut *conn)
        .await?;
    // The advisory lock keeps every other import and publish out, so the
    // refresh (on its own connection) builds exactly this generation.
    if state.current().generation() < generation {
        state.refresh(app.pool()).await?;
    }
    let base = state.current();
    if base.generation() == generation {
        Ok(base)
    } else {
        Err(OkfError::Busy {
            reason: format!("catalog {:?}'s snapshot is behind the database; retry", state.name()),
        })
    }
}

/// The changed paths' rows in `documents` (tombstones included), by path.
async fn current_rows(
    conn: &mut PgConnection,
    catalog_id: i64,
    paths: &[String],
) -> Result<HashMap<String, CurrentRow>, OkfError> {
    // `= ANY($2)` matches any element of the bound array: one query for all
    // changed paths, served by the primary key.
    let rows: Vec<(String, Vec<u8>, bool, i32, bool)> = sqlx::query_as(
        "SELECT path, content, executable, revision, deleted_at IS NULL FROM documents \
         WHERE catalog_id = $1 AND path = ANY($2)",
    )
    .bind(catalog_id)
    .bind(paths)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(path, content, executable, revision, live)| {
            (path, CurrentRow { content, executable, revision, live })
        })
        .collect())
}

/// Checks each entry's base against the current revision (design "Publish",
/// step 3). A fresh entry is kept. A stale one with a replay has its
/// operations redone on the current text. Any other stale one is a conflict.
///
/// # Errors
/// [`OkfError::Conflict`] listing every conflicting file; a replay's error.
fn merge(entries: Vec<EntryRow>, current: &HashMap<String, CurrentRow>) -> Result<Merged, OkfError> {
    let mut merged = Vec::with_capacity(entries.len());
    let mut replayed = Vec::new();
    let mut conflicts = Vec::new();
    // `mut entry`: the loop owns each entry and may rewrite its content.
    for mut entry in entries {
        let live = current.get(&entry.path).filter(|row| row.live);
        let current_revision = live.map(|row| row.revision);
        if entry.base_revision == current_revision {
            merged.push(entry);
            continue;
        }
        match replayed_text(&entry, live)? {
            Some(text) => {
                replayed.push(format!("/{}", entry.path));
                entry.content = Some(text.into_bytes());
                entry.base_revision = current_revision;
                merged.push(entry);
            }
            None => conflicts.push(ConflictFile {
                path: format!("/{}", entry.path),
                base_revision: entry.base_revision,
                current_revision,
            }),
        }
    }
    if conflicts.is_empty() {
        Ok(Merged { entries: merged, replayed })
    } else {
        Err(OkfError::Conflict { files: conflicts })
    }
}

/// A stale entry's content redone on the current text.
///
/// # Returns
/// `None` when it cannot be replayed (a free edit, unreadable operations,
/// or current content that is not text): a conflict.
///
/// # Errors
/// The replay's own error (text it cannot place a bullet in).
fn replayed_text(entry: &EntryRow, live: Option<&CurrentRow>) -> Result<Option<String>, OkfError> {
    let Some(stored) = &entry.replay else {
        return Ok(None);
    };
    // `write::apply` stored these; anything unreadable conflicts instead.
    let Ok(ops) = serde_json::from_value::<Vec<ReplayOp>>(stored.clone()) else {
        return Ok(None);
    };
    let current = match live {
        None => None,
        Some(row) => match std::str::from_utf8(&row.content) {
            Ok(text) => Some(text),
            Err(_) => return Ok(None),
        },
    };
    replay(&entry.path, current, &ops).map(Some)
}

/// The checks on the post-replay result (design "Publish", step 4).
///
/// # Returns
/// The overlay to write from (the current snapshot plus the entries) and
/// the lint advice.
///
/// # Errors
/// A content rule's error; [`OkfError::LintFailed`].
async fn check(
    base: Arc<Loaded>,
    changeset: &ChangesetRow,
    entries: Vec<EntryRow>,
    now: DateTime<Utc>,
) -> Result<(Overlay, Vec<Finding>), OkfError> {
    let changed: BTreeSet<String> = entries.iter().map(|entry| entry.path.clone()).collect();
    let deleted: BTreeSet<String> = entries
        .iter()
        .filter(|entry| entry.content.is_none())
        .map(|entry| entry.path.clone())
        .collect();
    // The secret scan is CPU work; the entries go to the blocking thread and back.
    let entries = blocking(move || check_contents(entries)).await?;
    let overlay = build_overlay(base, changeset, entries).await?;
    let (overlay, findings) = blocking(move || {
        let findings = review(&overlay, &changed, &deleted, now);
        (overlay, findings)
    })
    .await;
    if findings.errors.is_empty() {
        Ok((overlay, findings.warnings))
    } else {
        Err(OkfError::LintFailed { errors: findings.errors, warnings: findings.warnings })
    }
}

/// The content rules on every changed text file. Write already checked
/// each entry, but publish checks the result again: replayed text is new,
/// and an entry may predate a rule.
///
/// # Errors
/// The first refused file's error, in path order (so a refusal is the same
/// on every attempt).
fn check_contents(entries: Vec<EntryRow>) -> Result<Vec<EntryRow>, OkfError> {
    for entry in &entries {
        // A file that is not text is stored and exported but not scanned
        // (design "Storage"); a deletion has no content.
        let text = entry.content.as_deref().and_then(|content| std::str::from_utf8(content).ok());
        if let Some(text) = text {
            rules::check_content(&entry.path, text)?;
        }
    }
    Ok(entries)
}

/// Lint of the overlay, split for publish. Errors in changed files refuse,
/// as do links from unchanged documents into deleted paths. Errors
/// elsewhere become advice, beside the warnings in changed files. Warnings
/// elsewhere are left out: they predate this publish.
fn review(
    overlay: &Overlay,
    changed: &BTreeSet<String>,
    deleted: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> LintReport {
    let report = overlay.lint(now);
    // `Finding::file` has no leading `/`; `trim_start_matches` makes sure.
    let in_changed = |finding: &Finding| changed.contains(finding.file.trim_start_matches('/'));
    // `partition` splits one list in two by the predicate, in one pass.
    let (mut errors, mut warnings): (Vec<Finding>, Vec<Finding>) =
        report.errors.into_iter().partition(&in_changed);
    errors.extend(links_into_deleted(overlay.snapshot(), changed, deleted));
    warnings.extend(report.warnings.into_iter().filter(&in_changed));
    LintReport { errors, warnings }
}

/// Links from documents this publish does not change into paths it deletes
/// (design "Publish", step 4). Changed documents' links into them are
/// already `broken-link` errors in changed files.
fn links_into_deleted(
    snapshot: &Snapshot,
    changed: &BTreeSet<String>,
    deleted: &BTreeSet<String>,
) -> Vec<Finding> {
    if deleted.is_empty() {
        return Vec::new();
    }
    snapshot
        .docs()
        .iter()
        .filter(|doc| doc.in_link_graph() && !changed.contains(doc.path()))
        // `flat_map` turns each document into its offending links, then
        // chains them; `move` gives the inner closure its own `doc` handle.
        .flat_map(|doc| {
            doc.links()
                .iter()
                .filter(|link| !link.in_page && deleted.contains(&link.target))
                .map(move |link| Finding {
                    file: doc.path().to_string(),
                    line: link.line,
                    rule: "link-into-deleted".to_string(),
                    message: format!("links to /{}, which this publish deletes", link.target),
                })
        })
        .collect()
}

/// Writes every changed path from the overlay (design "Publish", step 5).
///
/// # Returns
/// The revisions written, in path order. A path whose result equals the
/// published file gets no revision.
///
/// # Errors
/// [`OkfError::Database`]; a unique violation (two writers of one revision,
/// which the lock rules out) as [`OkfError::Conflict`].
async fn write_all(
    conn: &mut PgConnection,
    publication: &Publication<'_>,
    overlay: &Overlay,
    current: &HashMap<String, CurrentRow>,
    paths: &[String],
) -> Result<Vec<PublishedRevision>, OkfError> {
    let changeset = publication.changeset;
    let mut revisions = Vec::with_capacity(paths.len());
    for path in paths {
        let row = current.get(path);
        let live = row.filter(|row| row.live);
        let file = overlay.workspace().stored(path);
        let op = match (&file, live) {
            (Some(file), Some(row)) if row.content == file.content && row.executable == file.executable => {
                continue
            }
            (Some(_), Some(_)) => RevisionOp::Update,
            (Some(_), None) => RevisionOp::Create,
            (None, Some(_)) => RevisionOp::Delete,
            // A deletion of a path that has no live document: nothing to do.
            (None, None) => continue,
        };
        // A tombstone keeps its number, so a path created again continues it.
        let revision = row.map_or(1, |row| row.revision + 1);
        let write = DocumentWrite {
            catalog_id: changeset.catalog_id,
            path,
            revision,
            actor: publication.actor,
            now: publication.now,
        };
        let record = RevisionRecord {
            op,
            file: file.as_ref(),
            changeset_id: Some(changeset.id),
            message: publication.message,
        };
        let backstop = |error| as_conflict(error, path, live.map(|row| row.revision));
        match &file {
            Some(file) => {
                let derived = Derived::of(overlay.snapshot().get(path));
                documents::write_document(conn, &write, file, &derived).await.map_err(backstop)?;
            }
            None => documents::delete_document(conn, &write).await.map_err(backstop)?,
        }
        documents::record_revision(conn, &write, &record).await.map_err(backstop)?;
        revisions.push(PublishedRevision { path: format!("/{path}"), revision, op: op.as_str() });
    }
    Ok(revisions)
}

/// The design's backstop: a unique violation while writing is a conflict,
/// never the opaque `Database` error.
fn as_conflict(error: OkfError, path: &str, expected: Option<i32>) -> OkfError {
    match error {
        // `if` guard: this arm matches only a unique violation.
        OkfError::Database { source } if is_unique_violation(&source) => OkfError::Conflict {
            files: vec![ConflictFile {
                path: format!("/{path}"),
                base_revision: expected,
                // Unknown: another writer got there first.
                current_revision: None,
            }],
        },
        other => other,
    }
}
```

Add `pub mod publish;` to `src/store/mod.rs`. `store::publish` is the one store module above `App`, the overlay and the blocking helper (`crate::mcp::blocking`). Publish needs all three, and they all sit below the MCP layer.

- [ ] **Step 6: Implement the `publish` tool**

Above the test module in `src/mcp/changesets.rs`. If Task 11 or 12 landed first, the file and `mod changesets;` exist. Keep its module doc, merge the `use` lines below into its own (dropping duplicates: a name imported twice does not compile), and add only the sections. Otherwise create it as shown:

```rust
//! The changeset tools (design "Write tools"):
//! - `publish` (Task 10);
//! - `discard`, `revert` and `pending_changes` (Task 11);
//! - `stranded_sessions` and `adopt_changeset` (Task 12).
//!
//! Each task's tools are one section with its own `#[tool_router]` block and
//! test module, so the tasks merge side by side; the shared lookup
//! ([`own_changeset`]) is at the top.

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogState;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::store::changesets::{self, ChangesetKey, ChangesetRow};
use crate::store::keys::Access;
// `as publishing`: the module's `publish` would clash with the tool's below.
use crate::store::publish::{self as publishing, PublishReport};

/// The catalog a changeset tool acts on, and this connection's open
/// changeset there (if any).
struct OwnChangeset {
    state: Arc<CatalogState>,
    changeset: Option<ChangesetRow>,
}

/// Resolves a changeset tool's catalog, checks the key's access to it and
/// finds this connection's (key and label) open changeset.
///
/// # Errors
/// A selection error; [`OkfError::Forbidden`] below `access`;
/// [`OkfError::Database`].
async fn own_changeset(ctx: &ToolContext, catalog: Option<&str>, access: Access) -> Result<Target, OkfError> {
    let state = ctx.one(catalog)?;
    let principal = ctx.principal();
    principal.require(access, state.name())?;
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &principal.key_id,
        label: ctx.label(),
    };
    let changeset = changesets::find_open(ctx.app().pool(), &key).await?;
    Ok(OwnChangeset { state, changeset })
}

// ---------------------------------------------------------------------------
// publish (Task 10)
// ---------------------------------------------------------------------------

/// `publish`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct PublishParams {
    /// What the change is, recorded on every revision it writes (like a commit message).
    pub message: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `publish`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Published {
    /// The catalog.
    pub catalog: String,
    /// This connection's changeset label.
    pub label: String,
    /// The changeset published, if this connection had one open.
    pub changeset: Option<i64>,
    /// What was written; `flatten` puts its fields beside these.
    #[serde(flatten)]
    pub report: PublishReport,
}

/// Publishes this connection's changeset in the target catalog.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the message and catalog.
///
/// # Returns
/// The report; `published: false` without an open, non-empty changeset.
///
/// # Errors
/// [`OkfError::Refused`] for a blank message; as [`target`]; as
/// [`publishing::publish`].
pub(crate) async fn publish(ctx: &ToolContext, params: PublishParams) -> Result<Published, OkfError> {
    let message = params.message.trim();
    if message.is_empty() {
        return Err(OkfError::Refused {
            reason: "publish needs a message saying what changed".to_string(),
        });
    }
    let OwnChangeset { state, changeset } = own_changeset(ctx, params.catalog.as_deref(), Access::Write).await?;
    let report = match &changeset {
        None => PublishReport::default(),
        Some(row) => publishing::publish(ctx.app(), &state, row, &ctx.principal().actor, message).await?,
    };
    Ok(Published {
        catalog: state.name().to_string(),
        label: ctx.label().to_string(),
        changeset: changeset.map(|row| row.id),
        report,
    })
}

#[tool_router(router = publish_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `publish` tool.
    #[tool(
        description = "Publish this connection's changeset in a catalog: its edits become the \
                       published documents, each with a new revision carrying `message`. Refused \
                       with Conflict when a document changed since this changeset read it \
                       (log.md and index.md bookkeeping is replayed onto the new text instead), \
                       or with LintFailed when a changed file has lint errors; the changeset \
                       then stays open. Lint errors elsewhere come back as warnings. Write access."
    )]
    async fn publish(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<PublishParams>,
    ) -> Result<Json<Published>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(publish(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`: `mod changesets;` and `+ Self::publish_router()`.

Run: `cargo test --lib -- mcp::changesets store::`
Expected: all pass (8 in `publish_tests`).

- [ ] **Step 7: The design**

Replace "Publish"'s steps 1 to 6 and the paragraph after them:

```markdown
1. Nothing changed (no open changeset, or no entries): return `published: false`.
2. Take the in-process mutex, begin the transaction, take the advisory lock, then the
   catalog row (`FOR KEY SHARE`) and the changeset row (`FOR UPDATE`), and check that
   the changeset is still open. If the catalog's `generation` (bumped by every publish
   and import) is newer than the in-memory snapshot's, rebuild the snapshot first, so
   the checks below see the current state.
3. Read the changed paths' rows from `documents` and check each entry's base against
   the current live revision:
   - an entry with a `replay` whose base is stale (the base revision is not the current
     one, or the base is NULL and the path now has a live document, such as a new
     folder's `index.md` that another session created first) has its operations
     re-applied to the current content, as git's `union` merge did for concurrent
     `log.md` lines. The operations are idempotent where that matters (creating a
     folder index that exists, listing a concept already listed);
   - any other entry whose base is stale is a conflict: roll back and return
     `Conflict { files }` with each file's base and current revision. The changeset
     stays open; the agent re-reads, re-applies and publishes again.
4. Build the overlay (the current snapshot plus the entries, replays applied) and run
   the checks on it:
   - the content rules (secret scan, frontmatter parses) on every changed text file,
     replayed or not (an entry may predate a rule);
   - lint: refuse with `LintFailed` if any **changed** file has errors; errors elsewhere
     and warnings in changed files are returned as warnings (as `okf.py publish` does);
   - links into deleted or moved paths: a document outside the changeset that still
     links to a path this changeset deletes or moves away from is a `link-into-deleted`
     error in `LintFailed`. This catches a link another session published after this
     session's `delete_concept` check ran.
   A failure rolls back and is returned like any other refusal.
5. Write each changed document (revision + 1, a tombstone's number continued) with its
   derived columns, and a `revisions` row (`create`, `update` or `delete`, the
   publishing key's actor, the changeset id, the message); a file whose result equals the
   published one gets no revision. Bump `generation` if anything was written, mark
   the changeset `published`, and commit.
6. Still holding the in-process mutex (the advisory lock ended with the transaction),
   rebuild and swap the catalog snapshot, and drop the changeset's cached overlay.
   Return the new revisions, the warnings and the replayed paths. A failed rebuild is
   logged; the publish stands and the poll loop rebuilds.
```

In "Errors", the `LintFailed` row: "Publish refused: lint errors in changed files or links into deleted paths (`errors`), with the rest as `warnings`; HTTP 422". The `Conflict` row: "...; `files` lists each path with its base and current revision; HTTP 409".

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-10-publish.md`:
- **Added:** `store::publish::{publish, PublishReport, PublishedRevision}`; the `publish` tool; `OkfError::{LintFailed, Conflict}`, `ConflictFile`; `CatalogState::publish_lock`; the `store::documents` writers; the tests.
- **Changed:** import on the shared writers; the Interfaces note; the design's "Publish" and "Errors".
- **Notes:**
  - the lock order;
  - why 422;
  - why the rules run on every changed file;
  - which lint findings refuse and which are advice;
  - unchanged results get no revision;
  - the unique-violation backstop.

Run every gate. Commit:

```bash
git add src/error.rs src/http/mod.rs src/catalog.rs src/store/ src/mcp/ \
    docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: publish with base-revision conflicts, bookkeeping replay, rules and lint (M5.10)" \
    -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: `discard`, `revert` and `pending_changes`

Three changeset tools (design "Write tools"):
- **`discard`** drops entries from this connection's changeset. Given no paths, it drops them all and closes the changeset.
- **`revert`** adds a document's content at an earlier revision, or its deletion, as an ordinary entry. It goes through `write::apply`, so every write rule applies and machinery stays refused.
- **`pending_changes`** shows the changeset as unified diffs against the published text.

> **Interfaces note (Task 11), 2026-11-03.**
> - **Deletion.** The `Workspace` trait has no delete method. Task 8's note settles how a deletion is represented: `apply`'s closure gets the concrete `&MemWorkspace`, and a touched path that is no longer a file after the op is stored as a deletion entry (`content: None`). `OpOutcome` does not change.
> - **`MemWorkspace::remove`.** This task adds `MemWorkspace::remove(&self, path) -> Result<bool, OkfError>`. Task 7's `with_changes` applies deletions to a new copy, but `revert`'s op must delete in place, on the copy `apply` hands it.
> - **Free edit.** A revert's outcome records no replay, so its entry is a free edit and conflicts on a stale base.
> - **Access.** `pending_changes` needs only `read` (it is a read tool in the design). A read key has no changeset, so it gets an empty list.
> - **Parameters.** It reuses `mcp::reports::CatalogParam` for its one argument.

**Files:**
- Modify: `Cargo.toml` (`similar`)
- Modify: `src/okf/workspace.rs` (`MemWorkspace::remove`)
- Modify: `src/store/history.rs` (`content_at`, `RevisionContent`)
- Modify: `src/mcp/changesets.rs` (the Task 11 section, `changes_router`, `changes_tests`)
- Modify: `src/mcp/mod.rs` (`+ Self::changes_router()`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` (the three tools' rows)
- Create: `changelog/<ts>-m5-task-11-discard-revert-pending.md`

- [ ] **Step 1: The dependency**

In `Cargo.toml`'s `[dependencies]`, in name order (keep Task 1's sqlx `json` change when merging):

```toml
# Unified diffs of a changeset against the published text (`pending_changes`).
similar = "2.7.0"
```

- [ ] **Step 2: `MemWorkspace::remove` (test first)**

Add to `src/okf/workspace.rs`'s tests:

```rust
    #[test]
    fn remove_deletes_a_file_and_says_whether_there_was_one() {
        let workspace = MemWorkspace::from_files([("a/b.md", "x"), ("c.md", "y")]);

        let removed = workspace.remove("/a/b.md").unwrap();
        let again = workspace.remove("a/b.md").unwrap();
        let outside = workspace.remove("../x.md");

        assert!(removed && !again);
        assert!(!workspace.is_file("a/b.md") && !workspace.is_dir("a"));
        assert!(workspace.is_file("c.md"));
        assert!(matches!(outside, Err(OkfError::InvalidPath { .. })));
    }
```

Add to `impl MemWorkspace`, after `stored`:

```rust
    /// Removes a file. Directories exist only as prefixes of file paths,
    /// so an emptied directory is gone too.
    ///
    /// Not part of [`Workspace`]: only the server's changesets delete, and
    /// they always work on a `MemWorkspace` (the write path's copy of an
    /// overlay).
    ///
    /// # Arguments
    /// * `path` - Catalog-relative path (a leading `/` means the root).
    ///
    /// # Returns
    /// `true` if a file was there (now removed), `false` if none was.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if the path escapes the catalog or is the root.
    pub fn remove(&self, path: &str) -> Result<bool, OkfError> {
        let rel = checked_file(path)?;
        // `remove` gives back the old value, if any: `is_some` says whether
        // there was a file.
        Ok(self.files.write().expect("MemWorkspace lock poisoned").remove(&rel).is_some())
    }
```

Run: `cargo test --lib -- okf::workspace::tests::remove`
Expected: 1 passed.

- [ ] **Step 3: `history::content_at` (test first)**

Add to `src/store/history.rs`'s tests:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn content_at_reads_one_revision_or_its_deletion() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        for (entries, replace) in [(&[("a.md", "# One\n"), ("b.md", "# B\n")][..], false), (&[("b.md", "# B\n")][..], true)] {
            let options = ImportOptions { replace, actor: "tester", now: Utc::now() };
            import(&db.pool, "pg", files(entries), &options).await.unwrap();
        }

        let first = content_at(&db.pool, catalog.id, "a.md", 1).await.unwrap().unwrap();
        let deleted = content_at(&db.pool, catalog.id, "a.md", 2).await.unwrap().unwrap();
        let missing = content_at(&db.pool, catalog.id, "a.md", 3).await.unwrap();

        assert_eq!((first.op.as_str(), first.content.as_deref()), ("import", Some(&b"# One\n"[..])));
        assert_eq!((deleted.op.as_str(), deleted.content), ("delete", None));
        assert!(missing.is_none());
    }
```

Add above the tests:

```rust
/// One revision's content, for `revert`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RevisionContent {
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// The bytes; `None` for a `delete`.
    pub content: Option<Vec<u8>>,
    /// The execute bit.
    pub executable: bool,
}

/// One path's content at one revision.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `path` - catalog-relative path, as stored.
/// * `revision` - the revision number.
///
/// # Returns
/// The revision's content, or `None` if the path has no such revision.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn content_at(
    pool: &PgPool,
    catalog_id: i64,
    path: &str,
    revision: i32,
) -> Result<Option<RevisionContent>, OkfError> {
    // One primary-key lookup.
    let row = sqlx::query_as::<_, RevisionContent>(
        "SELECT op, content, executable FROM revisions \
         WHERE catalog_id = $1 AND path = $2 AND revision = $3",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(revision)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}
```

Run: `cargo test --lib -- store::history`
Expected: 2 passed.

- [ ] **Step 4: Write the failing tool tests**

Add a test module at the end of `src/mcp/changesets.rs`:

```rust
#[cfg(test)]
mod changes_tests {
    use super::*;
    use chrono::Utc;

    use crate::config::Config;
    use crate::mcp::reports::CatalogParam;
    use crate::mcp::testing::{app_with, context, files, principal, writer, TestApp};
    use crate::mcp::writes::{create_concept, replace_body, CreateConceptParams, ReplaceBodyParams};
    use crate::store::changesets::ChangesetState;
    use crate::store::import::{import, ImportOptions};

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
    const NOTE_B: &str = "---\ntype: Note\ntitle: B\ndescription: B note.\n---\n# B\n";

    async fn pg() -> TestApp {
        let files: &[(&str, &str)] = &[("a.md", NOTE), ("b.md", NOTE_B), ("AGENTS.md", "# Agents\n")];
        app_with(&[("pg", files)], Config::default()).await
    }

    /// Replaces the published catalog with `entries` (an import standing in
    /// for other sessions' publishes), and lets the server notice.
    async fn republish(test: &TestApp, entries: &[(&str, &str)]) {
        let options = ImportOptions { replace: true, actor: "tester", now: Utc::now() };
        import(test.app.pool(), "pg", files(entries), &options).await.unwrap();
        test.app.catalog("pg").unwrap().refresh(test.app.pool()).await.unwrap();
    }

    async fn body(ctx: &ToolContext, path: &str, text: &str) -> crate::write::WriteReport {
        let params = ReplaceBodyParams { path: path.to_string(), body: text.to_string(), catalog: None };
        replace_body(ctx, params).await.unwrap()
    }

    fn revert_to(path: &str, revision: i32) -> RevertParams {
        RevertParams { path: path.to_string(), revision, catalog: None }
    }

    async fn pending(ctx: &ToolContext) -> PendingChanges {
        pending_changes(ctx, CatalogParam { catalog: None }).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn discard_drops_named_paths_or_closes_the_whole_changeset() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let written = body(&ctx, "/a.md", "One.\n").await;
        body(&ctx, "/b.md", "Two.\n").await;

        let some = discard(&ctx, DiscardParams { paths: Some(vec!["/a.md".to_string()]), catalog: None }).await.unwrap();
        let left = pending(&ctx).await;
        let all = discard(&ctx, DiscardParams { paths: None, catalog: None }).await.unwrap();
        let after = pending(&ctx).await;
        let again = body(&ctx, "/a.md", "Three.\n").await;

        assert_eq!((some.changeset, some.removed, some.closed), (Some(written.changeset), 1, false));
        let paths: Vec<&str> = left.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["/b.md"]);
        assert_eq!((all.removed, all.closed), (1, true));
        let row = changesets::by_id(test.app.pool(), written.changeset).await.unwrap().unwrap();
        assert!(matches!(row.state, ChangesetState::Discarded));
        assert_eq!((after.changeset, after.files.len()), (None, 0));
        assert_ne!(again.changeset, written.changeset, "a write after a discard opens a new changeset");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revert_writes_an_old_revision_as_a_free_edit() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        republish(&test, &[("a.md", "# Two\n"), ("b.md", NOTE_B), ("AGENTS.md", "# Agents\n")]).await;

        let reverted = revert(&ctx, revert_to("/a.md", 1)).await.unwrap();

        assert_eq!(reverted.touched, ["/a.md"]);
        let entries = changesets::entries(test.app.pool(), reverted.changeset).await.unwrap();
        assert_eq!(entries[0].content.as_deref(), Some(NOTE.as_bytes()));
        // Based on what is published now (revision 2), with no replay.
        assert_eq!((entries[0].base_revision, entries[0].replay.as_ref()), (Some(2), None));
        let shown = pending(&ctx).await;
        assert_eq!(shown.files[0].change, "update");
        assert!(shown.files[0].diff.starts_with("--- a/a.md\n+++ b/a.md\n"), "{}", shown.files[0].diff);
        assert!(shown.files[0].diff.contains("-# Two\n+---\n"), "{}", shown.files[0].diff);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revert_to_a_delete_revision_deletes_and_machinery_stays_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // b.md: 1 imported, 2 deleted, 3 imported again.
        republish(&test, &[("a.md", NOTE), ("AGENTS.md", "# Agents\n")]).await;
        republish(&test, &[("a.md", NOTE), ("b.md", NOTE_B), ("AGENTS.md", "# Agents\n")]).await;

        let deleted = revert(&ctx, revert_to("/b.md", 2)).await.unwrap();
        let machinery = revert(&ctx, revert_to("/AGENTS.md", 1)).await.unwrap_err();
        let no_such = revert(&ctx, revert_to("/a.md", 9)).await.unwrap_err();
        let reader = context(&test, principal(&[], Some("pg")), None);
        let read_only = revert(&reader, revert_to("/a.md", 1)).await.unwrap_err();

        let entries = changesets::entries(test.app.pool(), deleted.changeset).await.unwrap();
        assert_eq!((entries[0].path.as_str(), entries[0].content.as_ref()), ("b.md", None));
        assert_eq!(entries[0].base_revision, Some(3));
        let shown = pending(&ctx).await;
        assert_eq!(shown.files[0].change, "delete");
        assert!(shown.files[0].diff.starts_with("--- a/b.md\n+++ /dev/null\n"), "{}", shown.files[0].diff);
        assert_eq!(machinery.kind(), "InvalidPath");
        assert_eq!(no_such.kind(), "NotFound");
        assert_eq!(read_only.kind(), "Forbidden");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_changes_diffs_a_new_file_against_dev_null_and_needs_only_read() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let frontmatter = serde_json::json!({"type": "Note", "title": "New", "description": "New."});
        let params = CreateConceptParams {
            path: "/new.md".to_string(),
            frontmatter: frontmatter.as_object().unwrap().clone(),
            body: "# New\n".to_string(),
            index_summary: None,
            index_section: None,
            by: None,
            catalog: None,
        };
        create_concept(&ctx, params).await.unwrap();

        let shown = pending(&ctx).await;
        let reader = pending(&context(&test, principal(&[], Some("pg")), None)).await;

        assert_eq!((shown.catalog.as_str(), shown.label.as_str()), ("pg", "default"));
        let file = &shown.files[0];
        assert_eq!((file.path.as_str(), file.change, file.base_revision), ("/new.md", "create", None));
        assert!(file.diff.starts_with("--- /dev/null\n+++ b/new.md\n@@ -0,0 +1,"), "{}", file.diff);
        assert!(file.diff.contains("+# New\n"));
        assert!(reader.files.is_empty() && reader.changeset.is_none());
    }

    #[test]
    fn a_file_that_is_not_text_is_reported_as_binary() {
        let diff = unified_diff("logo.png", Some(b"\x89PNG\xff"), Some(b"\x89PNG\xfe"));

        assert_eq!(diff, "Binary files a/logo.png and b/logo.png differ\n");
    }
}
```

Run: `cargo test --lib -- mcp::changesets::changes_tests`
Expected: compile errors (the tools are not defined).

- [ ] **Step 5: Implement the tools**

Add the Task 11 section to `src/mcp/changesets.rs`, after the publish section's `#[tool_router]` block. Extend the file's imports with:

```rust
use std::collections::BTreeMap;

use rayon::prelude::*;
use similar::TextDiff;

use crate::catalog::Loaded;
use crate::mcp::blocking;
use crate::mcp::reports::CatalogParam;
use crate::okf::ops::OpOutcome;
use crate::okf::paths::{escapes_root, normpath};
use crate::okf::rules;
use crate::store::changesets::{ChangesetState, EntryRow};
use crate::store::history;
use crate::write::{apply, WriteReport};
```

and add:

```rust
// ---------------------------------------------------------------------------
// discard, revert and pending_changes (Task 11)
// ---------------------------------------------------------------------------

/// `discard`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DiscardParams {
    /// The paths to drop, e.g. `["/infra/ntp.md"]` (default: every entry, which also closes the changeset).
    pub paths: Option<Vec<String>>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `discard`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Discarded {
    /// The catalog.
    pub catalog: String,
    /// This connection's changeset label.
    pub label: String,
    /// The changeset, if this connection had one open.
    pub changeset: Option<i64>,
    /// How many entries were dropped.
    pub removed: u64,
    /// Whether the changeset was closed (`discarded`): no `paths` given.
    pub closed: bool,
}

/// `revert`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct RevertParams {
    /// The document as stored, with its extension, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The revision to go back to, as `history` lists it; a `delete` revision means deleting the document.
    pub revision: i32,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// One file of `pending_changes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PendingFile {
    /// `/`-prefixed path.
    pub path: String,
    /// `create`, `update` or `delete`, against the published catalog now.
    pub change: &'static str,
    /// The published revision the entry was made on (null: the path had no live document).
    pub base_revision: Option<i32>,
    /// Whether publish can replay the entry onto newer text (bookkeeping only).
    pub replayable: bool,
    /// A unified diff against the published text (`/dev/null` for a new or deleted file).
    pub diff: String,
}

/// `pending_changes`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PendingChanges {
    /// The catalog.
    pub catalog: String,
    /// This connection's changeset label.
    pub label: String,
    /// The open changeset, if any.
    pub changeset: Option<i64>,
    /// Its entries, in path order.
    pub files: Vec<PendingFile>,
}

/// A path argument as stored: no leading `/`, normalized, inside the catalog.
///
/// # Errors
/// [`OkfError::InvalidPath`] for a path outside the catalog or the root.
fn stored_path(reference: &str) -> Result<String, OkfError> {
    let path = normpath(reference.trim_start_matches('/'));
    if escapes_root(&path) || path == "." {
        return Err(OkfError::InvalidPath {
            path: reference.to_string(),
            reason: "outside the catalog".to_string(),
        });
    }
    Ok(path)
}

/// Drops entries from this connection's changeset; without `paths`, all
/// of them, and the changeset is closed.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the paths and catalog.
///
/// # Returns
/// How many entries went, and whether the changeset was closed.
///
/// # Errors
/// As [`own_changeset`]; [`OkfError::InvalidPath`] for a path outside the catalog;
/// [`OkfError::Refused`] if a publish closed the changeset meanwhile;
/// [`OkfError::Database`].
pub(crate) async fn discard(ctx: &ToolContext, params: DiscardParams) -> Result<Discarded, OkfError> {
    let OwnChangeset { state, changeset } = own_changeset(ctx, params.catalog.as_deref(), Access::Write).await?;
    // `transpose` turns `Option<Result<..>>` into `Result<Option<..>>`, so
    // `?` can raise a bad path while "no paths" stays `None`.
    let paths = params
        .paths
        .map(|paths| paths.iter().map(|path| stored_path(path)).collect::<Result<Vec<_>, _>>())
        .transpose()?;
    let label = ctx.label().to_string();
    let Some(changeset) = changeset else {
        return Ok(Discarded { catalog: state.name().to_string(), label, changeset: None, removed: 0, closed: false });
    };
    let app = ctx.app();
    let now = app.clock().now();
    let mut tx = app.pool().begin().await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    let still_open = changesets::lock(&mut tx, changeset.id)
        .await?
        .is_some_and(|row| matches!(row.state, ChangesetState::Open));
    if !still_open {
        return Err(OkfError::Refused {
            reason: format!("changeset {} is no longer open", changeset.id),
        });
    }
    let removed = changesets::remove_entries(&mut tx, changeset.id, paths.as_deref(), now).await?;
    let closed = paths.is_none();
    if closed {
        changesets::set_state(&mut tx, changeset.id, ChangesetState::Discarded, now).await?;
    }
    tx.commit().await?;
    app.overlays().forget(changeset.id);
    Ok(Discarded { catalog: state.name().to_string(), label, changeset: Some(changeset.id), removed, closed })
}

/// Adds a document's content at `revision` (or its deletion, for a
/// `delete` revision) to this connection's changeset, as a free edit.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the path, revision and catalog.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// [`OkfError::InvalidPath`] for machinery or a path outside the catalog
/// (before any history is read); a selection error;
/// [`OkfError::Forbidden`] below `write`; [`OkfError::NotFound`] for a
/// revision the path never had; [`OkfError::Refused`] for content that is
/// not text, or a deletion of a path already deleted; as [`apply`].
pub(crate) async fn revert(ctx: &ToolContext, params: RevertParams) -> Result<WriteReport, OkfError> {
    let RevertParams { path, revision, catalog } = params;
    // The write rules refuse machinery here, before the history is read;
    // `apply` checks the touched path again.
    let path = rules::check_path(path.trim_start_matches('/'))?;
    let state = ctx.one(catalog.as_deref())?;
    ctx.principal().require(Access::Write, state.name())?;
    let stored = history::content_at(ctx.app().pool(), state.id(), &path, revision)
        .await?
        .ok_or_else(|| OkfError::NotFound {
            path: format!("/{path} (revision {revision})"),
        })?;
    // `transpose` again: `Option<Result<String, _>>` to `Result<Option<..>>`.
    let content = stored
        .content
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| OkfError::Refused {
            reason: format!("/{path} revision {revision} is not text; only text documents can be reverted"),
        })?;
    apply(ctx, Some(state.name()), None, move |ws, _| {
        match &content {
            Some(text) => ws.write_atomic(&path, text)?,
            None => {
                if !ws.remove(&path)? {
                    return Err(OkfError::Refused {
                        reason: format!("/{path} is already deleted in this changeset's view"),
                    });
                }
            }
        }
        let message = format!("{path}: reverted to revision {revision}");
        // No replay: a revert is a free edit, which conflicts on a stale base.
        // `path` moves into the outcome (the closure owns it; no copy).
        Ok(OpOutcome {
            touched: vec![path],
            replay: BTreeMap::new(),
            message,
            notes: Vec::new(),
        })
    })
    .await
}

/// This connection's changeset in a catalog, as diffs against the published
/// text.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog.
///
/// # Returns
/// The changed paths and their diffs; empty without an open changeset.
///
/// # Errors
/// As [`own_changeset`] (any key that reaches the catalog may ask).
pub(crate) async fn pending_changes(ctx: &ToolContext, params: CatalogParam) -> Result<PendingChanges, OkfError> {
    let OwnChangeset { state, changeset } = own_changeset(ctx, params.catalog.as_deref(), Access::Read).await?;
    let files = match &changeset {
        None => Vec::new(),
        Some(row) => {
            let entries = changesets::entries(ctx.app().pool(), row.id).await?;
            let loaded = state.current();
            blocking(move || pending_files(&loaded, &entries)).await
        }
    };
    Ok(PendingChanges {
        catalog: state.name().to_string(),
        label: ctx.label().to_string(),
        changeset: changeset.map(|row| row.id),
        files,
    })
}

/// Each entry against the published file (on a blocking thread).
fn pending_files(loaded: &Loaded, entries: &[EntryRow]) -> Vec<PendingFile> {
    // The diffs are independent: rayon's `par_iter` spreads them over the
    // CPU pool, and `collect` keeps the entries' (path) order.
    entries
        .par_iter()
        .map(|entry| {
            let published = loaded.workspace().stored(&entry.path);
            let change = match (&published, &entry.content) {
                (_, None) => "delete",
                (None, Some(_)) => "create",
                (Some(_), Some(_)) => "update",
            };
            let old = published.as_ref().map(|file| file.content.as_slice());
            PendingFile {
                path: format!("/{}", entry.path),
                change,
                base_revision: entry.base_revision,
                replayable: entry.replay.is_some(),
                diff: unified_diff(&entry.path, old, entry.content.as_deref()),
            }
        })
        .collect()
}

/// A unified diff (3 lines of context) between the published and the
/// pending file. A missing side is `/dev/null`, as `git diff` writes it.
///
/// # Arguments
/// * `path` - catalog-relative path.
/// * `published` - the published bytes, if the file is published.
/// * `pending` - the pending bytes, or `None` for a deletion.
///
/// # Returns
/// The diff text; a one-line "Binary files ... differ" when either side is
/// not UTF-8.
fn unified_diff(path: &str, published: Option<&[u8]>, pending: Option<&[u8]>) -> String {
    // A missing side diffs as empty text.
    let as_text = |bytes: Option<&[u8]>| bytes.map_or(Ok(""), std::str::from_utf8);
    let (Ok(old), Ok(new)) = (as_text(published), as_text(pending)) else {
        return format!("Binary files a/{path} and b/{path} differ\n");
    };
    let old_header = published.map_or_else(|| "/dev/null".to_string(), |_| format!("a/{path}"));
    let new_header = pending.map_or_else(|| "/dev/null".to_string(), |_| format!("b/{path}"));
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&old_header, &new_header)
        .to_string()
}

#[tool_router(router = changes_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `discard` tool.
    #[tool(
        description = "Drop entries from this connection's changeset in a catalog: the named \
                       `paths`, or every entry (which also closes the changeset; the next write \
                       opens a new one). Published documents are not touched. Write access."
    )]
    async fn discard(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<DiscardParams>,
    ) -> Result<Json<Discarded>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(discard(&ctx, params).await)
    }

    /// The `revert` tool.
    #[tool(
        description = "Add a document's content at an earlier revision (from `history`), or its \
                       deletion for a `delete` revision, to this connection's changeset, for \
                       review and publish like any other edit. Every write rule applies, so \
                       machinery cannot be reverted. Write access."
    )]
    async fn revert(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<RevertParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(revert(&ctx, params).await)
    }

    /// The `pending_changes` tool.
    #[tool(
        description = "This connection's unpublished changeset in a catalog: each changed path \
                       (create, update or delete), the revision it was based on, whether publish \
                       can replay it, and a unified diff against the published text."
    )]
    async fn pending_changes(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CatalogParam>,
    ) -> Result<Json<PendingChanges>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(pending_changes(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, add `+ Self::changes_router()`. If `CatalogParam` is not reachable (`mod reports;` is private to `mcp`, and `changesets` is its sibling, so `crate::mcp::reports::CatalogParam` is), nothing more is needed.

Run: `cargo test --lib -- mcp::changesets okf::workspace store::history`
Expected: all pass (5 in `changes_tests`).

- [ ] **Step 6: The design**

In "Read tools", the `pending_changes` row: "This connection's changeset in the catalog: each changed path (`create`, `update`, `delete`), its base revision, whether it is replayable, and a unified diff against the published text (`/dev/null` for a new or deleted file; non-text files as `Binary files ... differ`)". In "Write tools", `discard`: "Drops this connection's changeset entries: the named paths, or all of them, which also marks the changeset `discarded`". `revert`: add "A revert is a free edit (no replay); a revision whose content is not UTF-8 is refused, and a revived file is not executable (scripts are machinery, which revert refuses anyway)."

- [ ] **Step 7: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-11-discard-revert-pending.md`:
- **Added:** the three tools; `changes_router`; `MemWorkspace::remove`; `history::{content_at, RevisionContent}`; `similar` 2.7.0; the tests.
- **Changed:** the design's rows; the Interfaces note.
- **Notes:**
  - a deletion is a touched path that is no longer a file;
  - revert checks the path before it reads the history;
  - `pending_changes` needs only `read`;
  - diffs run in parallel on the blocking pool.

Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/okf/workspace.rs src/store/history.rs src/mcp/ \
    docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: discard, revert and pending_changes (M5.11)" \
    -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: `stranded_sessions`, `adopt_changeset` and the prune

> Interfaces note: (1) The code lives in `src/mcp/changesets.rs` in its own `#[tool_router(router = lifecycle_router, vis = "pub(crate)")]` block, its own test module (`lifecycle_tests`, not `tests`), and function-local `use` lines for the store, so it merges with Tasks 10 and 11 in either order: whichever of Tasks 10, 11 and 12 lands first creates the file, adds `mod changesets;` to `src/mcp/mod.rs`, and the others add their block and router line. (2) `stranded_sessions` takes an optional `catalog` to narrow the list, and leaves out the calling connection's own changeset (same key and label), which is not stranded for it. (3) `adopt_changeset` answers an unknown id, a closed changeset, or one in a catalog the key does not reach with the same `Refused` ("no open changeset N in a catalog this key reaches"), so a key cannot probe other catalogs; a reachable changeset of another key without `admin` is `Forbidden`. It does not call `App::overlays().forget` (Task 7 is not a dependency): adoption bumps `version`, which already invalidates any cached overlay. (4) The changeset's `actor` stays the opener's; Task 10's `publish` should record the publishing key's actor on revisions. (5) `App::poll` prunes with the clock's `now` and a 24-hour cutoff on `updated_at` (the changeset's last change), and logs a failure instead of returning it, like the key reload.

Design "Session lifecycle" as Task 1 rewrote it. `stranded_sessions` lists open changesets that hold edits and that no one has changed within `session_idle_timeout`: the key's own, or for an `admin` key every one in the catalogs it reaches. `adopt_changeset(id)` moves one to the calling connection's key and label, after which every read and write of this connection sees and extends it: `write` access adopts the key's own, `admin` another key's, and a connection that already has an open changeset in that catalog is refused (publish or discard it first, or connect with another label). The poll loop discards empty changesets whose last change is more than 24 hours old.

**Files:**
- Modify (or create, if Tasks 10 and 11 have not landed): `src/mcp/changesets.rs`
- Modify: `src/mcp/mod.rs` (`mod changesets;` if not there; `+ Self::lifecycle_router()`)
- Modify: `src/app.rs` (the prune in `poll`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Read tools", "Write tools")
- Create: `changelog/<ts>-m5-task-12-stranded-adopt.md`

- [ ] **Step 1: Write the failing tests**

If `src/mcp/changesets.rs` does not exist yet, create it with this module doc, add `mod changesets;` to `src/mcp/mod.rs` (beside `mod catalogs;`), and add `+ Self::lifecycle_router()` to `OkfServer::tools`:

```rust
//! The changeset tools (design "Write tools"): `publish` (Task 10);
//! `discard`, `revert` and `pending_changes` (Task 11); `stranded_sessions`
//! and `adopt_changeset` (Task 12). Each task's tools are one
//! `#[tool_router]` block with its own router, joined in `OkfServer::tools`.
```

If it exists, add only the router line to `OkfServer::tools` (its `mod` line is there). Then append this test module to the file:

```rust
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use chrono::{TimeDelta, Utc};

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, pending_entry, principal_as, store_key, PendingEdit, TestApp,
    };
    use crate::store::changesets::{self, ChangesetKey};
    use crate::store::keys::Access;

    /// `pg` and `work`, and the keys `alice`, `bob` and `root`.
    async fn test_app() -> TestApp {
        let test = app_with(
            &[("pg", &[("a.md", "# A\n")]), ("work", &[])],
            Config::default(),
        )
        .await;
        for key in ["alice", "bob", "root"] {
            store_key(&test, key).await;
        }
        test
    }

    /// A connection of `key` with `access` on `label`, reaching `reach`.
    fn connection(
        test: &TestApp,
        key: &str,
        access: Access,
        reach: &[&str],
        label: &str,
    ) -> ToolContext {
        context(test, principal_as(key, access, reach, Some("pg")), None).with_label(label)
    }

    /// An edit of `a.md` in `pg`, `hours_ago` hours ago, by `ctx`.
    async fn edit_hours_ago(test: &TestApp, ctx: &ToolContext, hours_ago: i64) -> i64 {
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some("# Pending\n"),
            at: Utc::now() - TimeDelta::hours(hours_ago),
        };
        pending_entry(test, ctx, &edit).await.id
    }

    fn ids(stranded: &Stranded) -> Vec<i64> {
        stranded.changesets.iter().map(|changeset| changeset.id).collect()
    }

    fn all() -> StrandedParams {
        StrandedParams { catalog: None }
    }

    #[test]
    fn the_idle_cutoff_is_now_less_the_timeout_and_never_overflows() {
        let now = Utc::now();

        let cutoff = idle_cutoff(now, std::time::Duration::from_secs(7200));
        let huge = idle_cutoff(now, std::time::Duration::MAX);

        assert_eq!(cutoff, now - TimeDelta::hours(2));
        assert_eq!(huge, chrono::DateTime::<Utc>::MIN_UTC);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stranded_lists_the_key_s_idle_changesets_and_all_of_them_for_an_admin() {
        let test = test_app().await;
        let alice_old = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "old"),
            3,
        )
        .await;
        edit_hours_ago(&test, &connection(&test, "alice", Access::Write, &[], "fresh"), 0).await;
        let bob_old = edit_hours_ago(
            &test,
            &connection(&test, "bob", Access::Write, &[], "old"),
            3,
        )
        .await;

        let alice = stranded_sessions(&connection(&test, "alice", Access::Write, &[], "x"), all())
            .await
            .unwrap();
        let alice_on_old =
            stranded_sessions(&connection(&test, "alice", Access::Write, &[], "old"), all())
                .await
                .unwrap();
        let admin = stranded_sessions(&connection(&test, "root", Access::Admin, &[], "x"), all())
            .await
            .unwrap();
        let admin_of_work = stranded_sessions(
            &connection(&test, "root", Access::Admin, &["work"], "x"),
            all(),
        )
        .await
        .unwrap();
        let narrowed = stranded_sessions(
            &connection(&test, "root", Access::Admin, &[], "x"),
            StrandedParams {
                catalog: Some("work".to_string()),
            },
        )
        .await
        .unwrap();

        assert_eq!(ids(&alice), [alice_old]);
        let listed = &alice.changesets[0];
        assert_eq!(
            (listed.catalog.as_str(), listed.label.as_str(), listed.key.as_deref()),
            ("pg", "old", Some("alice"))
        );
        assert_eq!(listed.paths, ["a.md"]);
        assert!(ids(&alice_on_old).is_empty(), "this connection's own is not stranded");
        let mut both = ids(&admin);
        both.sort_unstable();
        let mut expected = vec![alice_old, bob_old];
        expected.sort_unstable();
        assert_eq!(both, expected);
        assert!(ids(&admin_of_work).is_empty());
        assert!(ids(&narrowed).is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_moves_a_changeset_to_this_connection() {
        let test = test_app().await;
        let stranded_id = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "old"),
            3,
        )
        .await;
        let here = connection(&test, "alice", Access::Write, &[], "default");

        let adopted = adopt_changeset(&here, AdoptParams { id: stranded_id })
            .await
            .unwrap();
        let after = stranded_sessions(&here, all()).await.unwrap();

        assert_eq!(adopted.id, stranded_id);
        assert_eq!(
            (adopted.catalog.as_str(), adopted.label.as_str(), adopted.previous_label.as_str()),
            ("pg", "default", "old")
        );
        assert_eq!(adopted.paths, ["a.md"]);
        assert!(after.changesets.is_empty(), "adopted means changed just now");
        let pg = test.app.catalog("pg").unwrap();
        let key = ChangesetKey {
            catalog_id: pg.id(),
            key_id: "alice",
            label: "default",
        };
        let open = changesets::find_open(test.app.pool(), &key).await.unwrap();
        assert_eq!(open.map(|row| row.id), Some(stranded_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_key_s_changeset_needs_admin_to_adopt() {
        let test = test_app().await;
        let bobs = edit_hours_ago(&test, &connection(&test, "bob", Access::Write, &[], "old"), 3)
            .await;

        let by_writer = adopt_changeset(
            &connection(&test, "alice", Access::Write, &[], "default"),
            AdoptParams { id: bobs },
        )
        .await;
        let by_admin = adopt_changeset(
            &connection(&test, "root", Access::Admin, &[], "default"),
            AdoptParams { id: bobs },
        )
        .await;

        assert!(matches!(by_writer, Err(OkfError::Forbidden { .. })));
        let adopted = by_admin.unwrap();
        assert_eq!(adopted.previous_key.as_deref(), Some("bob"));
        let row = changesets::by_id(test.app.pool(), bobs).await.unwrap().unwrap();
        assert_eq!(row.key_id.as_deref(), Some("root"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_is_refused_for_a_busy_connection_or_an_unknown_id() {
        let test = test_app().await;
        let stranded_id = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "old"),
            3,
        )
        .await;
        let here = connection(&test, "alice", Access::Write, &[], "default");
        edit_hours_ago(&test, &here, 0).await;

        let busy = adopt_changeset(&here, AdoptParams { id: stranded_id }).await;
        let unknown = adopt_changeset(&here, AdoptParams { id: i64::MAX }).await;
        let unreachable = adopt_changeset(
            &connection(&test, "alice", Access::Write, &["work"], "default"),
            AdoptParams { id: stranded_id },
        )
        .await;

        assert!(
            matches!(&busy, Err(OkfError::Refused { reason }) if reason.contains("already has")),
            "{busy:?}"
        );
        for refused in [unknown, unreachable] {
            assert!(
                matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("no open changeset")),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn both_tools_are_listed_with_object_output_schemas() {
        let tools = OkfServer::tools().list_all();

        for name in ["stranded_sessions", "adopt_changeset"] {
            let listed = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            let schema = listed.output_schema.as_ref().expect("an output schema");
            assert_eq!(schema["type"], "object", "{name}");
        }
    }
}
```

In `src/app.rs`'s test module (imports: `chrono::{DateTime, TimeDelta}`, `crate::store::changesets::{self, ChangesetKey, ChangesetState, EntryRow}`):

```rust
    /// Opens `label`'s changeset for key `writer` at `at`, with an entry for
    /// `path` if given.
    async fn changeset_at(
        pool: &PgPool,
        catalog_id: i64,
        label: &str,
        at: DateTime<Utc>,
        path: Option<&str>,
    ) -> i64 {
        let key = ChangesetKey {
            catalog_id,
            key_id: "writer",
            label,
        };
        let mut tx = pool.begin().await.unwrap();
        changesets::lock_catalog(&mut tx, catalog_id).await.unwrap();
        let changeset = changesets::get_or_create(&mut tx, &key, "writer", at)
            .await
            .unwrap();
        if let Some(path) = path {
            let entry = EntryRow {
                path: path.to_string(),
                base_revision: None,
                content: Some(b"x\n".to_vec()),
                executable: false,
                replay: None,
            };
            changesets::upsert_entry(&mut tx, changeset.id, &entry, at)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        changeset.id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_discards_empty_changesets_a_day_old_and_keeps_the_rest() {
        let db = test_db().await;
        let pg = catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        new_key(&db.pool, "writer").await;
        let app = app(&db.pool).await;
        let day_old = Utc::now() - TimeDelta::hours(25);
        let empty_old = changeset_at(&db.pool, pg.id, "a", day_old, None).await;
        let empty_new = changeset_at(&db.pool, pg.id, "b", Utc::now(), None).await;
        let full_old = changeset_at(&db.pool, pg.id, "c", day_old, Some("a.md")).await;

        app.poll().await.unwrap();

        let state = |id| {
            let pool = db.pool.clone();
            async move { changesets::by_id(&pool, id).await.unwrap().unwrap().state }
        };
        assert_eq!(state(empty_old).await, ChangesetState::Discarded);
        assert_eq!(state(empty_new).await, ChangesetState::Open);
        assert_eq!(state(full_old).await, ChangesetState::Open);
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test --lib -- mcp::changesets::lifecycle_tests app::`
Expected: compile errors (`stranded_sessions`, `adopt_changeset`, `idle_cutoff`, `StrandedParams` ... not found); once it compiles, the poll test fails until Step 4.

- [ ] **Step 3: The tools**

Add to `src/mcp/changesets.rs`, above `lifecycle_tests`. If the file already has `use` lines from Task 10 or 11, merge these into them and drop duplicates (a name imported twice does not compile):

```rust
use std::time::Duration;

use axum::http::request::Parts;
use chrono::{DateTime, TimeDelta, Utc};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::store::keys::Access;
```

```rust
/// `stranded_sessions`' arguments.
// `Deserialize` reads them from the call's JSON; `JsonSchema` gives the tool
// its input schema, with the doc comments as the fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct StrandedParams {
    /// Only this catalog (default: every catalog the key reaches).
    pub catalog: Option<String>,
}

/// One stranded changeset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StrandedChangeset {
    /// Its id: what `adopt_changeset` takes.
    pub id: i64,
    /// Its catalog.
    pub catalog: String,
    /// The label of the connection that made it.
    pub label: String,
    /// The key it belongs to; null once that key is deleted.
    pub key: Option<String>,
    /// The actor of the key that opened it.
    pub actor: String,
    /// When it was opened.
    pub created: String,
    /// Its last change.
    pub last_change: String,
    /// Its changed paths, sorted.
    pub paths: Vec<String>,
}

/// `stranded_sessions`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Stranded {
    /// By catalog, oldest change first.
    pub changesets: Vec<StrandedChangeset>,
}

/// `adopt_changeset`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct AdoptParams {
    /// The changeset's id, from `stranded_sessions`.
    pub id: i64,
}

/// `adopt_changeset`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Adopted {
    /// The changeset's id.
    pub id: i64,
    /// Its catalog.
    pub catalog: String,
    /// Its label now: this connection's.
    pub label: String,
    /// The key it belonged to.
    pub previous_key: Option<String>,
    /// The label it had.
    pub previous_label: String,
    /// Its changed paths, sorted.
    pub paths: Vec<String>,
}

/// The time before which an open changeset's last change makes it stranded.
///
/// # Arguments
/// * `now` - the current time (server clock).
/// * `idle` - `session_idle_timeout`.
///
/// # Returns
/// `now - idle`, or the earliest representable time if that would overflow
/// (an absurdly long timeout: nothing is stranded).
fn idle_cutoff(now: DateTime<Utc>, idle: Duration) -> DateTime<Utc> {
    // `TimeDelta::from_std` fails for a duration beyond chrono's range, and
    // `checked_sub_signed` for a result before its earliest date; both `ok()`
    // and `and_then` turn a failure into `None`.
    TimeDelta::from_std(idle)
        .ok()
        .and_then(|idle| now.checked_sub_signed(idle))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Lists stranded changesets (design "Session lifecycle").
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - an optional catalog to narrow the list to.
///
/// # Returns
/// Open, non-empty changesets not changed within `session_idle_timeout`,
/// in catalogs the key reaches: the key's own (except this connection's
/// own, same label), and with `admin` every key's.
///
/// # Errors
/// A selection error for `catalog`; [`OkfError::Database`].
pub(crate) async fn stranded_sessions(
    ctx: &ToolContext,
    params: StrandedParams,
) -> Result<Stranded, OkfError> {
    // Local: Tasks 10 and 11 import the store in this file in their own way.
    use crate::store::changesets;

    // `transpose` turns `Option<Result<..>>` into `Result<Option<..>>`, so
    // `?` can fail on an unreachable catalog named in the argument.
    let only = params
        .catalog
        .as_deref()
        .map(|name| ctx.one(Some(name)))
        .transpose()?
        .map(|state| state.id());
    let now = ctx.app().clock().now();
    let idle_since = idle_cutoff(now, ctx.app().config().session_idle_timeout);
    let principal = ctx.principal();
    let admin = principal.access >= Access::Admin;
    let rows = changesets::stranded(ctx.app().pool(), idle_since).await?;
    let changesets = rows
        .into_iter()
        .filter(|row| {
            let own = row.changeset.key_id.as_deref() == Some(principal.key_id.as_str());
            let this_connection = own && row.changeset.label == ctx.label();
            principal.reaches(&row.catalog)
                && only.is_none_or(|catalog_id| catalog_id == row.changeset.catalog_id)
                && (own || admin)
                && !this_connection
        })
        .map(|row| StrandedChangeset {
            id: row.changeset.id,
            catalog: row.catalog,
            label: row.changeset.label,
            key: row.changeset.key_id,
            actor: row.changeset.actor,
            created: format_timestamp(row.changeset.created_at),
            last_change: format_timestamp(row.changeset.updated_at),
            paths: row.paths,
        })
        .collect();
    Ok(Stranded { changesets })
}

/// Moves an open changeset to this connection's key and label.
///
/// The changeset is read, then the catalog row locked, then this
/// connection's open changeset looked up and the changeset relabelled, in
/// one transaction (Task 1's lock order). Two adoptions of one changeset at
/// once both succeed in turn; the later one owns it.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the changeset's id.
///
/// # Returns
/// The changeset's catalog, new and old label and key, and its paths.
///
/// # Errors
/// [`OkfError::Refused`] if there is no open changeset with that id in a
/// catalog the key reaches, or if this connection already has another open
/// changeset in that catalog; [`OkfError::Forbidden`] without `write` (the
/// key's own) or `admin` (another key's); [`OkfError::Busy`];
/// [`OkfError::Database`].
pub(crate) async fn adopt_changeset(
    ctx: &ToolContext,
    params: AdoptParams,
) -> Result<Adopted, OkfError> {
    use crate::store::changesets::{self, ChangesetKey, ChangesetState};

    let id = params.id;
    // One message for "no such id", "closed" and "not in your catalogs", so
    // a key cannot tell which (and so probe other catalogs).
    let unknown = || OkfError::Refused {
        reason: format!("no open changeset {id} in a catalog this key reaches"),
    };
    let row = changesets::by_id(ctx.app().pool(), id)
        .await?
        .filter(|row| row.state == ChangesetState::Open)
        .ok_or_else(unknown)?;
    let principal = ctx.principal();
    let state = ctx
        .catalogs()
        .values()
        .find(|state| state.id() == row.catalog_id && principal.reaches(state.name()))
        .ok_or_else(unknown)?;
    let own = row.key_id.as_deref() == Some(principal.key_id.as_str());
    let needed = if own { Access::Write } else { Access::Admin };
    principal.require(needed, state.name())?;

    let now = ctx.app().clock().now();
    let mut tx = ctx.app().pool().begin().await?;
    changesets::lock_catalog(&mut tx, row.catalog_id).await?;
    let mine = ChangesetKey {
        catalog_id: row.catalog_id,
        key_id: &principal.key_id,
        label: ctx.label(),
    };
    if let Some(open) = changesets::find_open(&mut *tx, &mine).await? {
        // Adopting the changeset this connection already has is a no-op
        // relabel; any other open one is in the way.
        if open.id != id {
            return Err(OkfError::Refused {
                reason: format!(
                    "this connection already has open changeset {} in {}; publish or \
                     discard it first, or connect with another X-OKF-Changeset label",
                    open.id,
                    state.name()
                ),
            });
        }
    }
    let adopted = changesets::adopt(&mut tx, id, &principal.key_id, ctx.label(), now).await?;
    let entries = changesets::entries(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(Adopted {
        id,
        catalog: state.name().to_string(),
        label: adopted.label,
        previous_key: row.key_id,
        previous_label: row.label,
        paths: entries.into_iter().map(|entry| entry.path).collect(),
    })
}

// As in the read tool modules: the macro turns this block's `#[tool]`
// methods into `lifecycle_router`, which `OkfServer::tools` joins.
#[tool_router(router = lifecycle_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `stranded_sessions` tool.
    #[tool(
        description = "Open changesets holding unpublished edits that nobody has changed \
                       within session_idle_timeout: this key's own (from other connections or \
                       labels), or with an admin key every one in the catalogs it reaches. Each \
                       has an id (for adopt_changeset), catalog, label, key, actor, creation and \
                       last-change times, and changed paths."
    )]
    async fn stranded_sessions(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<StrandedParams>,
    ) -> Result<Json<Stranded>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(stranded_sessions(&ctx, params).await)
    }

    /// The `adopt_changeset` tool.
    #[tool(
        description = "Move an open changeset (its id from stranded_sessions) to this \
                       connection, so this connection's reads show it and its writes, publish \
                       and discard act on it. A write key adopts its own changesets; another \
                       key's needs an admin key. Refused while this connection already has an \
                       open changeset in that catalog."
    )]
    async fn adopt_changeset(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<AdoptParams>,
    ) -> Result<Json<Adopted>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(adopt_changeset(&ctx, params).await)
    }
}
```

- [ ] **Step 4: The prune in the poll loop**

In `src/app.rs` (imports: `chrono::TimeDelta`, `crate::store::changesets`):

```rust
/// How long an empty open changeset may go unchanged before the poll loop
/// discards it (design "Session lifecycle").
const EMPTY_CHANGESET_HOURS: i64 = 24;
```

At the end of `poll`, after `*broken = still_broken;`:

```rust
        self.prune_empty_changesets().await;
```

and the method, after `reload_keys_if_changed`:

```rust
    /// Discards empty changesets unchanged for [`EMPTY_CHANGESET_HOURS`].
    /// Called only under `polling`.
    ///
    /// A failure is logged, not returned: the next pass tries again, and
    /// the rest of the pass is not held up by it.
    async fn prune_empty_changesets(&self) {
        let now = self.clock.now();
        let older_than = now - TimeDelta::hours(EMPTY_CHANGESET_HOURS);
        match changesets::prune_empty(&self.pool, older_than, now).await {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "discarded empty changesets a day old"),
            Err(error) => {
                tracing::error!(%error, "pruning empty changesets failed; retrying at the next poll");
            }
        }
    }
```

Add to `poll`'s doc, after its first sentence: "It also discards empty changesets unchanged for 24 hours (design "Session lifecycle"); a failure there is logged and retried at the next pass."

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- mcp::changesets::lifecycle_tests app::`
Expected: the six `lifecycle_tests` and `poll_discards_empty_changesets_a_day_old_and_keeps_the_rest` pass, with every older `app` test. Then `cargo test`.

- [ ] **Step 6: Update the design**

1. "Read tools", the `stranded_sessions` row: parameters `catalog?`; result "Open changesets holding edits that nobody changed within `session_idle_timeout`: the key's own (other labels than this connection's), or for an `admin` key every one in the catalogs it reaches: id, catalog, label, key, actor, creation and last-change times, changed paths".
2. "Write tools", the `adopt_changeset` row: "Moves an open changeset to this connection's key and label. The key's own needs `write`; another key's needs `admin` on its catalog. Refused while this connection has another open changeset in that catalog; an unknown id, a closed changeset and one in a catalog the key does not reach are refused alike".
3. Check "Session lifecycle" (Task 1's text) still matches: the prune runs on every poll pass and goes by the changeset's last change.

- [ ] **Step 7: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-12-stranded-adopt.md`: the two tools (filters, the connection's own changeset left out, the uniform refusal, the access rule, the busy-connection refusal), `idle_cutoff`, the prune in `poll`, the tests, and the Interfaces notes above (in particular the layout that lets Tasks 10, 11 and 12 merge in any order). Run every gate. Commit:

```bash
git add src/mcp/changesets.rs src/mcp/mod.rs src/app.rs \
    docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "$(cat <<'EOF'
feat: stranded_sessions, adopt_changeset and the empty-changeset prune (M5.12)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: end-to-end and concurrency tests (`tests/writes.rs`)

The write path over real HTTP, with rmcp clients, as an agent sees it (design "Testing"):
- a full create, link, index, log and publish cycle, exported through the route;
- concurrent `log.md` publishes from two keys;
- a conflict as a tool result;
- a changeset that survives a server restart;
- two changesets on one key, kept apart by `X-OKF-Changeset`.

> **Interfaces note (Task 13), 2026-11-03.** M4's `TestServer` has no restart and sets only `X-OKF-Catalogs`. This task adds two methods, and every M4 caller keeps working:
> - **`TestServer::client_with(key, headers)`** sets any headers. `client(key, catalogs)` becomes a wrapper over it.
> - **`TestServer::restart(&mut self, config)`** stops the server and starts a new `App` and listener on the same `TestDb`, so the database (and its changesets) outlive the process, as across a pod restart. `TestServer` implements `Drop`, so its fields cannot be moved out. `restart` takes `&mut self` and replaces `base`, `app` and `shutdown` in place, and `start` and `restart` share one private `serve_app`.

**Files:**
- Modify: `tests/support/server.rs` (`client_with`, `restart`, `serve_app`)
- Create: `tests/writes.rs`
- Create: `changelog/<ts>-m5-task-13-end-to-end.md`

- [ ] **Step 1: Extend the harness**

In `tests/support/server.rs`, replace the end of `start` (from `let app = App::load(...)` to the returned `Self`) with:

```rust
        let (base, app, shutdown) = serve_app(&db, config).await;
        Self {
            base,
            app,
            shutdown,
            db,
        }
    }

    /// Stops the server and starts a new one on the same database, as a
    /// pod restart would: nothing in memory survives, everything stored does.
    ///
    /// # Arguments
    /// * `config` - the new server's settings.
    ///
    /// # Panics
    /// As [`TestServer::start`].
    pub async fn restart(&mut self, config: Config) {
        // The old server's sessions and poll loop stop; its connections close.
        self.shutdown.cancel();
        let (base, app, shutdown) = serve_app(&self.db, config).await;
        self.base = base;
        self.app = app;
        self.shutdown = shutdown;
    }
```

replace `client` with:

```rust
    /// An MCP client with `key`, on a connection naming `catalogs` (the
    /// `X-OKF-Catalogs` header), if any.
    ///
    /// # Arguments
    /// * `key` - the bearer key.
    /// * `catalogs` - the header's value (`"pg,work"`), if any.
    ///
    /// # Returns
    /// The connected client.
    ///
    /// # Errors
    /// The client's failure to initialize (a refused key, say), as text.
    pub async fn client(&self, key: &str, catalogs: Option<&str>) -> Result<Client, String> {
        // `into_iter` on an `Option` yields its value, if any: no header, or one.
        let headers: Vec<(&'static str, &str)> = catalogs.map(|list| ("x-okf-catalogs", list)).into_iter().collect();
        self.client_with(key, &headers).await
    }

    /// An MCP client with `key` and extra request headers (`X-OKF-Catalogs`,
    /// `X-OKF-Changeset`) on every request.
    ///
    /// # Arguments
    /// * `key` - the bearer key.
    /// * `headers` - `(lowercase name, value)` pairs.
    ///
    /// # Returns
    /// The connected client.
    ///
    /// # Errors
    /// As [`TestServer::client`].
    pub async fn client_with(&self, key: &str, headers: &[(&'static str, &str)]) -> Result<Client, String> {
        let headers: HashMap<HeaderName, HeaderValue> = headers
            .iter()
            // `&(name, value)` copies the two references out of each pair.
            .map(|&(name, value)| {
                (HeaderName::from_static(name), HeaderValue::from_str(value).expect("header value"))
            })
            .collect();
        let config = StreamableHttpClientTransportConfig::with_uri(self.mcp_url())
            .auth_header(key)
            .custom_headers(headers);
        // `()` is a client handler that does nothing: tests only call tools.
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|error| error.to_string())
    }
```

and add after the `impl TestServer` block:

```rust
/// Loads the app on `db` and serves it on a free port.
///
/// # Returns
/// The base URL, the app, and the token that stops the server.
///
/// # Panics
/// If the app does not load or the bind fails.
async fn serve_app(db: &TestDb, config: Config) -> (String, Arc<App>, CancellationToken) {
    let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
        .await
        .expect("app loads");
    // Port 0: the OS picks a free one.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let shutdown = CancellationToken::new();
    tokio::spawn(okf_mcp::http::serve(Arc::clone(&app), listener, shutdown.clone()));
    (base, app, shutdown)
}
```

Run: `cargo test --test server`
Expected: every M4 and Task 6 test passes unchanged.

- [ ] **Step 2: Write the tests**

Create `tests/writes.rs`:

```rust
//! End-to-end tests of the write tools (design "Testing"), over real HTTP
//! with rmcp's client:
//! - a full create, link, index, log, publish and export cycle;
//! - concurrent publishes of `log.md`;
//! - a conflict as an agent sees it;
//! - a changeset surviving a restart;
//! - `X-OKF-Changeset` keeping two changesets of one key apart.

mod support;

use okf_mcp::archive::{read_tarball, Files};
use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};
use support::server::{call, files, structured, Client, TestServer};

const TIME: &str = "---\ntype: System\ntitle: Time server\ndescription: The GPS clock.\n---\n# Time server\n";

/// One catalog, `pg`: a root index, one folder with one concept, a log.
fn catalog() -> Vec<(&'static str, Files)> {
    vec![(
        "pg",
        files(&[
            ("index.md", "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n"),
            ("infra/index.md", "# Infra\n\n* [Time server](time.md) - the GPS clock\n"),
            ("infra/time.md", TIME),
            ("log.md", "# Log\n"),
        ]),
    )]
}

/// A write key with `pg` as its default.
async fn writer(server: &TestServer, id: &str) -> String {
    server.key(id, Access::Write, &[], Some("pg")).await
}

/// Calls a tool that must succeed and returns its JSON.
async fn ok(client: &Client, tool: &'static str, arguments: Value) -> Value {
    structured(&call(client, tool, arguments).await).clone()
}

/// The paths of a `publish` result's revisions.
fn revised(published: &Value) -> Vec<&str> {
    published["revisions"]
        .as_array()
        .expect("revisions")
        .iter()
        .filter_map(|revision| revision["path"].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn create_link_index_log_publish_and_export() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let reader_key = server.key("reader", Access::Read, &[], Some("pg")).await;
    let client = server.client(&key, None).await.unwrap();
    let reader = server.client(&reader_key, None).await.unwrap();

    ok(&client, "create_concept", json!({
        "path": "/infra/ntp.md",
        "frontmatter": {"type": "System", "title": "NTP", "description": "How hosts sync."},
        "body": "# NTP\n\nHosts sync from the [time server](/infra/time.md).\n",
    }))
    .await;
    ok(&client, "index", json!({"path": "/infra/ntp.md", "summary": "how hosts sync"})).await;
    ok(&client, "log", json!({"text": "added NTP", "kind": "Creation"})).await;
    // Before publish: the writer sees the concept through its overlay, the reader does not.
    let pending = ok(&client, "pending_changes", json!({})).await;
    let seen = ok(&client, "get_concept", json!({"path": "/infra/ntp.md"})).await;
    let unseen = call(&reader, "get_concept", json!({"path": "/infra/ntp.md"})).await;
    let published = ok(&client, "publish", json!({"message": "add NTP"})).await;
    let history = ok(&reader, "history", json!({"path": "/infra/ntp.md"})).await;
    let response = reqwest::Client::new()
        .get(format!("{}/catalogs/pg/export", server.base))
        .bearer_auth(&reader_key)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let tarball = response.bytes().await.unwrap();
    client.cancel().await.unwrap();
    reader.cancel().await.unwrap();

    assert_eq!(pending["files"].as_array().unwrap().len(), 3);
    assert_eq!(seen["title"], "NTP");
    assert_eq!(unseen.is_error, Some(true));
    assert_eq!(published["published"], true);
    assert_eq!(revised(&published), ["/infra/index.md", "/infra/ntp.md", "/log.md"]);
    assert_eq!(published["revisions"][1]["op"], "create");
    assert_eq!(history["revisions"][0]["message"], "add NTP");
    assert_eq!(history["revisions"][0]["actor"], "writer");
    assert_eq!(status, reqwest::StatusCode::OK);
    let exported = read_tarball(&tarball[..], u64::MAX).unwrap();
    let text = |path: &str| String::from_utf8(exported[path].content.clone()).unwrap();
    assert!(text("infra/ntp.md").contains("[time server](/infra/time.md)"));
    assert!(text("infra/index.md").contains("[NTP](ntp.md) - how hosts sync"));
    assert!(text("log.md").contains("* **Creation:** added NTP"));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_log_publishes_from_two_keys_keep_both_bullets() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let (first_key, second_key) = (writer(&server, "writer-a").await, writer(&server, "writer-b").await);
    let first = server.client(&first_key, None).await.unwrap();
    let second = server.client(&second_key, None).await.unwrap();
    ok(&first, "log", json!({"text": "from a"})).await;
    ok(&second, "log", json!({"text": "from b"})).await;

    // `tokio::join!` sends both publishes at once; the server's publish lock
    // runs them one after the other.
    let (one, two) = tokio::join!(
        call(&first, "publish", json!({"message": "a"})),
        call(&second, "publish", json!({"message": "b"})),
    );
    let log = ok(&first, "read_document", json!({"path": "/log.md"})).await;
    first.cancel().await.unwrap();
    second.cancel().await.unwrap();

    let (one, two) = (structured(&one).clone(), structured(&two).clone());
    assert_eq!((one["published"].as_bool(), two["published"].as_bool()), (Some(true), Some(true)));
    let replays = [&one, &two].iter().map(|result| result["replayed"].as_array().unwrap().len()).sum::<usize>();
    assert_eq!(replays, 1, "exactly the second publish replays log.md");
    let text = log["text"].as_str().unwrap();
    assert!(text.contains("* **Update:** from a") && text.contains("* **Update:** from b"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_sees_a_conflict_and_can_redo_its_edit() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let (first_key, second_key) = (writer(&server, "writer-a").await, writer(&server, "writer-b").await);
    let first = server.client(&first_key, None).await.unwrap();
    let second = server.client(&second_key, None).await.unwrap();
    let edit = |text: &str| json!({"path": "/infra/time.md", "body": format!("# Time server\n\n{text}\n")});
    ok(&first, "replace_body", edit("From a.")).await;
    ok(&second, "replace_body", edit("From b.")).await;
    ok(&first, "publish", json!({"message": "a"})).await;

    let refused = call(&second, "publish", json!({"message": "b"})).await;
    let still_pending = ok(&second, "pending_changes", json!({})).await;
    ok(&second, "discard", json!({})).await;
    ok(&second, "replace_body", edit("From a, then b.")).await;
    let redone = ok(&second, "publish", json!({"message": "b again"})).await;
    first.cancel().await.unwrap();
    second.cancel().await.unwrap();

    assert_eq!(refused.is_error, Some(true));
    let body = refused.structured_content.expect("the error's JSON");
    assert_eq!(body["error"], "Conflict");
    assert_eq!(body["files"], json!([{"path": "/infra/time.md", "base_revision": 1, "current_revision": 2}]));
    assert_eq!(still_pending["files"][0]["path"], "/infra/time.md");
    assert_eq!(redone["revisions"][0]["revision"], 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changeset_survives_a_server_restart() {
    let mut server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    // No label: the `default` changeset is the same across a restart, even
    // though the new client gets a new MCP session.
    let before = server.client(&key, None).await.unwrap();
    let written = ok(&before, "log", json!({"text": "before the restart"})).await;
    before.cancel().await.unwrap();

    server.restart(Config::default()).await;
    let after = server.client(&key, None).await.unwrap();
    let pending = ok(&after, "pending_changes", json!({})).await;
    let published = ok(&after, "publish", json!({"message": "after the restart"})).await;
    after.cancel().await.unwrap();

    assert_eq!(written["label"], "default");
    assert_eq!(pending["changeset"], written["changeset"]);
    assert_eq!(pending["files"][0]["path"], "/log.md");
    assert_eq!(published["published"], true);
    assert_eq!(published["changeset"], written["changeset"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn x_okf_changeset_keeps_two_changesets_of_one_key_apart() {
    let server = TestServer::start(catalog(), Config::default()).await;
    let key = writer(&server, "writer").await;
    let drafts = server.client_with(&key, &[("x-okf-changeset", "draft-a")]).await.unwrap();
    let others = server.client_with(&key, &[("x-okf-changeset", "draft-b")]).await.unwrap();
    let edited = ok(&drafts, "replace_body", json!({"path": "/infra/time.md", "body": "# Time server\n\nDraft a.\n"})).await;
    let logged = ok(&others, "log", json!({"text": "draft b"})).await;

    let published = ok(&drafts, "publish", json!({"message": "draft a"})).await;
    let left = ok(&others, "pending_changes", json!({})).await;
    let seen_by_b = ok(&others, "get_concept", json!({"path": "/infra/time.md"})).await;
    drafts.cancel().await.unwrap();
    others.cancel().await.unwrap();

    assert_ne!(edited["changeset"], logged["changeset"]);
    assert_eq!(revised(&published), ["/infra/time.md"]);
    let paths: Vec<&str> = left["files"].as_array().unwrap().iter().filter_map(|file| file["path"].as_str()).collect();
    assert_eq!(paths, ["/log.md"]);
    // Published now, so draft b's connection sees draft a's edit too.
    assert!(seen_by_b["body"].as_str().unwrap().contains("Draft a."));
}
```

Run: `cargo test --test writes`
Expected: 5 passed.

If a test fails because of a shipped detail (a field name in `get_concept`'s result, how `ensure_index` words a bullet), fix the test to the shipped behaviour. Then check the plan's earlier tasks describe that behaviour, and correct them with a dated note if they don't.

- [ ] **Step 3: Changelog, gates, commit**

Write `changelog/<ts>-m5-task-13-end-to-end.md`:
- **Added:** `tests/writes.rs` (the five tests); `TestServer::{client_with, restart}`.
- **Changed:** `client` on `client_with`; `start` and `restart` share `serve_app`.
- **Notes:**
  - the restart test uses `X-OKF-Changeset`, since a new client has a new session;
  - the concurrency test sends both publishes at once, so it exercises the publish lock and the replay;
  - the timings of `cargo test --test writes`.

Run every gate. Commit:

```bash
git add tests/support/server.rs tests/writes.rs changelog/
git commit -m "test: end-to-end writes: publish cycle and export, concurrent logs, conflicts, restart, labels (M5.13)" \
    -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: the M5 smoke test, README, design update and milestone changelog

This task runs the write path on the real catalog and times it, documents the write tools, and closes the milestone in the design. It follows M4's Task 12.

**Files:**
- Modify: `tests/smoke.rs` (add `writes_on_a_real_catalog`)
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`
- Create: `changelog/<ts>-m5-writes.md` (milestone) and `changelog/<ts>-m5-task-14-wrap-up.md`

- [ ] **Step 1: The smoke test**

Add to `tests/smoke.rs`. In its module doc, write "four tests" where it now says "All three tests", and add to the list: "`writes_on_a_real_catalog` (M5) imports the checkout into a throwaway catalog, creates a concept with a new folder index, stamps it, logs it and publishes, timing each write tool and the publish through rmcp's client."

```rust
/// Write to a real catalog over HTTP and time the write path and publish.
///
/// The checkout is imported into the test's own Postgres container, so the
/// real catalog is never written. The new concept goes in a new folder:
/// `create_concept` with `index_summary` then also creates the folder's
/// `index.md` and lists it in the root index, as `ensure_index` does, so
/// publish replays nothing but checks three changed files against the
/// whole catalog's lint. Only sizes and timings are printed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn writes_on_a_real_catalog() {
    use okf_mcp::archive::read_directory;
    use okf_mcp::config::Config;
    use okf_mcp::store::keys::Access;
    use serde_json::json;
    use support::server::{call, TestServer};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let files = read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    let count = files.len();
    let started = Instant::now();
    let server = TestServer::start(vec![("pg", files)], Config::default()).await;
    eprintln!(
        "start Postgres, import {count} files and start the server: {:?}",
        started.elapsed()
    );
    let key = server.key("smoke", Access::Write, &[], Some("pg")).await;
    let client = server
        .client_with(&key, &[("x-okf-changeset", "smoke")])
        .await
        .unwrap();

    // The first write builds the overlay (a full snapshot); the later ones
    // rebuild it after each version bump, so their times show that cost.
    let calls = [
        ("create_concept", json!({
            "path": "/m5-smoke/probe.md",
            "frontmatter": {"type": "Note", "title": "M5 smoke probe", "description": "Written by the M5 smoke test."},
            "body": "# M5 smoke probe\n\nSee the [log](/log.md).\n",
            "index_summary": "the M5 smoke test's probe",
        })),
        ("stamp", json!({"path": "/m5-smoke/probe.md", "field": "verified"})),
        ("log", json!({"text": "M5 smoke probe", "kind": "Creation"})),
        ("pending_changes", json!({})),
        ("lint", json!({"pending": true})),
        ("publish", json!({"message": "M5 smoke test"})),
    ];
    let mut failed = Vec::new();
    let mut published = serde_json::Value::Null;
    for (tool, arguments) in calls {
        let started = Instant::now();
        let result = call(&client, tool, arguments).await;
        let elapsed = started.elapsed();
        let bytes = result
            .structured_content
            .as_ref()
            .map_or(0, |value| value.to_string().len());
        eprintln!("{tool}: {bytes} bytes of JSON in {elapsed:?}");
        if result.is_error == Some(true) {
            // The `{error, message}` JSON names paths and rules, never document text.
            failed.push((tool, result.content));
        } else if tool == "publish" {
            published = result.structured_content.unwrap_or_default();
        }
    }
    client.cancel().await.unwrap();

    assert!(failed.is_empty(), "these tools returned errors: {failed:?}");
    assert_eq!(published["published"], true);
    let revisions = published["revisions"].as_array().map_or(0, Vec::len);
    eprintln!(
        "publish wrote {revisions} revisions, {} warnings",
        published["warnings"].as_array().map_or(0, Vec::len)
    );
    assert!(revisions >= 4, "the probe, its folder index, the root index and log.md");
}
```

Run: `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`
Expected: four tests pass. Paste the timings into the milestone changelog.

If the real catalog's root `index.md` has lint errors of its own, publish refuses with `LintFailed`, since that file is changed. Record that in the changelog as a finding about the catalog, and point the probe at a folder that already has an `index.md`; don't weaken the assertion.

- [ ] **Step 2: README**

Rewrite the status line (M1 to M5 delivered) and add a "Writing" section after "Running the server":
- **Changesets.** A write key's edits go to a changeset per (catalog, key, label). The label is the `X-OKF-Changeset` header, else `?changeset=` on the MCP URL, else the MCP session, else `default`. Nothing is visible to others until `publish`, and a changeset survives disconnects and restarts until it is published or discarded.
- **The `mcp.json` entry with a label:**

  ```json
  {"mcpServers": {"okf": {"type": "http", "url": "https://okf.example/mcp",
    "headers": {"Authorization": "Bearer ${OKF_MCP_KEY}", "X-OKF-Catalogs": "pg",
                "X-OKF-Changeset": "my-agent"}}}}
  ```
- **The write tools, one line each:**
  - content: `create_concept`, `edit_document`, `replace_body`, `set_frontmatter`;
  - bookkeeping: `stamp`, `review`, `question`, `unquestion`, `add_source`, `log`, `index`;
  - changesets: `pending_changes`, `publish`, `discard`, `revert`, `stranded_sessions`, `adopt_changeset`;
  - `lint` with `pending: true`.
- **Publish:**
  - it is refused with `Conflict` (re-read, redo, publish again) or `LintFailed`;
  - `log` and `index` entries are replayed onto newer text instead of conflicting.
- **The write rules:**
  - the secret scan;
  - frontmatter must parse;
  - paths stay inside the catalog;
  - machinery is read-only: `AGENTS.md`, `CLAUDE.md`, `README.md`, `tools/`, `.claude/`, `.git*`, `install/`, the root `docs/` and `tests/`.
- **Timestamps:** every one comes from the server's clock; `by` defaults to the key's actor.

Keep the testing section's Docker note, and add `cargo test --test writes`.

- [ ] **Step 3: The design**

- Milestones table: "M5. Writes (delivered)", with the plan link and the PR range.
- The status line at the top: M1 to M5 delivered.
- Add "What M5 shipped differently", after M4's. Its "Deviations planned" are the plan's Decisions and its "Simplifications and deviations":
  - changesets per (catalog, key, label), labels from `X-OKF-Changeset`, `?changeset=`, the session or `default`;
  - MCP sessions bound to their key (404 otherwise);
  - the lock order (catalog row `FOR KEY SHARE`, then the changeset row), and `Busy` for `40P01` and `40001`;
  - `install/`, the root `docs/` and `tests/` as machinery;
  - no derived-column backfill;
  - overlays rebuilt whole and cached per (changeset version, generation);
  - ops checked byte for byte against `okf.py`;
  - no discard on session close;
  - `stranded_sessions` by idle time;
  - `publish` with nothing to publish returns `published: false`;
  - `LintFailed` is 422;
  - publish records the publishing key's actor;
  - a file whose publish result equals the published one gets no revision;
  - links into deleted paths are `link-into-deleted` errors in `LintFailed`.

  Then "Plan corrections", one bullet per dated `Corrected` note in the M5 plan, as for M4.
- "Open questions for M5": rename it "Questions M5 answered" and give each bullet its answer (Decisions 1 to 5).
- "Session lifecycle" and "Storage" should already be Task 1's text. Check that they are.

- [ ] **Step 4: The milestone changelog**

Write `changelog/<ts>-m5-writes.md`. It covers:
- what M5 delivers;
- the seven Decisions;
- the deviations;
- every `Corrected` note;
- the smoke timings: create, stamp, log, pending, lint pending, publish;
- the test counts against M4's: 342 unit, 8 server, 6 golden parity, 1 roundtrip, 13 doc. Add the new suites: `tests/writes.rs` (5), the ops parity golden and live tests, and the smoke test (4 ignored).
- the follow-ups for M6:
  - `move_concept`, `split_concept` and `delete_concept` on `write::apply`. A deletion is a touched path the op removed, through `MemWorkspace::remove`. Their listing changes need `ReplayOp` variants for renaming and removing a bullet;
  - the derived-column backfill, when `Derived::of` first changes;
  - anything filed during M5.

Write `changelog/<ts>-m5-task-14-wrap-up.md` for this task itself.

- [ ] **Step 5: Gates, commit**

Run every gate, plus the release smoke test. Commit:

```bash
git add tests/smoke.rs README.md docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "docs: M5 smoke test, README, design update and changelog (M5.14)" \
    -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Self-review (2026-11-03)

- **Design coverage (M5 row and "Write tools").**
  - **Write tools:**
    - content tools: `create_concept`, `edit_document`, `replace_body` and `set_frontmatter` (ops T4, tools T8);
    - bookkeeping tools: `stamp`, `review`, `question`, `unquestion`, `add_source`, `log` and `index` (ops T2 and T3, tools T9).
  - **Changeset tools:**
    - `publish` with conflicts and replay (T10);
    - `discard`, `revert` and `pending_changes` (T11);
    - `stranded_sessions` and `adopt_changeset` (T12).
  - **Supporting pieces:**
    - changesets and overlays (T1, T7);
    - `okf::ops` (T2-T4);
    - `lint pending` (T7);
    - the write rules (T5).
  - **Out of M5:** `move_concept`, `split_concept` and `delete_concept` are M6.
  - **Tests:**
    - database tests in T1 and T10;
    - end-to-end and concurrency tests in T13: the full cycle, concurrent `log.md` publishes, survival across a restart.
  - **The open M5 questions** are each answered by Decisions 1-5 and implemented in T1 (identity, lock order), T6 (binding), T5 (machinery), and Decision 5 (no backfill).
- **Placeholders.** None. Every step that changes code shows the code. The tasks were drafted in three parallel groups against the interfaces block. Each task's "Interfaces note" records what it adds beyond the block, and the block was reconciled with all of them.
- **Names across tasks.** Every interface symbol (`lock_catalog`, `get_or_create`, `lock`, `with_changes`, `with_dirty`, `remove`, `connection_label`, `bind_session`, `build_overlay`, `view`, `apply`, `replay`, `ensure_index`, `is_machinery`, `check_path`, `check_content`, `publish_lock`, `publish`, `content_at`, `write_document`, `writer`, `restart`, `client_with`) has one definition, in the task the file map assigns it to.
- **Shared files.** `src/mcp/changesets.rs` takes three routers (`publish_router`, `changes_router`, `lifecycle_router`) and three test modules, in whichever order T10, T11 and T12 merge. The first of them creates the file.
