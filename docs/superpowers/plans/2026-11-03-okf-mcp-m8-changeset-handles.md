# OKF MCP Server, Milestone 8 (Changeset handles): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Changesets are named by a handle the server issues, not by a label the client sends. `open_changeset` returns a random handle. Every write tool, `publish`, `discard`, `revert` and `pending_changes` takes it as a required `changeset` argument, and the read tools take it optionally to read through the drafts. The `X-OKF-Changeset` header, `?changeset=`, the `default` label, `stranded_sessions` and `adopt_changeset` go. `list_changesets` takes their place. Every error payload writes catalog paths `/`-prefixed. Then 0.8.0 is released and rolled out.

**Architecture:**
- **Storage:** a migration adds `changesets.handle` (text, unique, `gen_random_uuid()::text` by default). A later one drops `label` and its unique index. Changesets keep their bigint `id` internally: entries and revisions reference it, and the overlay cache is keyed by it.
- **Resolving a handle:** `ToolContext::changeset(handle, access)` finds the row, its catalog and checks ownership. It returns a `Pinned` (catalog state plus row) or `UnknownChangeset`. Every changeset tool and the write path start from it.
- **Writes:** `write::apply` takes the handle instead of a catalog and label, and keeps its transaction and lock order (catalog row, then changeset row). A closed changeset is now an ordinary refusal, not a defensive `Busy`.
- **Reads:** `ToolContext::view(state, pinned)` uses the pinned changeset's overlay for its own catalog, and the published snapshot everywhere else. Without a `changeset` argument a read is always published, whatever the key.

**Tech Stack:** Rust 2021 (toolchain 1.99.1), and everything M1 to M7 use. There are no new dependencies. Postgres's built-in `gen_random_uuid()` (13 and later) makes the handles.

---

## Roadmap position

This is milestone 8. It is the first milestone after the design's original seven, which were all delivered by M7 (`main` at `628f84c`, epic #139 closed). This plan's PR adds M8 to the design's "Milestones" table, and a "Revision: changeset handles" section. The cutover of the git catalogs stays out of scope (design "Out of scope"). M8 removes the label, which was the main obstacle to many sessions writing at once.

## Scope

In scope:
- **Handles:** a migration, the store functions, `open_changeset`, and `UnknownChangeset` (Task 2).
- **The write path, the changeset tools and the reads:** writes and the changeset tools take `changeset`, and the write tools lose `catalog`; the read tools take an optional `changeset`, and `lint`'s `pending` becomes it (Task 3).
- **`list_changesets`:** it replaces `stranded_sessions` and `adopt_changeset` (Task 4).
- **Labels removed:** the header, the query parameter, the context's label and the `label` column; README, operations guide, design and version 0.8.0 (Task 5).
- **The error-path follow-up** from things-to-fix (found in Brooke's review of PR #152). Every error payload writes catalog paths `/`-prefixed (Task 1).
- **Release and rollout of 0.8.0,** with pgrace's go-ahead at each outward step (Task 6).
- **Wrap-up:** the milestone changelog, the design's "What M8 shipped differently", the smoke test (Task 7).

Deliberately deferred:

| Item | Where it goes |
|---|---|
| A one-shot atomic `apply` (operations and a message in one call) | Not planned. Discussed and dropped on 2026-11-03: handles fix the problem that motivated it. Revisit if the number of calls per write turns out to matter |
| Fewer tool definitions (a token cost) | Not planned; see above |
| The tool results' own path forms (reports ported from `okf.py --json`, such as `lint`'s findings) | Unchanged: they keep `okf.py`'s JSON shapes, which the parity tests pin. Task 1 covers error payloads only |
| The M5 write-path gaps (pool limits, the overlay cache's size cap) | things-to-fix. Task 3 removes the "defensive `Busy`" gap: that branch becomes a tested refusal |
| Cutting the git catalogs over to the server | Design "Out of scope"; pgrace decides later |

## Decisions taken while planning (2026-11-03)

1. **Changesets are named by a server-issued handle** (pgrace). `open_changeset` returns it. Every call on that changeset passes it back. Clients set no header and no label. Why: a label the client has to choose was brittle. Two sessions that send the same label silently share drafts, and a session that changes its label loses them. No setting in Claude Code gives each session a stable, distinct header without a helper script.
2. **A write without a handle is refused** (pgrace). `changeset` is a required parameter of every write tool, and of `publish`, `discard`, `revert` and `pending_changes`. Leaving it out fails rmcp's parameter check (`failed to deserialize parameters: missing field \`changeset\``). The tool descriptions say to call `open_changeset` first. A write never opens a changeset implicitly, so a forgotten handle can't scatter edits over several changesets.
3. **The changeset's key may use it, and so may an admin** (pgrace). A handle works on any connection of the key that opened it, so resuming after a reconnect, a compaction or `--resume` needs nothing but the handle. A key with `admin` access that reaches the changeset's catalog may use it directly, which is how an admin takes work over. `adopt_changeset` is removed. Any other key gets `UnknownChangeset`, worded exactly as for a handle that doesn't exist, so a key can't probe for other keys' handles.
4. **The handle is a random UUID in text form,** generated by Postgres (`gen_random_uuid()::text`). It isn't a secret: the key still authorizes every call, and a handle alone grants nothing. It is random so that a typo or a guess can't land in another session's changeset by accident.
5. **The catalog comes from the changeset.** The write tools, `publish`, `discard`, `revert` and `pending_changes` lose their `catalog` parameter. `open_changeset` takes `catalog` (default: the connection's write target).
6. **Reads see drafts only when asked.** A read tool given `changeset` reads that changeset's overlay for the changeset's own catalog, and the published state for every other catalog it selects. Without it, a read is published, even for a key that has open changesets. If the changeset's catalog is not one the call selects, the read is refused with `Refused`, which names the catalog. `lint`'s `pending` flag is replaced by `changeset`.
7. **Several open changesets per key and catalog are allowed.** The `(catalog, key, label)` unique index goes. Until Task 5 drops the column, a new changeset's `label` is set to its handle, so the old index can never clash.
8. **`list_changesets` replaces `stranded_sessions`.** It lists the open changesets the key may use: its own, and with `admin` every key's in the catalogs it reaches. Empty ones are listed too. Each row gives the handle, catalog, key, actor, creation and last-change times, its paths, and `idle` (no change within `session_idle_timeout`). The 24-hour prune of empty changesets stays, and with handles its effect is visible: a handle that `open_changeset` returned and that gets no write within 24 hours is discarded (`prune_empty`, `src/store/changesets.rs:671-690`; a write bumps `updated_at`, so only empty ones go). Using it afterwards is refused with "changeset … is discarded", plus a hint that an empty changeset is discarded after 24 hours without a write and to open a new one. The `open_changeset` description says so too.
9. **Every error payload writes catalog paths `/`-prefixed** (pgrace chose the things-to-fix entry). The prefix is applied where the error becomes JSON (`OkfError::to_json`), so the messages and the internal forms that the `okf.py` parity tests pin don't change. It covers:
   - `path` of `NotFound`, `FrontmatterParse`, `Shape`, `SecretDetected` and `EditMismatch`;
   - `file` of each finding in `LintFailed`.

   - `path` of `InvalidPath` too, unless it is empty or climbs out of the catalog once normalized (`escapes_root(&normpath(path))`: `..`, `../x`, or `infra/../../x.md`). Those aren't catalog paths and stay as given. `InvalidPath.path` is mixed today (M7 Task 1's table): a normalized catalog path for machinery (`check_path`), "already exists" (`new_concept_path`) and "is not a concept document" (`resolve_concept`), and the caller's reference otherwise. Both become `/`-prefixed, and a reference given as `infra/x` reads `/infra/x`, which every tool accepts.
 `Io` is internal: its payload is the generic "internal error" message plus its `path`, which stays as it is. `Io.path` is documented as catalog-relative, but it is also a filesystem path for a catalog root and an archive entry's name for import and export (M7 Task 1's table). `Config` writes no `path` in its payload at all (its `to_json` arm is empty, `src/error.rs:310-315`), so there is nothing to change. Report results (`lint`, `oldest`, `sprawl`, `links_report`) keep `okf.py`'s unprefixed forms on purpose, for parity. So the same finding reads `/a.md` in a `LintFailed` error body and `a.md` in the `lint` tool's result. The README and the design say this explicitly (Task 1, Step 5).
10. **Version 0.8.0.** This breaks the tool API, but no client uses the write tools yet: claude.ai is read-only (pgrace's instructions file), and no other client is connected with a write key. The read tools change only by adding an optional `changeset`. The release and its rollout follow M7's go-ahead rules (Task 6).
11. **The header helper script goes** (pgrace). `~/.claude/okf-mcp-changeset.sh` was written on 2026-11-03 to give each Claude Code session its own label. It was tested only against a dummy server and never configured for a real client, which is consistent with Decision 10. It lives outside this repo, so Task 7 deletes it locally and records that in the catalog.

## Simplifications and deviations relative to the design

- The design's Decision "Write grouping" and its sections "Write tools", "Data flow" (Write, step 2) and "Session lifecycle" describe labels. This plan's PR adds the M8 milestone row and a short "Revision: changeset handles (2026-11-03)" section that names what replaces them. Task 5 rewrites those sections, once the code matches.

## Verified facts the code relies on

- **rmcp 3.8.0, missing required parameter:** `Parameters<P>` deserializes the arguments and fails with `ErrorData::invalid_params(format!("failed to deserialize parameters: {error}"))` (`rmcp-3.8.0/src/handler/server/tool.rs:239-245`). A `changeset: String` field with no `#[serde(default)]` is therefore required, and the schema marks it so.
  > **Corrected (issue #162), 2026-11-04.** The client does not get a JSON-RPC error: rmcp's tool router (`rmcp-3.8.0/src/handler/server/router/tool.rs:144-156`, `into_tool_argument_error`) turns an `invalid_params` error whose message starts with `failed to deserialize parameters:` into a tool result with `isError: true`, no structured content and the message as its text. `tests/writes.rs`'s `a_write_without_a_changeset_is_a_parameter_error` asserts exactly that text.
- **Postgres `gen_random_uuid()`** is built in since 13 (no `pgcrypto`). The tests run `postgres:17-alpine` (`tests/tls.rs:90-91`) and production runs 18. `ADD COLUMN ... DEFAULT <volatile expression>` evaluates the default once per existing row, so each existing changeset gets a distinct handle.
- **Migrations run on every subcommand's connect,** `serve` included (`src/cli.rs:481-486`, `connect` calls `db::migrate`). The rollout needs no separate migrate step.
- **The current lock order** is the catalog row (`changesets::lock_catalog`, `FOR KEY SHARE`), then the changeset row (`changesets::lock`, `FOR UPDATE`). See `src/write.rs:126-145` and `src/store/publish.rs:140-150`. It stays.
- **`Principal::require(access, catalog)`** returns `CatalogNotSelected` for a catalog the key doesn't reach, and `Forbidden` below `access` (`src/auth.rs:73-90`). The level is per key, not per catalog (`principal.access`).
- **`ToolContext::view`** is called by `search` (`src/mcp/search.rs:111`), `get_concept`/`read_document` (through `view_or_note`, `src/mcp/documents.rs:284`), the reports (`one_view`, `src/mcp/reports.rs:331`) and `lint` (`src/mcp/reports.rs:301`). `history` doesn't read through overlays.
- **`own_changeset`** (`src/mcp/changesets.rs:332`) is the shared lookup of `publish`, `discard`, `revert` and `pending_changes`. Task 3 replaces it with `ToolContext::changeset`.
- **`revisions.changeset_id`** is nullable with `ON DELETE SET NULL` (`migrations/0001_schema.sql:108`). Keeping the bigint id leaves history untouched.
- **`OkfError::to_json`** (`src/error.rs:270-319`) is the only place a tool error becomes JSON (`tool_error`, `src/mcp/mod.rs:129-137`; the HTTP routes use it too).

## Conventions for every task

The M4 to M7 plans' "Conventions for every task" apply unchanged. That includes: no `.unwrap()` in library code, `thiserror` errors, a doc comment on every public item, comments for a Python expert, no emoji, never a secret-shaped string, and outward steps wait for pgrace's go-ahead.

**Nothing site-specific in this repository** (it is public). Docs and changelogs say `example.com`, never an internal address, bucket, Vault path, mirror name, private repository name or commit.

**Gates** (all must pass before every push; paste the counts into the PR):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -- --test-threads=6` (Docker, for testcontainers). If you split it, `--tests` and `--doc` can't share one cargo call.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`
- Task 7 adds the release smoke test (`cargo test --release --test smoke -- --ignored`).

There is no CI and no justfile, so run the gates locally.

**Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m8-task-<NN>-<slug>.md`, with Added/Changed/Notes sections.

Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with the Claude Code attribution line.

The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## Interfaces shared across tasks

Task 2 defines these, and Tasks 3 and 4 use them:

```rust
// src/store/changesets.rs
pub struct ChangesetRow {
    pub id: i64,
    pub handle: String,        // new (Task 2)
    pub catalog_id: i64,
    pub key_id: Option<String>,
    pub actor: String,
    pub state: ChangesetState,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
pub async fn open(conn: &mut PgConnection, catalog_id: i64, key_id: &str, actor: &str,
                  now: DateTime<Utc>) -> Result<ChangesetRow, OkfError>;
pub async fn by_handle<'e>(executor: impl PgExecutor<'e>, handle: &str)
                  -> Result<Option<ChangesetRow>, OkfError>;

// src/error.rs
OkfError::UnknownChangeset { changeset: String }   // kind "UnknownChangeset"; JSON field `changeset`

// src/mcp/context.rs
pub struct Pinned { pub state: Arc<CatalogState>, pub row: ChangesetRow }
impl ToolContext {
    pub async fn changeset(&self, handle: &str, access: Access) -> Result<Pinned, OkfError>;
}
```

After Task 3, the result types name the handle as `changeset: String` and drop `label`: `WriteReport`, `Published`, `Discarded` and `PendingChanges`. Task 3 also changes `ToolContext::view(&self, state: &CatalogState, pinned: Option<&Pinned>)`, and adds (as shipped):

```rust
// src/mcp/context.rs
impl ToolContext {
    pub async fn pin(&self, handle: Option<&str>) -> Result<Option<Pinned>, OkfError>;
    // The one catalog a report runs on: `catalog`, else the pinned changeset's, else the first selected.
    pub fn report_catalog(&self, catalog: Option<&str>, pinned: Option<&Pinned>)
                          -> Result<Arc<CatalogState>, OkfError>;
}
pub fn require_selected(pinned: Option<&Pinned>, states: &[Arc<CatalogState>]) -> Result<(), OkfError>;

// src/write.rs
pub async fn apply<F>(ctx: &ToolContext, changeset: &str, by: Option<&str>, op: F)
                      -> Result<WriteReport, OkfError>;

// src/mcp/changesets.rs
pub struct PendingParams { pub changeset: String }   // pending_changes' arguments

// src/mcp/testing.rs (test helpers)
pub(crate) async fn open(ctx: &ToolContext) -> String;                       // open_changeset in the write target
pub(crate) async fn open_at(test: &TestApp, ctx: &ToolContext, catalog: &str, at: DateTime<Utc>) -> ChangesetRow;
pub(crate) async fn changeset_row(test: &TestApp, handle: &str) -> ChangesetRow;
pub(crate) async fn pending_entry(test: &TestApp, handle: &str, edit: &PendingEdit<'_>) -> ChangesetRow;
// `PendingEdit` lost its `catalog` field: the handle's changeset names the catalog.
```

Task 4 adds (as shipped):

```rust
// src/store/changesets.rs
pub struct ListedRow { pub changeset: ChangesetRow, pub catalog: String, pub paths: Vec<String> }
pub async fn list_open(pool: &PgPool, catalog_ids: &[i64], key_id: Option<&str>)
                       -> Result<Vec<ListedRow>, OkfError>;

// src/mcp/changesets.rs
pub struct ListParams { pub catalog: Option<String> }
pub struct Listed { pub changesets: Vec<ListedChangeset> }  // ListedChangeset as in Task 4
```

## File map

| File | Tasks |
|---|---|
| `src/error.rs` | 1 (`to_json` prefixes), 2 (`UnknownChangeset`) |
| `migrations/0003_changeset_handles.sql` (new) | 2 |
| `migrations/0004_drop_changeset_labels.sql` (new) | 5 |
| `src/store/changesets.rs` | 2 (`handle`, `open`, `by_handle`), 3 (drop `get_or_create`, `find_open`, `ChangesetKey`), 4 (`list_open`; drop `stranded`, `adopt`), 5 (drop `label`) |
| `src/mcp/context.rs` | 2 (`Pinned`, `changeset`), 3 (`view(state, pinned)`, `pin`, `require_selected`), 5 (drop `label`, `with_label`) |
| `src/mcp/changesets.rs` | 2 (`open_changeset` tool), 3 (`publish`, `discard`, `revert`, `pending_changes` take `changeset`; drop `own_changeset`), 4 (`list_changesets`; drop `stranded_sessions`, `adopt_changeset`) |
| `src/http/mod.rs` (`AppError::status`: `UnknownChangeset` is 404) | 2 |
| `src/overlay.rs` (its test builds a `ChangesetRow` by hand) | 2 (`handle`), 5 (drop `label`) |
| `src/write.rs` | 3 |
| `src/mcp/writes.rs`, `src/mcp/restructure.rs` | 3 |
| `src/mcp/documents.rs`, `src/mcp/search.rs`, `src/mcp/reports.rs` | 3 |
| `src/selection.rs` | 5 |
| `src/db.rs` (its schema test inserts `label` and asserts `changesets_one_open_per_label`) | 5 |
| `src/mcp/testing.rs`, `src/app.rs` (test helpers) | 2, 3, 5 |
| `tests/support/server.rs`, `tests/writes.rs`, `tests/restructure.rs`, `tests/smoke.rs` | 1 (expected paths), 3, 4, 5 |
| `src/mcp/mod.rs` (router list, server instructions) | 2, 4 |
| `README.md`, `docs/operations.md`, design | 5 (README, operations, design sections), 7 (design "What M8 shipped differently", milestone row) |
| `Cargo.toml`, `Cargo.lock` | 5 (version 0.8.0) |
| `changelog/` | every task |

## Task dependencies

```
T1 (error paths) ─┐
T2 (handles) ─────┴─> T3 (writes, changeset tools, reads) ─> T4 (list_changesets) ─> T5 (remove labels, docs, 0.8.0) ─> T6 (release + rollout) ─> T7 (wrap-up)
```

- **T1 and T2 can start at once.** They share only `src/error.rs`, in different places (T1 edits `to_json`, T2 adds a variant and its `kind` and `to_json` arms); keep both sides.
- **T3 waits for T1** as well as T2, because T3 rewrites the tests whose expected error paths T1 changes. Serializing them avoids a painful merge.
- **T3 is one PR on purpose** (Brooke's review of the plan): writes and draft reads must switch to handles together, or the tests that read drafts back fail in between.
- **T4 waits for T3,** since both edit `src/mcp/changesets.rs`.

---

### Task 1: error payloads write `/`-prefixed catalog paths

Implements Decision 9 (the things-to-fix entry "okf_mcp's error payloads write paths in mixed forms"). M7 Task 1's changelog (`changelog/2026-11-03T191611Z-m7-task-01-still-linked-slash.md`, "Notes") lists each variant's path field and how it is written now.

**Files:**
- Modify: `src/error.rs` (`to_json`, a `client_path` helper, tests)
- Modify: tests that assert an error payload's `path` or a `LintFailed` finding's `file`. Find them with `grep -rn '"path"\]\|\["file"\]\|"file":' src tests`.
- Modify: the design's "Errors" section, one paragraph
- Create: `changelog/<ts>-m8-task-01-error-paths.md`

- [ ] **Step 1: Write the failing tests** in `src/error.rs`'s test module:

```rust
#[test]
fn to_json_writes_catalog_paths_with_a_leading_slash() {
    let not_found = OkfError::NotFound { path: "infra/x.md".to_string() };
    let already = OkfError::NotFound { path: "/infra/x.md".to_string() };
    let mismatch = OkfError::EditMismatch { path: "a.md".to_string(), matches: 0 };

    assert_eq!(not_found.to_json()["path"], "/infra/x.md");
    // Already prefixed: unchanged, never doubled.
    assert_eq!(already.to_json()["path"], "/infra/x.md");
    assert_eq!(mismatch.to_json()["path"], "/a.md");
}

#[test]
fn lint_failed_findings_name_their_file_with_a_leading_slash() {
    let finding = Finding {
        file: "a.md".to_string(),
        line: 3,
        rule: "broken-link".to_string(),
        message: "links to /b.md, which does not exist".to_string(),
    };
    let error = OkfError::LintFailed { errors: vec![finding.clone()], warnings: vec![finding] };

    let body = error.to_json();

    assert_eq!(body["errors"][0]["file"], "/a.md");
    assert_eq!(body["warnings"][0]["file"], "/a.md");
}

#[test]
fn invalid_path_is_prefixed_unless_it_leaves_the_catalog() {
    let invalid = |path: &str| OkfError::InvalidPath {
        path: path.to_string(),
        reason: "refused".to_string(),
    };

    // A normalized catalog path (machinery, "already exists").
    assert_eq!(invalid("AGENTS.md").to_json()["path"], "/AGENTS.md");
    assert_eq!(invalid("infra/x.md").to_json()["path"], "/infra/x.md");
    // A path that climbs out is not a catalog path: as given.
    assert_eq!(invalid("../x.md").to_json()["path"], "../x.md");
    assert_eq!(invalid("..").to_json()["path"], "..");
    // Not normalized, and climbs out once it is: as given, never `/infra/../../x.md`.
    assert_eq!(invalid("infra/../../x.md").to_json()["path"], "infra/../../x.md");
    // Empty: as given, not `/`.
    assert_eq!(invalid("").to_json()["path"], "");
}
```

- [ ] **Step 2: Run them** (`cargo test --lib error::tests`). All three fail on their unprefixed assertions (`"infra/x.md"` is not `"/infra/x.md"`). The assertions that already pass are `already` (it guards against doubling the slash) and the ones for paths kept as given.

- [ ] **Step 3: Implement.** In `to_json`:
  - write `client_path(path)` for `NotFound`, `FrontmatterParse`, `Shape`, `SecretDetected` and `EditMismatch`;
  - write `InvalidPath`'s `path` through `client_path` unless it is empty or `escapes_root(&normpath(path))` (`escapes_root` expects a normalized path), and `Io`'s unchanged. Split the shared `|` arm accordingly;
  - for `LintFailed`, serialize the findings through `findings_json`, which maps each finding's `file` through `client_path`.

  `Conflict` and `StillLinked` are already prefixed, and `client_path` leaves them unchanged.

```rust
/// A catalog path as clients see it: catalog-relative with a leading `/`
/// (`infra/x.md` becomes `/infra/x.md`; `/infra/x.md` stays). The one rule
/// for every path an error payload names (M8 Decision 9).
fn client_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Lint findings for an error body, each `file` as [`client_path`] writes it.
fn findings_json(findings: &[Finding]) -> serde_json::Value {
    let findings: Vec<Finding> = findings
        .iter()
        .map(|finding| Finding { file: client_path(&finding.file), ..finding.clone() })
        .collect();
    json_list(&findings)
}
```

  Update the doc comments of the affected fields in `OkfError`: they say "catalog-relative, no leading `/`". The internal value is unchanged; the payload is `/`-prefixed.

- [ ] **Step 4: Update the expected payloads in other tests.** Run the full gates and fix each assertion on an error body's `path` or finding `file`. Assertions on `error.to_string()` (the message) don't change.

  > **Corrected (issue #160), 2026-11-04.** Only `src/error.rs`'s own tests asserted unprefixed error-payload paths (`SecretDetected`, `EditMismatch` and a `LintFailed` warning); no test under `tests/` or elsewhere in `src/` did, so the full gate passed without other changes. The file map's "1 (expected paths)" for `tests/` turned out empty.

- [ ] **Step 5: The design's "Errors" section and the README.** Add: every catalog path in an error body (`path`, `files[].path`, `links[].source`, `errors[].file`, `warnings[].file`) is `/`-prefixed; an `InvalidPath` whose path climbs out of the catalog (`../x`) echoes it as given. Report results (`lint`, `oldest`, `sprawl`, `links_report`) keep `okf.py`'s unprefixed paths, so the same lint finding is `/a.md` in a `LintFailed` error and `a.md` in `lint`'s result, on purpose. The `Finding.file` doc comment (`src/okf/analysis/mod.rs:22`, "without a leading `/`") stays true for the struct; add that the error payload prefixes it.

- [ ] **Step 6: Changelog, gates, commit:** `fix: error payloads write catalog paths with a leading slash`. In the PR body, list the variants that changed and the ones left as they are.

---

### Task 2: changeset handles and `open_changeset`

**Files:**
- Create: `migrations/0003_changeset_handles.sql`
- Modify: `src/store/changesets.rs` (`ChangesetRow.handle`, `RawChangeset`, `changeset_columns!`, `open`, `by_handle`, tests)
- Modify: `src/error.rs` (`UnknownChangeset`: variant, `kind` arm, `to_json` arm, test)
- Modify: `src/mcp/context.rs` (`Pinned`, `ToolContext::changeset`, tests)
- Modify: `src/mcp/changesets.rs` (the `open_changeset` tool, its own `#[tool_router]` block, `open_router`)
- Modify: `src/mcp/mod.rs` (join `open_router` in `OkfServer::tools`; the tool-count test, if any, goes up by one)
- Modify: `src/http/mod.rs` (`AppError::status` gives `UnknownChangeset` 404, beside `CatalogNotSelected`; its status test)
- Modify: `src/overlay.rs` (its test's hand-built `ChangesetRow` gains `handle`)
- Create: `changelog/<ts>-m8-task-02-handles.md`

> **Corrected (issue #161), 2026-11-04.** As shipped:
> - `AppError::status` (`src/http/mod.rs`) matches `OkfError` with no `_` arm, so the new variant needs a status there too: 404, like `CatalogNotSelected` (a handle is a name that isn't there, or not for this key). `src/overlay.rs`'s test builds a `ChangesetRow` literal, which needs `handle` (and Task 5 must drop its `label`).
> - `is_internal` is a `matches!` of the internal variants, so `UnknownChangeset` needs no arm there.
> - The store module's fixture is `setup(&db.pool)` (catalog `pg`, keys `alice` and `bob`), and its tests already have an `open` helper (get-or-create in a transaction), which shadows the store's `open` under `use super::*`; the new test calls `super::open`. Step 2's code below is as shipped.
> - The `ToolContext::changeset` tests open their changeset through the `open_changeset` tool function (`crate::mcp::changesets::open_changeset`), so no new test helper was added. `src/mcp/changesets.rs` also has `open_changeset_is_listed_with_an_object_output_schema`.

- [ ] **Step 1: The migration.**

```sql
-- Changeset handles (M8, Decision 1): the server names each changeset with a
-- random handle that `open_changeset` returns and every changeset call passes
-- back, instead of a label the connection sends. A volatile default is
-- evaluated per row, so existing changesets get distinct handles too.
-- That rewrites the table under ACCESS EXCLUSIVE, on purpose: changesets is
-- a small bookkeeping table and migrations run at start-up.
ALTER TABLE changesets
    ADD COLUMN handle text NOT NULL DEFAULT gen_random_uuid()::text;
ALTER TABLE changesets ADD CONSTRAINT changesets_handle_key UNIQUE (handle);
```

  Changesets already open when this runs keep their labels (`default` and so on) and get fresh handles, but nothing hands those handles out until Task 4's `list_changesets`. In a development database, drafts left open when Task 3 merges can't be reached until Task 4 does. Publish or discard them before Task 3 merges. Production is unaffected: Tasks 2 to 5 ship together in 0.8.0, and the production catalog has no open changesets (claude.ai is read-only).

- [ ] **Step 2: Write the failing store tests** (`src/store/changesets.rs`, using the module's existing database fixture):

```rust
#[tokio::test]
async fn open_returns_a_fresh_handle_each_time_and_by_handle_finds_it() {
    let db = test_db().await;
    let pg = setup(&db.pool).await;
    let mut tx = db.pool.begin().await.unwrap();
    lock_catalog(&mut tx, pg).await.unwrap();

    // `super::open`: the test module's own `open` helper shadows the store's.
    let first = super::open(&mut tx, pg, "alice", "agent:a", now()).await.unwrap();
    let second = super::open(&mut tx, pg, "alice", "agent:a", now()).await.unwrap();
    tx.commit().await.unwrap();

    assert_ne!(first.handle, second.handle);
    assert_eq!(first.state, ChangesetState::Open);
    assert_eq!(by_handle(&db.pool, &first.handle).await.unwrap(), Some(first));
    assert_eq!(by_handle(&db.pool, "no-such-handle").await.unwrap(), None);
}
```

  The module's fixture is `setup` (see the correction note above).

- [ ] **Step 3: Implement.** Add `handle` to `RawChangeset`, `ChangesetRow`, `From<RawChangeset>` and the `changeset_columns!` macro. Then:

```rust
/// Opens a new changeset for `key_id` in a catalog (M8 Decision 1). Several
/// may be open at once for one key and catalog. Until the `label` column goes
/// (Task 5), the label is set to the handle, so the old one-open-per-label
/// index can never refuse it.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `catalog_id` - the catalog.
/// * `key_id` - the opening key.
/// * `actor` - its actor.
/// * `now` - the server clock's time.
///
/// # Returns
/// The new, open, empty changeset.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn open(
    conn: &mut PgConnection,
    catalog_id: i64,
    key_id: &str,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<ChangesetRow, OkfError> {
    // One statement: the CTE draws the handle once, and the insert writes it
    // to both columns.
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "WITH drawn AS (SELECT gen_random_uuid()::text AS handle) \
         INSERT INTO changesets (catalog_id, key_id, label, handle, actor, state, \
         created_at, updated_at) \
         SELECT $1, $2, drawn.handle, drawn.handle, $3, 'open', $4, $4 FROM drawn \
         RETURNING ",
        changeset_columns!()
    ))
    .bind(catalog_id)
    .bind(key_id)
    .bind(actor)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.into())
}

/// A changeset by its handle, in any state.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn by_handle<'e>(
    executor: impl PgExecutor<'e>,
    handle: &str,
) -> Result<Option<ChangesetRow>, OkfError> {
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "SELECT ",
        changeset_columns!(),
        " FROM changesets WHERE handle = $1"
    ))
    .bind(handle)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(ChangesetRow::from))
}
```

- [ ] **Step 4: `UnknownChangeset`.** Add it to `src/error.rs`:

```rust
/// No changeset with this handle that the key may use: it does not exist,
/// belongs to another key, or is in a catalog the key does not reach (worded
/// alike, so a key cannot probe for other keys' handles).
#[error("{changeset}: no such changeset; open one with open_changeset, or see list_changesets")]
UnknownChangeset {
    /// The handle as given.
    changeset: String,
},
```

  Its `kind()` is `"UnknownChangeset"`, and it is not internal. `to_json` adds `body["changeset"]`. Add a test, `unknown_changeset_names_the_handle`, beside `to_json_names_the_kind_and_what_to_act_on`.

- [ ] **Step 5: `ToolContext::changeset`.** Write the failing tests in `src/mcp/context.rs`. Use the module's existing helpers that make a writer, a reader and an admin on catalog `pg`, plus a second key:
  - `a_handle_resolves_for_its_key_and_an_admin_only`: open a changeset with the writer's key. The writer resolves it with `Access::Write`. The admin resolves it. Another writer key gets `UnknownChangeset`. A made-up handle gets `UnknownChangeset`, with the same message apart from the handle.
  - `a_closed_changeset_is_refused_by_name`: after `set_state(.., Discarded, ..)`, resolving gives `Refused` whose reason contains the handle and `discarded`.
  - `a_read_key_resolves_nothing`: a `read` key can't own a changeset, so a writer's handle is `UnknownChangeset` for it.

  Then implement:

```rust
/// A changeset a call acts on, and its catalog.
#[derive(Debug, Clone)]
pub struct Pinned {
    /// The changeset's catalog.
    pub state: Arc<CatalogState>,
    /// The changeset, as read when the call resolved it (not locked).
    pub row: ChangesetRow,
}

impl ToolContext {
    /// Resolves a changeset handle for this call (M8 Decisions 1 and 3).
    ///
    /// # Arguments
    /// * `handle` - the call's `changeset` argument.
    /// * `access` - what the call needs on the changeset's catalog.
    ///
    /// # Returns
    /// The open changeset and its catalog.
    ///
    /// # Errors
    /// - [`OkfError::UnknownChangeset`] for a handle that does not exist, is
    ///   in a catalog this key does not reach (or that is not loaded), or
    ///   belongs to another key while this one is not `admin`;
    /// - [`OkfError::Forbidden`] below `access`;
    /// - [`OkfError::Refused`] for a changeset that was published or discarded
    ///   (a discarded one with the 24-hour hint);
    /// - [`OkfError::Database`].
    pub async fn changeset(&self, handle: &str, access: Access) -> Result<Pinned, OkfError> {
        let unknown = || OkfError::UnknownChangeset { changeset: handle.to_string() };
        let row = changesets::by_handle(self.app.pool(), handle)
            .await?
            .ok_or_else(unknown)?;
        // The map is keyed by name; a handful of catalogs, so a scan is fine.
        let state = self
            .catalogs
            .values()
            .find(|state| state.id() == row.catalog_id)
            .map(Arc::clone)
            .ok_or_else(unknown)?;
        // Ownership is checked before `require(access, ..)`, on purpose: a key
        // that neither owns the changeset nor is `admin` gets
        // `UnknownChangeset`, never `Forbidden`, so it cannot tell a real
        // handle from a made-up one (Decision 3). So a `read` key, which owns
        // none, never resolves one, and `access` only matters for owners and
        // admins (`pin` and `pending_changes` pass `Access::Read`). Don't
        // reorder these checks.
        let own = row.key_id.as_deref() == Some(self.principal.key_id.as_str());
        let admin = self.principal.access == Access::Admin;
        if !self.principal.reaches(state.name()) || !(own || admin) {
            return Err(unknown());
        }
        self.principal.require(access, state.name())?;
        // Only a discarded changeset gets the 24-hour hint (Brooke's review
        // of PR #167): a published one was never pruned.
        let hint = match row.state {
            ChangesetState::Open => return Ok(Pinned { state, row }),
            ChangesetState::Published => "",
            ChangesetState::Discarded => {
                " (a changeset with no edits is discarded after 24 hours)"
            }
        };
        Err(OkfError::Refused {
            reason: format!(
                "changeset {handle} is {}; open a new one with open_changeset{hint}",
                row.state.as_str()
            ),
        })
    }
}
```

  If `Catalogs` doesn't expose `values()`, use its iteration method. Correct this plan if the name differs.

- [ ] **Step 6: `open_changeset`.** In `src/mcp/changesets.rs`:

```rust
/// `open_changeset`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct OpenParams {
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `open_changeset`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Opened {
    /// The new changeset's handle: pass it as `changeset` to every write, `publish`, `discard`, `revert` and `pending_changes`, and to reads that should see the drafts.
    pub changeset: String,
    /// Its catalog.
    pub catalog: String,
}
```

  The function resolves `ctx.one(params.catalog.as_deref())` and requires `Access::Write`. Then it begins a transaction, runs `changesets::lock_catalog` and `changesets::open`, and commits. The tool description: "Open a changeset in a catalog and return its handle. Every edit, `publish`, `discard`, `revert` and `pending_changes` needs a handle, and the reads take one to see its drafts. Nothing is visible to anyone else until `publish`. Several changesets may be open at once. A changeset with no edits is discarded after 24 hours. Write access."

  Its test is `open_changeset_returns_a_handle_that_resolves`: call it, then resolve the handle with `ctx.changeset(.., Access::Write)`.

- [ ] **Step 7: Gates, changelog, commit:** `feat: server-issued changeset handles and open_changeset`. Labels still work alongside the handles: the label path is untouched until Task 3.

---

### Task 3: writes, the changeset tools and the reads take `changeset`

Every write tool, `publish`, `discard`, `revert` and `pending_changes` acts on the changeset its required `changeset` argument names. It no longer uses the connection's label. The write tools lose `catalog` (Decision 5). The read tools take an optional `changeset` in the same PR (Decision 6). Writes and draft reads have to switch together: once writes land in handle-opened changesets, the label lookup in `ToolContext::view` no longer finds them, so the tests that read drafts back would fail in between.

**Files:**
- Modify: `src/write.rs` (`apply`'s signature and body, `WriteReport`, tests)
- Modify: `src/mcp/writes.rs`, `src/mcp/restructure.rs` (every `*Params`: add `changeset`, remove `catalog`; handlers; tool descriptions; tests)
- Modify: `src/mcp/changesets.rs` (`PublishParams`, `DiscardParams`, `RevertParams`, `pending_changes`' params; `Published`, `Discarded`, `PendingChanges`; drop `own_changeset`; tests)
- Modify: `src/store/changesets.rs` (drop `get_or_create`, `find_open`, `ChangesetKey`, `GET_OR_CREATE_TRIES` and their tests; rewrite `prune_empty`'s doc comment, `src/store/changesets.rs:655-659`, whose `SKIP LOCKED` argument goes through `get_or_create`. It now relies on: the write resolves the handle unlocked, then takes `lock`, which waits for a prune that holds the row and then re-reads its state, so the write sees `discarded` and is refused; a write that took the row first makes the prune skip it)
- Modify: `src/mcp/context.rs` (`view(state, pinned)`, a `pin` helper, `require_selected`, tests; the draft-read tests become handle-based)
- Modify: `src/mcp/documents.rs` (`GetConceptParams`, `ReadDocumentParams`, `view_or_note`)
- Modify: `src/mcp/search.rs` (`SearchParams`, `run`)
- Modify: `src/mcp/reports.rs` (`CatalogParam` for `open_questions`, `OldestParams`, `SprawlParams`, `LinksParams`; `LintParams`'s `pending` becomes `changeset`; `one_view`, `lint_report`; `Lint.pending` stays as the result field)
- Modify: `src/mcp/testing.rs`, `src/app.rs` (test helpers that open changesets: use `changesets::open`)
- Modify: `tests/support/server.rs` (an `open_changeset` helper), `tests/writes.rs`, `tests/restructure.rs`, `tests/smoke.rs`
- Create: `changelog/<ts>-m8-task-03-changeset-arguments.md`

- [ ] **Step 1: Write the failing tests** in `src/write.rs`'s tests:
  - `a_write_lands_in_the_named_changeset_only`: open two changesets with one key and write to each. Each changeset's entries hold only its own edit.
  - `a_write_to_a_closed_changeset_is_refused`: discard a changeset, then write to its handle. The result is `Refused` naming the handle, and nothing is stored. This replaces the untested defensive `Busy` branch (an M5 gap): `ctx.changeset` refuses a changeset that is already closed. A changeset that closes while the write waits for its row lock is refused the same way, after `lock`.
  - `another_key_cannot_write_to_a_handle`: `UnknownChangeset`.
  - `a_write_waiting_on_a_changeset_that_closes_is_refused`: the race the new `prune_empty` comment relies on. A second transaction takes the changeset's row (`changesets::lock`), standing in for a prune or discard that holds it. Spawn a write (`tokio::spawn`). Wait until it is blocked on that row: poll `pg_locks` for a lock not granted on `changesets`, or `pg_stat_activity.wait_event_type = 'Lock'`, with a timeout. Then set the state to `discarded` in the second transaction and commit. The write returns `Refused` with "was published or discarded while this write waited", and the changeset has no entries. This is the post-`lock` branch, which was the untested defensive path.

  And in `tests/writes.rs`:
  - rename `x_okf_changeset_keeps_two_changesets_of_one_key_apart` to `two_handles_of_one_key_stay_apart`. Open two changesets on one connection, edit through both, and publish one. The other is still pending;
  - `a_changeset_survives_a_server_restart`: reconnect, and keep working with the same handle;
  - `concurrent_writes_from_two_connections_of_one_key_share_one_changeset` becomes `concurrent_writes_to_one_handle_from_two_connections_both_land`;
  - `a_write_without_a_changeset_is_a_parameter_error`: call `stamp` without `changeset`. The client gets an `isError` result (not a JSON-RPC error; see the correction in "Verified facts") whose text is `failed to deserialize parameters: missing field \`changeset\``.

- [ ] **Step 2: `write::apply`.** The new signature:

```rust
pub async fn apply<F>(
    ctx: &ToolContext,
    changeset: &str,
    by: Option<&str>,
    op: F,
) -> Result<WriteReport, OkfError>
```

  The body:
  1. `let Pinned { state, row } = ctx.changeset(changeset, Access::Write).await?;`
  2. Begin the transaction and run `changesets::lock_catalog(&mut tx, state.id())`.
  3. Lock the changeset: `changesets::lock(&mut tx, row.id)`. Handle its two outcomes apart:
     - `None`: the row is gone, because its catalog was deleted (the cascade). `lock_catalog` normally refuses first, so this is only a backstop. Return `Refused` with `changesets::CATALOG_GONE`;
     - a row that isn't open: return `Refused` with `format!("changeset {changeset} was published or discarded while this write waited; open a new one with open_changeset")`.
  4. Read the overlay and entries, run the op, and store the entries, exactly as now.

  `WriteReport` becomes `{ catalog, changeset: String /* handle */, touched, message, notes }`.

- [ ] **Step 3: The write tools.** In each `*Params` of `src/mcp/writes.rs` and `src/mcp/restructure.rs`, replace the `catalog` field with:

```rust
/// The changeset to write to: the handle `open_changeset` returned.
pub changeset: String,
```

  Pass `&changeset` to `apply`. In every tool description, replace "in this connection's changeset" with "in the changeset `changeset` names (open one with `open_changeset`)".

- [ ] **Step 4: The changeset tools.**
  - `publish(changeset, message)`: `ctx.changeset(&params.changeset, Access::Write)`, then `store::publish::publish(app, &pinned.state, &pinned.row, actor, message)`. `Published` becomes `{ catalog, changeset: String, report }` (no `label`). The blank-message check comes first, as now.
  - `discard(changeset, paths?)`: the same lookup with `Write`. `Discarded` becomes `{ catalog, changeset: String, removed, closed }`. Its description changes: discarding every entry closes the changeset for good, and its handle is then refused, so call `open_changeset` again for new work. Before, the next write opened a new changeset by itself. The README says the same.
  - `revert(changeset, path, revision)`: goes through `write::apply` with the handle.
  - `pending_changes(changeset)`: resolves with `Access::Read`. The key's own and an admin's resolve; a `read` key never owns one. `PendingChanges` becomes `{ catalog, changeset: String, files }`. Its params are a new `PendingParams { changeset: String }`, because `CatalogParam` stays for `open_questions`.
  - Remove `own_changeset` and `OwnChangeset`.

- [ ] **Step 5: The integration harness.** Add a helper to `tests/support/server.rs`:

```rust
/// Opens a changeset through the tool and returns its handle.
pub async fn open_changeset(client: &Client, catalog: Option<&str>) -> String
```

  It calls `open_changeset` and reads `structured_content["changeset"]`. Then pass `"changeset": handle` in every write and `publish` call in `tests/writes.rs`, `tests/restructure.rs` and `tests/smoke.rs`, and drop their `X-OKF-Changeset` headers.

- [ ] **Step 6: Reads.** Write the failing tests first (`src/mcp/context.rs`):
  - `a_read_without_a_changeset_is_published_even_for_a_writer_with_drafts`: a writer stages an edit. `view(&state, None)` is `Published`, and `view(&state, Some(&pinned))` is `Pending`.
  - `a_pinned_changeset_applies_only_to_its_own_catalog`: with two catalogs and a changeset in `pg`, `view(&work, Some(&pinned))` is `Published`.

  And in `src/mcp/search.rs` or `src/mcp/documents.rs`:
  - `get_concept_with_a_changeset_sees_its_draft`;
  - `a_changeset_outside_the_selected_catalogs_is_refused`: `search` with `catalogs: ["work"]` and a `pg` handle gives `Refused`, naming `pg`;
  - `get_concept_refuses_a_changeset_outside_its_read_set`: on a connection selecting only `work` (no `catalog` argument, so `get_concept` walks the read set), a `pg` handle gives the same `Refused`;
  - `lint_counts_only_the_pinned_changeset_s_paths_as_uncommitted`: with a draft of `a.md` in a changeset, `lint` with that handle doesn't report `generated-after-commit` for `a.md`, and `lint` without a handle lints the published catalog with its commit times, as before. Lint's "paths in the connection's changeset count as uncommitted" becomes "the pinned changeset's paths count as uncommitted, and without `changeset` none do". That is the overlay's `commit_times` (`Overlay::lint`), unchanged.

  Then implement:

```rust
impl ToolContext {
    /// The call's changeset, if it named one (read tools: `read` access).
    ///
    /// # Errors
    /// As [`ToolContext::changeset`].
    pub async fn pin(&self, handle: Option<&str>) -> Result<Option<Pinned>, OkfError> {
        match handle {
            None => Ok(None),
            Some(handle) => self.changeset(handle, Access::Read).await.map(Some),
        }
    }

    /// What this call reads of `state`: the pinned changeset's overlay if it
    /// is in this catalog, else the published state (M8 Decision 6).
    ///
    /// # Errors
    /// An overlay build error ([`crate::overlay::build_overlay`]); [`OkfError::Database`].
    pub async fn view(&self, state: &CatalogState, pinned: Option<&Pinned>) -> Result<View, OkfError> {
        match pinned.filter(|pinned| pinned.state.id() == state.id()) {
            None => Ok(View::Published(state.current())),
            Some(pinned) => {
                let overlay = self.app.overlays().get(self.app.pool(), state, &pinned.row).await?;
                Ok(View::Pending(overlay))
            }
        }
    }
}

/// Refuses a pinned changeset whose catalog the call does not select.
///
/// # Errors
/// [`OkfError::Refused`] naming the changeset's catalog.
pub fn require_selected(pinned: Option<&Pinned>, states: &[Arc<CatalogState>]) -> Result<(), OkfError> {
    // `let ... else`: without a pinned changeset there is nothing to check.
    let Some(pinned) = pinned else {
        return Ok(());
    };
    if states.iter().any(|state| state.id() == pinned.state.id()) {
        Ok(())
    } else {
        Err(OkfError::Refused {
            reason: format!(
                "changeset {} is in catalog {:?}, which this call does not read",
                pinned.row.handle,
                pinned.state.name()
            ),
        })
    }
}
```

  `pinned.row` is the unlocked row read when the handle resolved. `pin` reads it fresh on every call, as `find_open` does today, and `Overlays::get` matches its cache to that row's `version` and the catalog's generation (`src/overlay.rs:235-252`). Every write, publish and discard also drops the cached overlay (`forget`). A write that commits between `pin` and `view` therefore leaves the same window as today: this call may still read the version before it, and the next call rebuilds. It can't leave a stale overlay cached under the newer version. Keep `forget` in the write path.

  Each read tool adds this to its params:

```rust
/// Read through this changeset's drafts (its handle); its catalog must be one this call reads.
pub changeset: Option<String>,
```

  It calls `ctx.pin(params.changeset.as_deref())`, then `require_selected`, then passes the pinned changeset to every `view` call. The reports that run on one catalog (`open_questions`, `oldest`, `sprawl`, `links_report`, `lint`) default to the pinned changeset's catalog when the call names no `catalog`, rather than to the first selected one. That way a `pg` handle on a `work,pg` connection lints `pg` instead of being refused. An explicit `catalog` that differs from the changeset's is refused by `require_selected`, as for any other catalog the changeset isn't in. The test is `a_report_defaults_to_the_pinned_changeset_s_catalog`. The multi-catalog reads (`search`, and `get_concept`/`read_document` without `catalog`) keep their read set; the changeset applies to its own catalog within it. `lint` with `changeset` lints that overlay, and `Lint.pending` is true then.

  Rewrite the existing tests that read drafts back (`src/mcp/context.rs`, `src/mcp/documents.rs`, and the read-after-write flows in `tests/writes.rs`) to pass the handle, and give every other `ctx.view(&state)` call `None`.

- [ ] **Step 7: Gates, changelog, commit:** `feat!: writes, the changeset tools and the reads take a changeset handle`. The changelog lists every tool whose parameters changed.

> **Corrected (issue #162), 2026-11-04.** As shipped:
> - **The parameter error** is an `isError` tool result, not a JSON-RPC error (see "Verified facts"); the test asserts its text.
> - **Reports:** the plan's `one_view` default ("the pinned changeset's catalog when the call names no `catalog`") is `ToolContext::report_catalog(catalog, pinned)`, which also runs `require_selected` on the one catalog. The pinned catalog stands in for a `catalog` argument, which wins over the connection's catalogs, so it needs only the key's reach: a `pg` handle on a `work`-only connection reports on `pg` (asserted in `a_report_defaults_to_the_pinned_changeset_s_catalog`; Brooke's review of PR #169). `lint_report` goes through the same `one_view` as the other reports: with `changeset` its view is the overlay (`Lint.pending: true`), without it the published state. `CatalogParam` (now only `open_questions`') gains `changeset` too. `documents.rs` has a small `read_set(ctx, catalog, changeset)` that does `read_set`, `pin` and `require_selected` for `get_concept` and `read_document`.
> - **The overlay's errors name the handle**, not the bigint id (`overlay::in_changeset`): "in pending changeset <handle>; discard ...". A client never sees the id any more (`stranded_sessions` and `adopt_changeset` still do until Task 4).
> - **`revert`** resolves the handle (with `write`) to find the catalog whose history it reads, then goes through `apply` with the handle, which resolves it again under its locks.
> - **`stranded_sessions`** no longer leaves out "this connection's own" changeset (there is none: it was the one with the connection's label). **`adopt_changeset`** drops its `find_open` check. It still relabels to the connection's label, so adopting a second changeset onto one connection hits the store's one-open-per-label index, which `changesets::adopt` already reports as `Refused` ("already has an open changeset"). Both tools go in Task 4.
> - **Test helpers** (Interfaces block): `testing::open`, `open_at`, `changeset_row`, and `pending_entry(test, handle, edit)`. `lifecycle_tests::edit_hours_ago(test, ctx, hours_ago)` now opens its own changeset with `open_at` (created and changed `hours_ago` ago) and returns its id. The `writes`/`restructure`/`changesets` test modules pass a handle to their helpers; `restructure_publish_tests` wraps a writer's context and handle in a `Session`.
> - **README:** the "Changesets" section, the `lint`, `pending_changes` and `discard` rows and the read-tools paragraph describe handles now (Step 4 asks for the `discard` text). The label rules and the header example went with it; Task 5 still rewrites the rest (`stranded_sessions`, `adopt_changeset`, the operations guide, the design).
> - **`tests/writes.rs`'s restart test** also writes through the handle after the restart, then publishes.
> - The **`ctx.label()`** is now used only by `adopt_changeset` (relabel) and the context's own tests.

---

### Task 4: `list_changesets` replaces `stranded_sessions` and `adopt_changeset`

**Files:**
- Modify: `src/store/changesets.rs` (`list_open`; remove `stranded`, `StrandedRow`, `RawStranded`, `adopt` and their tests)
- Modify: `src/mcp/changesets.rs` (the `list_changesets` tool; remove `stranded_sessions`, `adopt_changeset`, their params, results and tests)
- Modify: `src/mcp/mod.rs` (routers; the server instructions text, if it names the removed tools)
- Modify: `tests/writes.rs` (any test of the removed tools)
- Create: `changelog/<ts>-m8-task-04-list-changesets.md`

- [ ] **Step 1: Write the failing tests:**
  - `list_changesets_shows_a_key_its_own_and_an_admin_everyone_s`: two writers each open one changeset, one empty and one with an edit made 3 hours ago (use the existing `edit_hours_ago` helper). Writer A sees only its own. An admin sees both, each with its `paths`, and the old one with `idle: true` (the test config's `session_idle_timeout` is shorter than 3 h).
  - `an_admin_may_publish_another_key_s_changeset`: the revisions carry the admin's actor, as `publish` records the publishing key's actor today.
  - `a_published_changeset_is_no_longer_listed`.

- [ ] **Step 2: Implement.** The store query: open changesets in the given catalogs, optionally only one key's, with their sorted paths. It follows the `stranded` query's shape, without its idle and non-empty filters, ordered by catalog then `updated_at`:

```rust
/// An open changeset as `list_open` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRow {
    /// The changeset (its handle, catalog id, key, actor, times).
    pub changeset: ChangesetRow,
    /// Its catalog's name (joined from `catalogs`).
    pub catalog: String,
    /// Its entries' paths, catalog-relative and sorted (`array_agg(path ORDER BY path)`,
    /// empty for an empty changeset: `COALESCE(..., '{}')` over a `LEFT JOIN`).
    pub paths: Vec<String>,
}

pub async fn list_open(
    pool: &PgPool,
    catalog_ids: &[i64],
    key_id: Option<&str>,
) -> Result<Vec<ListedRow>, OkfError>
```

  It is `StrandedRow` renamed, with the same `#[sqlx(flatten)]` raw form. The tool adds the `/` to each path.

  The tool lists the catalogs the key reaches (or the one named). A non-admin key passes its own `key_id`, and an admin passes `None`. Each row is:

```rust
pub struct ListedChangeset {
    /// Its handle.
    pub changeset: String,
    pub catalog: String,
    /// The key it belongs to; null once that key is deleted.
    pub key: Option<String>,
    pub actor: String,
    pub created: String,
    pub last_change: String,
    /// No change within `session_idle_timeout`.
    pub idle: bool,
    /// Its changed paths, `/`-prefixed and sorted.
    pub paths: Vec<String>,
}
```

  `idle_cutoff` moves over from `stranded_sessions`. The tool description: "List the open changesets this key may use (an admin: every key's, in the catalogs it reaches), with their handles, paths and last change; `idle` means nothing changed within the idle timeout. Use it to find a handle again after a reconnect or a lost context. Read access."

- [ ] **Step 3: Gates, changelog, commit:** `feat!: list_changesets replaces stranded_sessions and adopt_changeset`.

> **Corrected (issue #163), 2026-11-04.** As shipped:
> - **The query** needs `array_agg(e.path ORDER BY e.path) FILTER (WHERE e.path IS NOT NULL)` inside the `COALESCE`: over a `LEFT JOIN`, an empty changeset's one joined row has a `NULL` path, and a bare `array_agg` gives `{NULL}`, not `NULL`. The catalogs are bound as an array (`c.catalog_id = ANY($1)`). Its `SELECT` names the changeset's columns by hand, `c.label` among them (the `changeset_columns!` macro's bare names would clash with `catalogs.id` in the join), so Task 5 drops `c.label` there too.
> - **The tool's description** also says that empty changesets are listed, and that one with no write within 24 hours is discarded and drops off the list (Brooke's review of PR #170).
> - **The tool** takes `ListParams { catalog: Option<String> }` (only that catalog; `ctx.one` refuses one the key doesn't reach) and returns `Listed { changesets: Vec<ListedChangeset> }`.
> - **Tests:** `list_changesets_shows_a_key_its_own_and_an_admin_everyone_s` is in `src/mcp/changesets.rs`'s `lifecycle_tests`, whose `edit_hours_ago` now returns the handle. `an_admin_may_publish_another_key_s_changeset` and `a_published_changeset_is_no_longer_listed` are in its `publish_tests`, which have publishable fixtures. The store has `list_open_lists_open_changesets_with_their_sorted_paths`. `tests/writes.rs` had no test of the removed tools, so it is unchanged.
> - **Docs:** the README's `stranded_sessions` and `adopt_changeset` rows became `list_changesets` rows, and the `session_idle_timeout` comments in the README and `docs/operations.md` say "idle" for "stranded". The server instructions name `list_changesets` as the way to find a lost handle.
> - After this task, **`ctx.label()`** is used only by the context's own tests, and the lifecycle tests' `connection` helper no longer calls `with_label`.

---

### Task 5: remove labels; README, operations guide, design; 0.8.0

**Files:**
- Create: `migrations/0004_drop_changeset_labels.sql`
- Modify: `src/selection.rs` (remove `CHANGESET_HEADER`, `CHANGESET_QUERY`, `DEFAULT_LABEL`, `label_text`, `header_label`, `connection_label` and their tests)
- Modify: `src/mcp/context.rs` (remove `label`, `label()`, `with_label`, and their tests)
- Modify: `src/store/changesets.rs` (remove `label` from the rows, the columns macro and `open`'s insert)
- Modify: `src/db.rs` (its schema test inserts `label` and asserts the constraint `changesets_one_open_per_label`; afterwards it asserts that a duplicate `handle` violates `changesets_handle_key`)
- Modify: `src/mcp/testing.rs`, `src/app.rs`, `tests/support/server.rs` (any `label` left over)
- Modify: `README.md` ("Writing", "Changesets", the `mcp.json` example), `docs/operations.md` (any header mention)
- Modify: the design: Decision "Write grouping", "Write tools" (the `pending_changes`, `stranded_sessions` and `adopt_changeset` rows, and the new `open_changeset` and `list_changesets` rows), "Data flow" Write step 2 and Publish, "Session lifecycle", and the tool parameters in the read-tool table
- Modify: `Cargo.toml` (`version = "0.8.0"`), `Cargo.lock`
- Create: `changelog/<ts>-m8-task-05-remove-labels.md`

- [ ] **Step 1: The migration.**

```sql
-- Changesets are named by handle only (M8): the connection label and its
-- one-open-per-label index go. Dropping the column would drop the index
-- with it; dropping the index first says so.
DROP INDEX changesets_one_open_per_label;
ALTER TABLE changesets DROP COLUMN label;
```

- [ ] **Step 2: Remove the code**, so that `grep -rn 'label' src` finds only unrelated uses: link labels in `src/okf/markdown.rs`, `split.rs` and lint. Also remove `open`'s `label` column from its insert. `cargo build` guides the rest.

- [ ] **Step 3: A regression test** `a_changeset_header_is_ignored` (`tests/writes.rs`). It already passes after Task 3, and it keeps the header from coming back: a client that sends `X-OKF-Changeset: anything` still needs `changeset` arguments. Its write without a handle is a parameter error, and with one it lands in that changeset.

- [ ] **Step 4: Docs.**
  - **README "Writing":** open a changeset, pass its handle to every edit, `publish` with a message. `list_changesets` finds a lost handle, and an admin may use any key's handle. Remove the label rules and the `X-OKF-Changeset` example.
  - **`docs/operations.md`:** remove any mention of the header.
  - **The design:** rewrite the sections listed under Files to describe handles. Add "Revision: changeset handles (2026-11-03)" pointing at this plan. Everything stays generic.

- [ ] **Step 5: Version 0.8.0.** Bump `Cargo.toml`, then run `cargo build` to update `Cargo.lock`.

- [ ] **Step 6: Gates, changelog, commit:** `feat!: drop changeset labels; docs for changeset handles; 0.8.0`.

> **Corrected (issue #164), 2026-11-04.** As shipped:
> - **`changesets::open`** inserts without `handle` and lets the column's default (`gen_random_uuid()::text`, migration 0003) draw it; the CTE that wrote one value to `label` and `handle` is gone. `list_open`'s hand-written `SELECT` lost `c.label` too.
> - **`src/store/catalogs.rs`'s tests** also inserted `label` (three raw `INSERT`s); they lose it. `src/mcp/testing.rs`, `src/app.rs` and `tests/support/server.rs` had nothing left to remove.
> - **`src/db.rs`'s schema test** inserts two open changesets of one key in one catalog (allowed now, Decision 7), then a duplicate handle, which violates `changesets_handle_key`.
> - **`publish`'s "no longer open" refusal** (`src/store/publish.rs`) named the changeset's internal id; it names the handle.
> - **`a_changeset_header_is_ignored`** also checks that `pending_changes` with the header's value as a handle is `UnknownChangeset`. It shares `assert_missing_changeset` with `a_write_without_a_changeset_is_a_parameter_error`.
> - **The design:** `list_changesets` needs only `read`, so it takes `stranded_sessions`' row in the read-tool table; `open_changeset` heads the write-tool table. The `changesets` storage row, its constraints, the overlay paragraph and the tools' introduction changed too.
> - **`docs/operations.md`** gained a Troubleshooting row for `UnknownChangeset` and closed changesets, and its image examples say `0.8.0`.

---

### Task 6: release and rollout of 0.8.0

This task changes other systems. Its PR in this repo carries only its changelog, which names nothing site-specific, plus corrections to `docs/operations.md` or this plan if the rollout shows any are needed.

**Every outward step waits for pgrace's go-ahead in the session.** The steps that create or handle secret values (a write key for the check) are run by pgrace, from commands you give.

- [ ] **Step 1: Preconditions.** T1 to T5 are merged, `main` is green on the gates, `Cargo.toml` says `0.8.0`, and there is no `v0.8.0` tag yet.

- [ ] **Step 2: Image (go-ahead 1).** Run `gh workflow run image.yml --ref main` and watch it. Then tag `v0.8.0` on `main` and push the tag. Check that the image pulls through the cluster's cache, as in M7 Task 6.

- [ ] **Step 3: Backup, then deployment (go-ahead 2).** 0.8.0 is forward-only. Migration 0004 drops `changesets.label`, and under the Deployment's `Recreate` strategy no 0.7.0 pod runs against the new schema. Going back to 0.7.0 means restoring the database from a backup taken before the upgrade (`docs/operations.md`, section 8, "Upgrades"). So before go-ahead 2, take an on-demand backup of the database (as in M7's restore drill), wait for it to complete, and record its name in the changelog. Say this to pgrace when asking for the go-ahead. Then: In a fresh checkout of the site's Argo CD repository, change only the Deployment's image tag from `0.7.0` to `0.8.0`. Show pgrace the diff, commit, and push. Watch the app become Synced/Healthy, and the pod log the migrations (0003, 0004) on start.

- [ ] **Step 4: Checks.**
  - `/healthz`: database ok, and `clock` ok.
  - `tools/list` names `open_changeset` and `list_changesets`, and doesn't name `stranded_sessions` or `adopt_changeset`.
  - Ask pgrace to run a round trip with a `write` key on `pg`, using commands you give: `open_changeset`, `stamp` on a concept, `pending_changes` (shows the diff), `discard` (closes it), then `list_changesets` (empty). Nothing is published to `pg`, so it stays a copy of git.
  - claude.ai still reads: pgrace asks it one question.

- [ ] **Step 5: Changelog** `changelog/<ts>-m8-task-06-rollout.md`, with what was done and checked. Then commit, open the PR, and get it reviewed.

---

### Task 7: wrap-up: milestone changelog, design, smoke test

**Files:**
- Create: `changelog/<ts>-m8-changeset-handles.md` (the milestone summary)
- Modify: the design: the M8 row in "Milestones" (delivered, with PRs), and "What M8 shipped differently" (every `Corrected` note of this plan)
- Modify: this plan (a self-review note if needed)

- [ ] **Step 1: The release smoke test** (`cargo test --release --test smoke -- --ignored`) against the real catalog. Record its commands and output in the milestone changelog.
- [ ] **Step 2: The milestone changelog:** what changed for clients (tools, parameters, results), the migration, the decisions, the divergences, and the follow-ups.
- [ ] **Step 3: The design's milestone row and "What M8 shipped differently".**
- [ ] **Step 4: Outside this repo** (the coordinator does this, not the PR): delete `~/.claude/okf-mcp-changeset.sh` (Decision 11). Update the catalog's okf-mcp concept: the write workflow with handles, the M8 close-out, and remove the error-path entry from things-to-fix.
- [ ] **Step 5: Gates, commit:** `docs: M8 wrap-up: README, design, milestone changelog`.

## Self-review (2026-11-03)

> **Corrected (PR #158 review), 2026-11-03.** The first draft had 8 tasks, with reads in their own Task 4. Brooke pointed out that Task 3's gates couldn't pass that way: once writes land in handle-opened changesets, the label lookup in `view` no longer finds drafts. Reads now switch in Task 3, and Tasks 5 to 8 became 4 to 7.

- **Coverage.** Each item maps to a task:
  - Decisions 1 to 3: handles, refusal without a handle, ownership. Tasks 2, 3, 4.
  - Decisions 4 and 5: handle form, catalog from the changeset. Tasks 2, 3.
  - Decision 6: reads. Task 3.
  - Decision 7: several open per key. Tasks 2, 5.
  - Decision 8: `list_changesets`. Task 4.
  - Decision 9: error paths. Task 1.
  - Decision 10: release. Tasks 5, 6.
  - Decision 11: the helper script. Task 7.
- **Placeholders.** The tests in Tasks 3 and 4 are named and specified by behaviour rather than written out in full, because they reuse each module's existing fixtures (`test_app`, `edit_hours_ago`, `writer`). Their assertions are stated exactly. The code for the new functions is given.
- **Consistency.** `Pinned`, `ToolContext::changeset`, `pin`, `view(state, pinned)`, `changesets::open`, `by_handle`, `list_open`, `ListedRow`, `ListedChangeset`, `UnknownChangeset`, and `changeset: String` are used with the same names and types throughout.
