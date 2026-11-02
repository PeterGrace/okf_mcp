# OKF MCP Server, Milestone 3 (Storage): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Store catalogs in Postgres: schema and migrations, import from a directory or tarball, export as a tarball, `catalog` and `key` administration, and an in-memory snapshot per catalog that is rebuilt and swapped when the catalog changes, with lint reading revision times. Export then import reproduces the real catalog's file contents and modes byte for byte.

**Architecture:** A `db` module owns the pool and the embedded migrations. `store::{catalogs, keys, documents, import, export}` are async functions over a `PgPool` (runtime-checked `sqlx::query`), each taking "now" from the caller so every timestamp comes from the server clock. `archive` reads and writes `.tar.gz` and reads directories, applying the design's import rules; it is pure. `catalog::CatalogState` holds an `RwLock<Arc<Loaded>>` built from a catalog's rows through the in-memory workspace (which now holds bytes and execute bits) and M2's unchanged `Snapshot::build`. A `clap` CLI (`okf-mcp migrate | catalog | key | import | export`) wires it together; `serve` arrives in M4.

**Tech Stack:** Rust 2021 (toolchain 1.99.1), `sqlx` 0.9.0 (Postgres, tokio, chrono, migrate), `tokio` 1, `tar` 0.4.46, `flate2` 1.1.10, `getrandom` 0.4.3, `base64` 0.23.1, `sha2` 0.11.1, `subtle` 2.6.1, `clap` 4.6.7 (derive, env), `toml` 1.1, `bytesize` 2.7.0 (serde), `anyhow` 1; dev: `testcontainers-modules` 0.15.0 (postgres) with Docker.

---

## Roadmap position

Milestone 3 of 7 in `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Milestones"), as revised for Postgres storage in PR #36 (2026-11-02). M1 (core library) and M2 (analysis) are on `main` (`01ddbee`). M4 (read-only server) puts axum, rmcp, auth and the read tools on top of `catalog::CatalogState` and `store::keys`.

## Scope

In scope (design "Milestones", M3 row, and the sections it rests on: "Storage", "In-memory snapshot", "Import", "Export", "Noticing changes made outside the server", "Process and routes" for the subcommands, "Time", "Testing"):

- `db`: pool, embedded migrations, the whole schema of "Storage" (changeset tables included, so M5 adds no migration for them).
- `store::catalogs`, `store::keys`, `store::documents`, `store::import`, `store::export`.
- `archive`: tarball and directory reading with the import rules, tarball writing.
- `keys`: key generation, hashing and constant-time comparison.
- `config` (the fields M3 uses) and the `okf-mcp` CLI: `migrate`, `catalog create|list|delete`, `key create|list|revoke|grant|ungrant`, `import`, `export`.
- `catalog::CatalogState`: snapshot load, generation check, rebuild and swap (#32).
- The mention matcher cached per snapshot (#33).
- Lint's commit times from revision times (#31), and the doc comments that still describe the git plan.
- The in-memory workspace becomes production code holding bytes and execute bits; the `test-util` feature is dropped (#35).
- Round-trip tests (synthetic corpus always; the real catalog with `OKF_PARITY_CATALOG`) and the M3 smoke test.

Deliberately deferred:

| Item | Goes to | Why |
|---|---|---|
| HTTP server, auth layer, catalog selection, read tools, import/export routes, `/healthz` | M4 | Design milestone table |
| The `admin_generation` and `generation` polling loop, key cache | M4 (`serve`) | Needs a long-running process; M3 provides `CatalogState::refresh` and `store::catalogs::admin_generation` it calls |
| `search` tool description for `tags` (#34) | M4 | Belongs to the tool; the issue stays open |
| Cross-catalog search merge | M4 | Needs catalog selection |
| Changesets, overlays, `okf::ops`, publish with replay and the publish lock's in-process half, `revert`, `adopt_changeset` | M5 | Design milestone table; M3 creates their tables and takes the advisory lock on import |
| Secret refusal on write, the no-frontmatter `set_block` error | M5 | Write path |
| NTP offset check | M7 | Design milestone table |
| Config fields `listen`, `allowed_hosts`, `default_catalog`, `session_idle_timeout`, `time_server`, `poll_interval` | M4/M7 | Nothing in M3 reads them; each arrives with its user |

## Decisions taken while planning (2026-11-02)

1. **Database tests use testcontainers in plain `cargo test`.** Each test that needs Postgres starts its own throwaway `postgres:17-alpine` container (about 1.4 s, in parallel). The gates therefore need Docker, which this machine has; without it those tests fail at once with "is docker reachable?".
2. **Runtime-checked SQL.** `sqlx::query`/`query_as`/`query_scalar` with `FromRow` structs, as pr-review-bot does. No database at compile time, no `.sqlx` cache.
3. **`test-util` is dropped (#35).** The in-memory workspace becomes production code (snapshots load from rows), which leaves nothing behind the feature.
4. **sqlx 0.9.0** rather than 0.8: new code, so no `SqlSafeStr` migration later (verified fact 1).

## Simplifications and deviations relative to the design

- **The in-memory workspace holds bytes and execute bits itself.** The design says the store keeps execute bits and non-text files "beside" the `&str` workspace. A link to an image must resolve (the snapshot asks the workspace `is_file`), so the workspace has to know every file. `MemWorkspace` stores each file's bytes and execute bit; `read_to_string` fails with `Io` (`InvalidData`) on non-UTF-8, exactly as `DiskWorkspace` does. Task 1 updates the design's "In-memory snapshot" paragraph.
- **Text means valid UTF-8.** The design's "valid UTF-8 with no NUL" came from Postgres `text`, which cannot hold NUL. Content is `bytea`, so NUL no longer matters, and `okf.py` reads such a file too. Task 1 updates the design's "Storage" paragraph.
- **`config` holds only `max_import_bytes` in M3.** The other design fields arrive in the milestone that reads them (table above). Unknown keys are ignored, so one config file serves every milestone. Task 4 notes this in the design's "Configuration".
- **CLI subcommands run migrations on connect.** `migrate` still exists (for deploy hooks), but every subcommand runs the embedded migrations first (idempotent), so a fresh database works without a separate step. Task 4 notes this in the design's "Process and routes".
- **The binary is renamed `okf-mcp`**: not a deviation, the design already uses that name; `Cargo.toml` said `okf_mcp`.
- **`api_keys.default_catalog_id`** references `catalogs` (`ON DELETE SET NULL`) instead of naming the catalog, so deleting a catalog cannot leave a key with a stale default. Task 6 updates the design's table.
- **Import writes rows one at a time inside one transaction.** A catalog has a few hundred files and import is rare; the smoke test records the real catalog's import time.
- **A tombstone keeps an empty `content`** (the column is `NOT NULL`); its revision row has `NULL` content, as the design says.
- **A file dropped by `import --replace` gets a `delete` revision** (with `message = 'import'`), not an `import` one with NULL content: history readers see one spelling for every deletion. Task 8 updates the design's "Import" step 3.
- **Lint's commit time is the document's last revision time.** After a first import, every document's "commit" is the import, so `generated-after-commit` cannot flag a stamp that falls between the real git commit and the import. The server has no git, so this is the closest equivalent; Task 13 records it under "What M3 shipped differently".

## Verified facts the code relies on

Each was checked against the source or by running it on 2026-11-02 (a probe crate passing 12 tests against Postgres 17 in Docker). `R` is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

1. **sqlx 0.9.0** (published 2026-05-21; MSRV 1.94, `R/sqlx-0.9.0/CHANGELOG.md:35`). `query`, `query_as` and `query_scalar` take `impl SqlSafeStr`, which only `&'static str` implements (`R/sqlx-core-0.9.0/src/query.rs:653`, `sql_str.rs:52`); runtime-built SQL needs `sqlx::AssertSqlSafe(string)` (`sql_str.rs:74`). This plan only uses literal SQL. Features: `default-features = false, features = ["runtime-tokio", "postgres", "chrono", "migrate", "macros"]`. Without a TLS feature, `sslmode=prefer` (the default) connects in plaintext and `require` fails (`R/sqlx-postgres-0.9.0/src/connection/tls.rs:27-39`); TLS is an M7 decision.
2. **Pool and migrations.** `PgPoolOptions::new()` (`R/sqlx-core-0.9.0/src/pool/options.rs:143`), `.max_connections(u32)` (`:173`), `.connect(&str).await` (`:528`). `sqlx::migrate!("./migrations")` needs the `macros` feature (`R/sqlx-0.9.0/src/lib.rs:97-98`), embeds the files at compile time relative to `CARGO_MANIFEST_DIR`, and rejects a bare `"migrations"` (`R/sqlx-macros-core-0.9.0/src/common.rs:17-24`). Adding a migration file does not trigger a rebuild without `build.rs` printing `cargo:rerun-if-changed=migrations` (`R/sqlx-0.9.0/src/macros/mod.rs:22-33`). `Migrator::run(&pool)` (`R/sqlx-core-0.9.0/src/migrate/migrator.rs:176`) returns `Result<(), MigrateError>`; a second run is a no-op.
3. **Transactions and queries.** `pool.begin()` gives `Transaction<'static, Postgres>` (`pool/mod.rs:371`); execute with `&mut *tx` (`transaction.rs:229`, `DerefMut` to `PgConnection`); `commit()` (`:120`); dropping without commit rolls back (`:269-277`). `.bind(..)` (`query.rs:87`); `fetch_one/fetch_all/fetch_optional/execute` (`query.rs:185,247,269,291`); `#[derive(sqlx::FromRow)]` (the `macros` feature includes `derive`); a tuple is a `FromRow` type. `execute(..).rows_affected()` (`query_result.rs:9`).
4. **Types.** `bytea` ↔ `Vec<u8>` (NUL and 0xFF round-trip), `text[]` ↔ `Vec<String>`, `timestamptz` ↔ `chrono::DateTime<Utc>`, `bigint` ↔ `i64`, `integer` ↔ `i32`, `boolean` ↔ `bool`, nullable ↔ `Option<T>` (bind `None::<String>`). The crate's `chrono` 0.4.45 with `default-features = false, features = ["now"]` works with sqlx's `chrono` feature.
5. **Errors.** `sqlx::Error::as_database_error()` (`R/sqlx-core-0.9.0/src/error.rs:144`) then `.is_unique_violation()` (`:260`; code `23505`) and `.constraint()` (`R/sqlx-postgres-0.9.0/src/error.rs:241`). The constraint for `UNIQUE (catalog_id, path, revision)` on `revisions` is named `revisions_catalog_id_path_revision_key`; a partial unique index reports its index name. `SELECT pg_advisory_xact_lock($1)` (bind `i64`, run with `execute`, since it returns `void`) blocks other transactions until commit or rollback; the plan uses the two-key `int4, int4` form of the same function with a namespace key (PostgreSQL docs, "Advisory Lock Functions"), which takes the same path in the server.
6. **testcontainers.** `testcontainers-modules` 0.15.0 requires `testcontainers ^0.27` (not 0.28): depend only on the modules crate and use its re-export `testcontainers_modules::testcontainers` (`R/testcontainers-modules-0.15.0/src/lib.rs:212`). `Postgres::default().with_tag("17-alpine").start().await` (`ImageExt::with_tag`, `R/testcontainers-0.27.3/src/core/image/image_ext.rs:66`; `AsyncRunner::start`, `runners/async_runner.rs:65`); the default tag `11-alpine` is end-of-life, so always set one. `get_host().await` and `get_host_port_ipv4(5432).await`; URL `postgres://postgres:postgres@{host}:{port}/postgres` (`R/testcontainers-modules-0.15.0/src/postgres/mod.rs:103-107`). Dropping the `ContainerAsync` removes the container, on multi-thread and current-thread `#[tokio::test]` alike (`core/async_drop.rs:16-33`). **A container in a `static` is never dropped** (Rust does not drop statics, and there is no reaper), so each test owns its container.
7. **tar 0.4.46 and flate2 1.1.10.** Writing: `tar::Builder::new(GzEncoder::new(w, Compression::default()))`; `Header::new_ustar()`, `set_entry_type`, `set_size`, `set_mode`, `set_mtime`; `append_data(&mut header, path, reader)` sets the path (adding a GNU long-name entry when needed) and the checksum (`R/tar-0.4.46/src/builder.rs:213-221,851-896`); `into_inner()` then `finish()`. Reading: `Archive::new(GzDecoder::new(r)).entries()`; per entry `path_bytes()` (raw name, `entry.rs:87`), `header().entry_type()`, `header().mode()?` (fails on a garbage mode field), `read_to_end`. GNU long names and local pax headers are folded into the next entry, but **a pax global header (`EntryType::XGlobalHeader`) is yielded as an entry**. tar reads long-name and pax payloads whole with no limit, so the uncompressed cap must wrap the decompressed stream, and `Archive::into_inner()` (`archive.rs:72`) tells "cap exceeded" from other errors. Raw names such as `../escape`, `/abs`, `./a`, `a//b` and non-UTF-8 bytes come through `path_bytes` unchanged.
8. **`git archive --format=tar.gz HEAD` of `pg_knowledge_catalog`** (HEAD `67ca473b`): 1 `XGlobalHeader` (`pax_global_header`, holding the commit id), 36 `Directory` entries (names end in `/`), 225 `Regular` files (= `git ls-files`). Modes are 0664/0775 (git's `tar.umask` 002), so executability is `mode & 0o111 != 0`; the 19 files at 0775 are exactly the index's `100755` entries. Every mtime is the commit time. The catalog tracks `.claude/skills/*`, `.gitattributes`, `.gitignore`, `.okf-catalog.json`; every file is valid UTF-8 (one is empty).
9. **Keys.** `getrandom::fill(&mut [u8; 32]) -> Result<(), getrandom::Error>` (`R/getrandom-0.4.3/src/lib.rs:87`). `base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)` (43 characters for 32 bytes; `use base64::Engine as _`). `sha2::Sha256::digest(bytes)` returns a `hybrid_array::Array<u8, U32>` with no `LowerHex`, so convert with `.into()` to `[u8; 32]` (the plan stores `bytea` and needs no `hex`). `subtle::ConstantTimeEq::ct_eq` on slices returns a `Choice`; `.into()` gives `bool`, and different lengths compare false without panicking.
10. **clap 4.6.7** (features `derive`, `env`). Default subcommand: `command: Option<Command>`. Nested: `#[command(subcommand)] Catalog(CatalogCommand)`. `#[arg(long, value_delimiter = ',', required_unless_present = "all", conflicts_with = "all")] catalogs: Vec<String>` with `#[arg(long)] all: bool` gives `ArgumentConflict` for both and `MissingRequiredArgument` for neither. `#[arg(long, value_enum)]` with `#[derive(ValueEnum)]`. `#[arg(long, env = "DATABASE_URL", global = true, hide_env_values = true)]`; a flag beats the variable.
11. **bytesize 2.7.0** (feature `serde`): `"64MiB".parse::<ByteSize>()` is 67 108 864, `.as_u64()`; `"64MB"` and `"64M"` are decimal (64 000 000). Deserializes from a string or a number.
12. **M1/M2 API used here.** `Workspace` (`src/okf/workspace.rs:31`; `read_to_string`, `write_atomic`, `is_file`, `is_dir`, `list_markdown`, `list_files`), `DiskWorkspace::new` (`:243`) and its listing contract (`list_files` prunes `.git`/`.worktrees` directories but lists a `.git` file), `MemWorkspace` (`:515`, `from_files` `:527`), `Snapshot::build(&dyn Workspace)` (`src/okf/snapshot.rs:247`), `Snapshot::{docs, get, concepts}`, `SnapshotDoc::{path, kind, title, frontmatter}`, `DocKind` (`:57`), `Frontmatter::data` (`:96`), `yaml::Value::as_str` (`src/okf/yaml.rs:46`), `lint(&Snapshot, DateTime<Utc>, Option<&CommitTimes>)` (`src/okf/analysis/lint.rs:109`), `CommitTimes::new(HashMap<String, DateTime<FixedOffset>>, HashSet<String>)` (`:66`), `links_report` (`src/okf/analysis/links.rs:120`) and its private `MentionMatcher` (`:303`, built at `:190`), `clock::{Clock, SystemClock, FixedClock}`, `OkfError` (`src/error.rs`).
13. **Brooke** (`brooke-reviews[bot]`) reviews each PR once and skips a branch behind `main`; `/prrb` re-triggers, and no eyes reaction a minute after a request means it was missed (catalog `/processes/subagent-pr-workflow.md`).

## Conventions for every task

- Work in the worktree and branch the coordinator gives you (`.worktrees/okf_mcp-<issue>` off `origin/main`). Never edit the main checkout.
- The user is a Python expert and a Rust novice: comment Rust-specific ideas (`async`/`.await`, `?`, `&mut *tx`, trait objects, `Arc`/`RwLock`, `spawn_blocking`, derives), as M1 and M2 do.
- No `.unwrap()` outside tests; `.expect("why this can't fail")` only for invariants (poisoned locks, literal regexes).
- Every public item has a doc comment with Arguments, Returns and Errors where they apply.
- **Every timestamp written to the database comes from the caller's `now`** (the CLI passes `clock.now()`); no SQL uses `now()` or a column default for time.
- SQL is literal text (`&'static str`), parameters always bound, never formatted into the string.
- Tests: Arrange-Act-Assert, `#[cfg(test)] mod tests` beside the code. A test that needs Postgres is `#[tokio::test(flavor = "multi_thread")]` and starts its own database with `crate::db::test_db().await` (unit tests) or `support::pg::TestDb::start().await` plus `okf_mcp::db::migrate` (integration tests). No commented-out tests.
- **Gates** (all must pass before every push; paste the counts into the PR):
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test` (needs Docker from Task 3 on)
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  - `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored` (M2's live parity; needs `python3`)
  - From Task 12 on, also `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`.
- **Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m3-task-<NN>-<slug>.md`, with Added/Changed/Notes sections like `changelog/2026-10-27T171446Z-m2-analysis.md`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with the Claude Code attribution line.
- The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## File map

| File | Created/modified by | Responsibility |
|---|---|---|
| `src/okf/workspace.rs` | T1 (modify), T11 (doc comment) | `MemWorkspace` with bytes and execute bits, production code |
| `Cargo.toml` | T1 (drop `test-util`), T3 (`sqlx`, `tokio`, dev `testcontainers-modules`), T4 (`clap`, `anyhow`, `toml`, `bytesize`, bin rename), T6 (`getrandom`, `base64`, `sha2`, `subtle`), T7 (`tar`, `flate2`) | Dependencies (keep both sides on conflict) |
| `README.md` | T1 (drop `test-util` note), T13 (rewrite status, CLI, testing) | Front page |
| `src/okf/analysis/links.rs` | T2 (modify) | `MentionMatcher` owned and `pub(crate)` |
| `src/okf/snapshot.rs` | T2 (cached matcher), T11 (module doc) | `Snapshot::mention_matcher` |
| `build.rs` | T3 (create) | Rebuild when `migrations/` changes |
| `migrations/0001_schema.sql` | T3 (create) | The whole schema |
| `src/db.rs` | T3 (create) | Pool, `MIGRATOR`, `migrate`, `test_db` |
| `tests/support/pg.rs`, `tests/support/mod.rs` | T3 (create) | Throwaway Postgres for tests |
| `src/lib.rs` | T3 (`db`), T4 (`cli`, `config`), T5 (`store`), T6 (`keys`), T7 (`archive`), T10 (`catalog`) | `pub mod` lines (keep both sides) |
| `src/error.rs` | T3 (`Database`, `Migrate`), T4 (`Config`), T5 (`CatalogNotSelected`, `Refused`), T7 (`ImportRefused`), T11 (doc) | Error variants (keep both sides) |
| `src/config.rs` | T4 (create) | `Config`, `load` |
| `src/cli.rs` | T4 (create); T5, T6, T8, T9 (add a subcommand each) | `Cli`, `Command`, `run` (keep both sides) |
| `src/main.rs` | T4 (modify) | tokio entry point |
| `src/store/mod.rs` | T5 (create); T6, T8, T9 (add `pub mod`) | Store module root |
| `src/store/catalogs.rs` | T5 (create) | Catalog rows, `admin_generation` |
| `src/keys.rs` | T6 (create) | Key generation and hashing |
| `src/store/keys.rs` | T6 (create) | Key rows and grants |
| `src/archive.rs` | T7 (create) | Tarball and directory reading, tarball writing |
| `src/store/documents.rs` | T8 (create), T11 (modify) | Document rows, derived columns, `published` |
| `src/store/import.rs` | T8 (create) | `import` |
| `src/store/export.rs` | T9 (create) | `export` |
| `src/catalog.rs` | T10 (create), T11 (modify) | `CatalogState`, `Loaded` |
| `src/okf/analysis/lint.rs`, `src/okf/mod.rs` | T11 (doc comments) | Git-era wording |
| `tests/roundtrip.rs` | T12 (create) | Import/export round trips |
| `tests/smoke.rs` | T13 (modify) | Storage timings on the real catalog |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | T1 (two paragraphs), T4 (configuration, subcommands), T6 (`default_catalog_id`), T8 (import step 3), T13 (M3 delivered) | Design |
| `changelog/*-m3-*.md` | every task | Iteration notes |

## Task dependencies

```
T1 workspace bytes (#35) ──> T7 archive ──────────┐
T2 matcher cache (#33) ─────────────────────────────┼──────────────────────────────┐
T3 db + schema ──> T4 cli + config ──> T5 catalogs ─┼─> T6 keys                     │
                                    T1, T5, T7 ─────┴─> T8 import ──> T9 export ──┐ │
                                                         T8 ──> T10 catalog state (#32) ──> T11 commit times (#31)
                                                         T8, T9 ──> T12 round trip ──────────┴─> T13 smoke/README/changelog
```

Edges, explicitly:

- T3 -> T4 -> T5
- T5 -> T6; T5 -> T8
- T1 -> T7 (`archive` stores `StoredFile`, which T1 creates)
- T1 -> T8; T7 -> T8
- T8 -> T9
- T1 -> T10; T8 -> T10
- T10 -> T11
- T8 -> T12; T9 -> T12
- every task -> T13 (T2, T6, T11, T12 included)

Tasks with no dependencies, which can start straight away: **T1, T2, T3**; T7 follows T1. T1 and T2 touch disjoint files; T1 and T3 both touch `Cargo.toml`, and T3 and T7 both touch `Cargo.toml` and `src/lib.rs`. Keep both sides.

> **Corrected (review of PR #37), 2026-11-02.** Added T1 -> T7: Task 7's `archive` uses `StoredFile` from Task 1.

---

### Task 1: The in-memory workspace holds bytes and execute bits; drop `test-util` (#35)

Snapshots of stored catalogs are built over `MemWorkspace`, so it becomes production code. It must know every file of a catalog, text or not, so a link to an image resolves; and it must keep each file's execute bit, so a script round-trips. Nothing is left behind the `test-util` feature, which is removed (decision 3).

**Files:**
- Modify: `src/okf/workspace.rs` (remove every `#[cfg(any(test, feature = "test-util"))]`, the module comment about it, and the `MemWorkspace` doc paragraph about it; add `StoredFile`; change the map's value type; new constructors and accessors; tests)
- Modify: `Cargo.toml` (delete the `[features]` table and its comment)
- Modify: `README.md` (delete the paragraph "The in-memory `MemWorkspace` test double is compiled only ...")
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Storage" and "In-memory snapshot" paragraphs, below)
- Create: `changelog/<ts>-m3-task-01-workspace-bytes.md`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module of `src/okf/workspace.rs`:

```rust
    fn stored(content: &[u8], executable: bool) -> StoredFile {
        StoredFile {
            content: content.to_vec(),
            executable,
        }
    }

    #[test]
    fn stored_files_keep_bytes_and_execute_bits() {
        let ws = MemWorkspace::from_stored([
            ("tools/run.sh".to_string(), stored(b"#!/bin/sh\n", true)),
            ("img/logo.png".to_string(), stored(b"\x89PNG\0\xff", false)),
            ("a.md".to_string(), stored(b"# A\n", false)),
        ])
        .unwrap();

        assert_eq!(ws.read_to_string("tools/run.sh").unwrap(), "#!/bin/sh\n");
        assert_eq!(ws.stored("tools/run.sh"), Some(stored(b"#!/bin/sh\n", true)));
        // Non-UTF-8 content is listed and exists, but reading it as text fails
        // with `Io`, exactly as `DiskWorkspace` reports it.
        assert!(ws.is_file("img/logo.png"));
        assert!(ws.list_files().unwrap().contains(&"img/logo.png".to_string()));
        assert!(matches!(ws.read_to_string("img/logo.png"), Err(OkfError::Io { .. })));
    }

    #[test]
    fn a_write_keeps_an_existing_execute_bit_and_new_files_are_not_executable() {
        let ws = MemWorkspace::from_stored([("run.sh".to_string(), stored(b"old", true))]).unwrap();

        ws.write_atomic("run.sh", "new").unwrap();
        ws.write_atomic("fresh.sh", "x").unwrap();

        assert_eq!(ws.stored("run.sh"), Some(stored(b"new", true)));
        assert_eq!(ws.stored("fresh.sh"), Some(stored(b"x", false)));
    }

    #[test]
    fn from_stored_refuses_a_path_outside_the_catalog() {
        let refused = MemWorkspace::from_stored([("../x.md".to_string(), stored(b"", false))]);
        assert!(matches!(refused, Err(OkfError::InvalidPath { .. })));
    }

    #[test]
    fn into_stored_returns_every_file() {
        let ws = MemWorkspace::from_files([("a.md", "a"), ("b/c.txt", "c")]);
        let files = ws.into_stored();
        assert_eq!(files.len(), 2);
        assert_eq!(files["b/c.txt"], stored(b"c", false));
    }
```

And in `src/okf/snapshot.rs`'s test module, a link to a binary file must not be broken:

```rust
    #[test]
    fn a_link_to_a_binary_file_resolves() {
        use crate::okf::workspace::StoredFile;
        let ws = MemWorkspace::from_stored([
            (
                "a.md".to_string(),
                StoredFile {
                    // A plain link: `okf.py`'s link pattern skips image syntax
                    // (`![..](..)`), so an image embed is not a link at all.
                    content: b"---\ntype: Note\n---\n[logo](img/logo.png)\n".to_vec(),
                    executable: false,
                },
            ),
            (
                "img/logo.png".to_string(),
                StoredFile {
                    content: b"\x89PNG\0\xff".to_vec(),
                    executable: false,
                },
            ),
        ])
        .unwrap();

        let snapshot = Snapshot::build(&ws).unwrap();

        let link = &snapshot.get("a.md").unwrap().links()[0];
        assert!(link.target_exists && link.target_is_file);
    }
```

(If `snapshot.rs`'s test module does not import `MemWorkspace` yet, add `use crate::okf::workspace::MemWorkspace;`.)

> **Corrected (issue #35), 2026-11-02.** The fixture first used `![logo](img/logo.png)`, which yields no link: `markdown::find_links` ports `okf.py`'s `LINK_RE` with its `(?<!!)`, so an image embed is not a link and `links()[0]` panicked. It now uses `[logo](img/logo.png)`; Task 10's `load_builds_the_snapshot_of_the_live_documents` had the same fixture and is fixed the same way.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib workspace snapshot`
Expected: compile errors (`cannot find type StoredFile`, `no function from_stored`).

- [ ] **Step 3: Implement**

In `src/okf/workspace.rs`, delete every `#[cfg(any(test, feature = "test-util"))]` line and the module comment block that explains it (lines 8-12). Above `MemWorkspace`, add:

```rust
/// One stored file: its exact bytes and its execute bit (a record, so its
/// fields are public).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    /// The file's exact bytes (UTF-8 or not).
    pub content: Vec<u8>,
    /// Is the execute bit set (`chmod +x`)?
    pub executable: bool,
}
```

Replace the `MemWorkspace` doc comment's last paragraph ("Compiled for this crate's unit tests ...") with:

```rust
/// Production code: a stored catalog's snapshot is built over one (see
/// `crate::catalog`), and tests use it as a catalog fixture. Each file is
/// held as bytes with its execute bit, so non-text files exist (links to them
/// resolve) but cannot be read as text, as on disk.
```

Change the field to `files: RwLock<BTreeMap<String, StoredFile>>`, and `has_key_with_prefix`'s parameter to `&BTreeMap<String, StoredFile>`. In `impl MemWorkspace`, keep `from_files` (each fixture is non-executable text):

```rust
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let map = files
            .into_iter()
            .map(|(p, c)| {
                let rel = checked_file(p).expect("fixture path must be inside the catalog");
                let file = StoredFile {
                    content: c.as_bytes().to_vec(),
                    executable: false,
                };
                (rel, file)
            })
            .collect();
        Self {
            files: RwLock::new(map),
        }
    }

    /// Builds a workspace from stored files (a catalog's rows).
    ///
    /// # Arguments
    /// * `files` - `(catalog-relative path, file)` pairs.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if a path escapes the catalog or is the root.
    pub fn from_stored(
        files: impl IntoIterator<Item = (String, StoredFile)>,
    ) -> Result<Self, OkfError> {
        // `collect` into `Result<BTreeMap<_, _>, _>` stops at the first error,
        // like a loop that raises.
        let map = files
            .into_iter()
            .map(|(path, file)| Ok((checked_file(&path)?, file)))
            .collect::<Result<BTreeMap<_, _>, OkfError>>()?;
        Ok(Self {
            files: RwLock::new(map),
        })
    }

    /// The stored file at `path`, if there is one.
    ///
    /// # Returns
    /// A copy of its bytes and execute bit; `None` if there is no such file.
    pub fn stored(&self, path: &str) -> Option<StoredFile> {
        let rel = checked(path).ok()?;
        self.files().get(&rel).cloned()
    }

    /// Every file, consuming the workspace (no copy).
    ///
    /// # Returns
    /// Catalog-relative path to file, sorted by path.
    pub fn into_stored(self) -> BTreeMap<String, StoredFile> {
        // `into_inner` takes the map out of the lock; it fails only if a
        // thread panicked while holding the lock.
        self.files
            .into_inner()
            .expect("MemWorkspace lock poisoned")
    }
```

`files()` returns `RwLockReadGuard<'_, BTreeMap<String, StoredFile>>`. In `impl Workspace for MemWorkspace`:

```rust
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        let rel = checked(path)?;
        let files = self.files();
        let file = files.get(&rel).ok_or_else(|| OkfError::NotFound {
            path: path.to_string(),
        })?;
        // The same error `std::fs::read_to_string` gives for non-UTF-8 bytes.
        // `from_utf8` borrows the bytes (no copy until it is known to be text).
        std::str::from_utf8(&file.content)
            .map(str::to_string)
            .map_err(|error| OkfError::Io {
                path: path.to_string(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            })
    }
```

In `write_atomic`, replace `files.insert(rel, contents.to_string());` with:

```rust
        // An existing file keeps its execute bit, as a rewrite on disk keeps
        // its mode; a new file is not executable (0o666 less the umask).
        let executable = files.get(&rel).is_some_and(|file| file.executable);
        files.insert(
            rel,
            StoredFile {
                content: contents.as_bytes().to_vec(),
                executable,
            },
        );
```

`is_file`, `is_dir`, `list_markdown` and `list_files` only read keys and are unchanged.

In `Cargo.toml`, delete the `[features]` table (`test-util = []`) and its three comment lines. In `README.md`, delete the `MemWorkspace`/`test-util` paragraph under "Testing".

In the design doc, "Storage" section, replace "A file that is valid\nUTF-8 with no NUL is text:" with "A file that is valid\nUTF-8 is text (NUL bytes included: content is `bytea`):"; and in "In-memory snapshot" replace the sentences from "The `Workspace` trait\nreads and writes `&str`" to the end of that paragraph with:

```markdown
The in-memory workspace holds every file's bytes and execute bit, so a link to
a non-text file resolves, reading such a file as text fails as it does on disk,
and a write through the workspace keeps an existing file's execute bit (a new
file is not executable).
```

> **Corrected (issue #35), 2026-11-02.** The sentence before that replacement, "The in-memory workspace, which M2 kept behind the `test-util` feature, becomes production code.", also had to change, or Step 5's `test-util` grep finds it: it reads "The in-memory workspace, test-only in M2, becomes production code." The paragraph then continues "It holds every file's bytes ..." rather than repeating "The in-memory workspace".

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib`
Expected: PASS, including every existing workspace and snapshot test (fixtures go through `from_files`).

- [ ] **Step 5: Gates, changelog, commit**

Run the gates. `grep -rn "test-util" --include=*.rs --include=*.toml --include=*.md . | grep -v '^./docs/superpowers/plans\|^./changelog\|^./target'` must print nothing but the design's "Carried into M3" entry. Write `changelog/<ts>-m3-task-01-workspace-bytes.md` (Added: `StoredFile`, `from_stored`, `stored`, `into_stored`; Changed: `MemWorkspace` is production code holding bytes and execute bits; Removed: the `test-util` feature, closing #35). Commit:

```bash
git add src/okf/workspace.rs src/okf/snapshot.rs Cargo.toml README.md docs/superpowers/specs changelog/
git commit -m "workspace: MemWorkspace holds bytes and execute bits; drop test-util (#35)"
```

PR body: `Closes #35`.

---

### Task 2: Cache the unlinked-mention matcher per snapshot (#33)

`links_report` builds its `RegexSet` over every concept name on each call (about 120 ms of the real catalog's 126 ms links report). With long-lived snapshots it is built once per snapshot.

**Files:**
- Modify: `src/okf/analysis/links.rs` (`MentionMatcher` owns its paths and is `pub(crate)`; `links_report` uses the cached one)
- Modify: `src/okf/snapshot.rs` (a `OnceLock` field and `mention_matcher()`)
- Create: `changelog/<ts>-m3-task-02-matcher-cache.md`

- [ ] **Step 1: Write the failing test**

In `src/okf/snapshot.rs`'s test module:

```rust
    #[test]
    fn the_mention_matcher_is_built_once_per_snapshot() {
        let snapshot = Snapshot::build(&MemWorkspace::from_files([
            ("a.md", "---\ntype: Note\ntitle: Alpha concept\n---\nSee Beta concept.\n"),
            ("b.md", "---\ntype: Note\ntitle: Beta concept\n---\nBody.\n"),
        ]))
        .unwrap();

        let first: *const _ = snapshot.mention_matcher();
        let second: *const _ = snapshot.mention_matcher();

        // `*const _` is a raw pointer: comparing two of them asks "same object?",
        // like Python's `is`.
        assert!(std::ptr::eq(first, second));
        let report = crate::okf::analysis::links::links_report(&snapshot, true);
        assert_eq!(report.unlinked_mentions.len(), 1);
        assert_eq!(report, crate::okf::analysis::links::links_report(&snapshot, true));
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test --lib the_mention_matcher_is_built_once`
Expected: compile error, `no method named mention_matcher`.

- [ ] **Step 3: Implement**

In `src/okf/analysis/links.rs`:

- Make the matcher own its target paths, so it can live inside the snapshot (a struct cannot borrow from the struct that holds it):

```rust
/// Every concept's names as one `RegexSet`, built once per snapshot
/// ([`Snapshot::mention_matcher`]).
// `pub(crate)`: visible inside this crate (the snapshot stores one), not to users.
#[derive(Debug, Clone)]
pub(crate) struct MentionMatcher {
    /// `(concept path, its names)`, in concept order.
    targets: Vec<(String, Vec<String>)>,
    /// For each set pattern: `(index into targets, index into its names)`.
    owners: Vec<(usize, usize)>,
    /// How lines are searched for the names, one pattern per `owners` entry.
    search: NameSearch,
}
```

  Add `#[derive(Debug, Clone)]` to `enum NameSearch`. Change `impl<'a> MentionMatcher<'a>` to `impl MentionMatcher`, `fn new(concepts: &[&'a SnapshotDoc])` to `pub(crate) fn new(concepts: &[&SnapshotDoc])` (likewise `with_set_limit`), and build `targets` with owned paths:

```rust
        let targets: Vec<(String, Vec<String>)> = concepts
            .iter()
            .map(|doc| (doc.path().to_string(), names_for(doc)))
            .filter(|(_, names)| !names.is_empty())
            .collect();
```

  Drop the lifetime from every `&MentionMatcher<'_>` parameter (`unlinked_mentions`, `mentions_in`). In `mentions_in`, `target_path` is now `&String`: compare with `target_path == source.path()` and `linked.contains(target_path.as_str())`. Delete the comment block about `<'a>` above the struct.
- In `links_report`, replace `&MentionMatcher::new(&concepts)` with `snapshot.mention_matcher()`.
- Update the module doc or the `SET_SIZE_LIMIT` comment's timing sentence: the set is now compiled once per snapshot.

In `src/okf/snapshot.rs`:

```rust
use std::sync::{LazyLock, OnceLock};

use crate::okf::analysis::links::MentionMatcher;
```

Add a field to `Snapshot`:

```rust
    /// The unlinked-mention matcher, built on first use ([`Snapshot::mention_matcher`]).
    // `OnceLock` is a cell written at most once, safely across threads: the
    // first caller builds the value and every later caller reads it.
    mention_matcher: OnceLock<MentionMatcher>,
```

initialize it with `mention_matcher: OnceLock::new(),` in `build`, and add:

```rust
    /// The matcher `links_report` uses for unlinked mentions: every concept in
    /// the link graph, built on the first call and reused afterwards.
    pub(crate) fn mention_matcher(&self) -> &MentionMatcher {
        self.mention_matcher.get_or_init(|| {
            // The same concepts `links_report` collects: in the link graph, kind Concept.
            let concepts: Vec<&SnapshotDoc> = self
                .docs
                .iter()
                .filter(|doc| doc.in_link_graph() && doc.kind == DocKind::Concept)
                .collect();
            MentionMatcher::new(&concepts)
        })
    }
```

The existing test that compares `MentionMatcher::new` with `with_set_limit(.., 1)` keeps working with the new signatures.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib links snapshot` then `cargo test --test parity`
Expected: PASS (links goldens unchanged).

- [ ] **Step 5: Gates, changelog, commit**

Run the gates (live parity included: the links JSON must not change). Changelog: Changed: the mention matcher is built once per snapshot (#33). Commit:

```bash
git add src/okf/analysis/links.rs src/okf/snapshot.rs changelog/
git commit -m "links: build the mention matcher once per snapshot (#33)"
```

PR body: `Closes #33`.

---

### Task 3: `db`: dependencies, the schema, migrations and the test database

The whole schema of the design's "Storage" section in one migration, embedded in the binary, plus the throwaway Postgres every later database test uses.

**Files:**
- Modify: `Cargo.toml` (`sqlx`, `tokio`; dev `testcontainers-modules`)
- Create: `build.rs`
- Create: `migrations/0001_schema.sql`
- Create: `src/db.rs`; modify `src/lib.rs` (`pub mod db;`)
- Modify: `src/error.rs` (`Database`, `Migrate`)
- Create: `tests/support/mod.rs`, `tests/support/pg.rs`
- Create: `changelog/<ts>-m3-task-03-db.md`

- [ ] **Step 1: Dependencies, build script, test support**

`Cargo.toml`, under `[dependencies]`:

```toml
# Postgres access with runtime-checked queries, and the schema migrations
# embedded at compile time (`migrate!` needs `macros`). No TLS feature yet:
# that is a deployment decision (M7).
sqlx = { version = "0.9.0", default-features = false, features = ["runtime-tokio", "postgres", "chrono", "migrate", "macros"] }
# The async runtime sqlx runs on, and the CLI's entry point.
tokio = { version = "1.54", features = ["macros", "rt-multi-thread"] }
```

and under `[dev-dependencies]`:

```toml
# Throwaway Postgres containers for the database tests (needs Docker). Only
# the modules crate: it re-exports the testcontainers version it supports.
testcontainers-modules = { version = "0.15.0", features = ["postgres"] }
```

`build.rs`:

```rust
//! Rebuilds the crate when a migration is added or edited: `sqlx::migrate!`
//! embeds `migrations/` at compile time, and cargo would not notice a new file.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
```

`tests/support/mod.rs`:

```rust
//! Helpers shared by the integration tests (`mod support;` in each test file).

pub mod pg;
```

`tests/support/pg.rs`:

```rust
//! A throwaway Postgres for tests: one container per test, removed when the
//! [`TestDb`] is dropped. The library's unit tests include this same file
//! (`#[path]` in `src/db.rs`), so there is one copy.
//!
//! Each test starts its own container (about 1.4 s; tests run in parallel). A
//! container shared through a `static` would never be removed, because Rust
//! does not drop statics.

// Each crate that includes this file uses a different part of it (the CLI
// tests need `url`, the unit tests only `pool`).
#![allow(dead_code)]

use sqlx::postgres::{PgPool, PgPoolOptions};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// A running Postgres 17 and a pool connected to it (not migrated).
pub struct TestDb {
    /// Connected to the container's `postgres` database.
    pub pool: PgPool,
    /// The connection URL, for code that connects by itself (the CLI).
    pub url: String,
    // Fields drop in declaration order: the pool closes before the container
    // (whose `Drop` stops and removes it) goes away.
    _container: ContainerAsync<Postgres>,
}

impl TestDb {
    /// Starts the container and connects.
    ///
    /// # Panics
    /// If Docker is unreachable or the container does not start: a test
    /// environment problem, reported at once.
    pub async fn start() -> Self {
        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start a postgres container (is docker reachable?)");
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect to the test postgres");
        Self {
            pool,
            url,
            _container: container,
        }
    }
}
```

- [ ] **Step 2: Write the failing tests**

`src/db.rs` (tests first; the functions come in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn migrations_create_the_schema_and_rerun_as_a_no_op() {
        let db = test_db().await;

        migrate(&db.pool).await.unwrap();

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations' \
             ORDER BY table_name",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            [
                "api_keys",
                "catalogs",
                "changeset_entries",
                "changesets",
                "documents",
                "key_catalogs",
                "revisions",
                "server_state",
            ]
        );
        let admin_generation: i64 =
            sqlx::query_scalar("SELECT admin_generation FROM server_state")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(admin_generation, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn constraints_hold() {
        let db = test_db().await;
        let catalog: i64 = sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at) VALUES ('pg', $1) RETURNING id",
        )
        .bind(chrono::Utc::now())
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let bad_name = sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('Bad Name', $1)")
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap_err();
        assert!(bad_name.as_database_error().unwrap().is_check_violation());

        let open = "INSERT INTO changesets (catalog_id, session_id, actor, state, created_at, \
                    updated_at) VALUES ($1, 's1', 'a', 'open', $2, $2)";
        sqlx::query(open).bind(catalog).bind(chrono::Utc::now()).execute(&db.pool).await.unwrap();
        let second = sqlx::query(open)
            .bind(catalog)
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap_err();
        let second = second.as_database_error().unwrap();
        assert!(second.is_unique_violation());
        assert_eq!(second.constraint(), Some("changesets_one_open_per_session"));

        let revision = "INSERT INTO revisions (catalog_id, path, revision, op, content, \
                        executable, actor, message, created_at) \
                        VALUES ($1, 'a.md', 1, 'import', $2, false, 'a', 'm', $3)";
        sqlx::query(revision)
            .bind(catalog)
            .bind(b"x".to_vec())
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap();
        let duplicate = sqlx::query(revision)
            .bind(catalog)
            .bind(b"y".to_vec())
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap_err();
        assert_eq!(
            duplicate.as_database_error().unwrap().constraint(),
            Some("revisions_catalog_id_path_revision_key")
        );

        // Deleting the catalog cascades to every row that refers to it.
        sqlx::query("DELETE FROM catalogs WHERE id = $1").bind(catalog).execute(&db.pool).await.unwrap();
        let left: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM changesets), (SELECT count(*) FROM revisions)",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(left, (0, 0));
    }

}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib db::`
Expected: compile errors (`cannot find function test_db`, `migrate`, `connect`).

- [ ] **Step 4: Write the schema and the module**

`migrations/0001_schema.sql`:

```sql
-- The okf_mcp schema: the design's "Storage" section. Every timestamp is
-- written by the server from its clock, so no column defaults to now().

-- One row: bumped by every `catalog` and `key` subcommand, so a running
-- server notices administrative changes.
CREATE TABLE server_state (
    singleton        boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    admin_generation bigint NOT NULL DEFAULT 0
);
INSERT INTO server_state DEFAULT VALUES;

CREATE TABLE catalogs (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        text NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9-]+$'),
    description text NOT NULL DEFAULT '',
    -- Bumped by every publish and import of this catalog.
    generation  bigint NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL
);

-- One row per file. A deleted document keeps its row as a tombstone
-- (deleted_at set, content empty), so a path created again continues its
-- revision numbering.
CREATE TABLE documents (
    catalog_id   bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    path         text NOT NULL,
    content      bytea NOT NULL,
    executable   boolean NOT NULL,
    revision     integer NOT NULL CHECK (revision > 0),
    deleted_at   timestamptz,
    updated_at   timestamptz NOT NULL,
    updated_by   text NOT NULL,
    -- Derived from content on every write (NULL for non-markdown files).
    kind         text CHECK (kind IN ('concept', 'index', 'log', 'machinery')),
    concept_type text,
    title        text,
    status       text,
    tags         text[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (catalog_id, path)
);

CREATE TABLE api_keys (
    id              text PRIMARY KEY CHECK (id ~ '^[A-Za-z0-9._-]+$'),
    sha256          bytea NOT NULL UNIQUE CHECK (length(sha256) = 32),
    access          text NOT NULL CHECK (access IN ('read', 'write', 'admin')),
    all_catalogs    boolean NOT NULL,
    -- Cleared when that catalog is deleted.
    default_catalog_id bigint REFERENCES catalogs (id) ON DELETE SET NULL,
    actor           text NOT NULL,
    created_at      timestamptz NOT NULL,
    revoked_at      timestamptz
);

CREATE TABLE key_catalogs (
    key_id     text NOT NULL REFERENCES api_keys (id) ON DELETE CASCADE,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    PRIMARY KEY (key_id, catalog_id)
);

CREATE TABLE changesets (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    catalog_id bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    session_id text NOT NULL,
    key_id     text REFERENCES api_keys (id) ON DELETE SET NULL,
    actor      text NOT NULL,
    state      text NOT NULL CHECK (state IN ('open', 'published', 'discarded')),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
-- A racing "get or create" cannot open two changesets for one session.
CREATE UNIQUE INDEX changesets_one_open_per_session
    ON changesets (catalog_id, session_id) WHERE state = 'open';

CREATE TABLE changeset_entries (
    changeset_id  bigint NOT NULL REFERENCES changesets (id) ON DELETE CASCADE,
    path          text NOT NULL,
    -- NULL for a path with no live document when the entry was made.
    base_revision integer,
    -- NULL for a delete.
    content       bytea,
    executable    boolean NOT NULL DEFAULT false,
    -- The entry's log and listing operations while every change to it came
    -- from one; NULL once any other edit touches it.
    replay        jsonb,
    PRIMARY KEY (changeset_id, path)
);

-- Append-only history.
CREATE TABLE revisions (
    catalog_id   bigint NOT NULL REFERENCES catalogs (id) ON DELETE CASCADE,
    path         text NOT NULL,
    revision     integer NOT NULL,
    op           text NOT NULL CHECK (op IN ('create', 'update', 'delete', 'import')),
    -- NULL for a delete.
    content      bytea,
    executable   boolean NOT NULL,
    actor        text NOT NULL,
    changeset_id bigint REFERENCES changesets (id) ON DELETE SET NULL,
    message      text NOT NULL,
    created_at   timestamptz NOT NULL,
    UNIQUE (catalog_id, path, revision)
);
```

`src/error.rs`, two variants (update the enum's doc to drop "git"; see Task 11 for the rest of that wording):

```rust
    /// A database operation failed. M4 logs the details and returns only the
    /// variant to MCP clients; the CLI prints them.
    #[error("database: {source}")]
    Database {
        /// The driver's error.
        // `#[from]` generates `From<sqlx::Error>`, so `?` converts it.
        #[from]
        source: sqlx::Error,
    },
    /// Applying the schema migrations failed.
    #[error("migrations: {source}")]
    Migrate {
        /// The migrator's error.
        #[from]
        source: sqlx::migrate::MigrateError,
    },
```

`src/db.rs`:

```rust
//! The Postgres connection pool and the schema migrations (`migrations/`).

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::error::OkfError;

// The library's unit tests share the integration tests' container helper:
// `#[path]` compiles that file here as a module (relative to `src/`).
#[cfg(test)]
#[path = "../tests/support/pg.rs"]
pub(crate) mod pg_support;

/// The schema migrations in `migrations/`, embedded at compile time.
// `migrate!` reads the directory while compiling (`build.rs` makes cargo
// notice new files), so the binary needs no SQL files at runtime.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Connects a pool to the database.
///
/// # Arguments
/// * `url` - a `postgres://` or `postgresql://` URL (CNPG's generated secret).
///
/// # Returns
/// A pool of up to 8 connections. `async fn` returns a future: the caller
/// `.await`s it, like a Python coroutine.
///
/// # Errors
/// [`OkfError::Database`] if the URL is malformed or the server is unreachable.
pub async fn connect(url: &str) -> Result<PgPool, OkfError> {
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await?;
    Ok(pool)
}

/// Applies every migration not yet applied (a no-op when up to date).
///
/// # Errors
/// [`OkfError::Migrate`] if a migration fails, or the database has a
/// migration this binary does not know (it is older than the schema).
pub async fn migrate(pool: &PgPool) -> Result<(), OkfError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// A migrated throwaway database for one test.
#[cfg(test)]
pub(crate) async fn test_db() -> pg_support::TestDb {
    let db = pg_support::TestDb::start().await;
    migrate(&db.pool).await.expect("migrations apply");
    db
}
```

`src/lib.rs`: add `pub mod db;` and mention the database in the crate doc ("the OKF document model (`okf`), storage (`db`), errors, and the clock").

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib db::`
Expected: PASS (two tests, about 2 s, Docker running).

- [ ] **Step 6: Gates, changelog, commit**

Run the gates. Changelog: Added: the schema (`migrations/0001_schema.sql`), `db::{connect, migrate, MIGRATOR}`, the test-database helper; Notes: `cargo test` now needs Docker. Commit:

```bash
git add Cargo.toml Cargo.lock build.rs migrations src/db.rs src/lib.rs src/error.rs tests/support changelog/
git commit -m "db: Postgres schema, embedded migrations and the test database"
```

---

### Task 4: `config` and the `okf-mcp` command line

The binary becomes `okf-mcp` with a `clap` command line. `migrate` applies the schema; `serve` (the default) says the server arrives in M4. Later tasks each add a subcommand.

**Files:**
- Modify: `Cargo.toml` (`clap`, `anyhow`, `toml`, `bytesize`; rename the `[[bin]]` to `okf-mcp`)
- Create: `src/config.rs`, `src/cli.rs`; modify `src/lib.rs` (`pub mod cli; pub mod config;`)
- Modify: `src/main.rs`
- Modify: `src/error.rs` (`Config`)
- Create: `changelog/<ts>-m3-task-04-cli.md`

- [ ] **Step 1: Dependencies**

```toml
# The command line (`okf-mcp migrate | catalog | key | import | export`);
# `env` lets DATABASE_URL fill `--database-url`.
clap = { version = "4.6.7", features = ["derive", "env"] }
# Application-level errors in the command line (the library uses OkfError).
anyhow = "1.0.104"
# The configuration file.
toml = "1.1"
# Sizes with units in the configuration ("64MiB").
bytesize = { version = "2.7.0", features = ["serde"] }
```

and rename the binary:

```toml
[[bin]]
name = "okf-mcp"
path = "src/main.rs"
```

- [ ] **Step 2: Write the failing tests**

`src/config.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
        assert_eq!(Config::default().max_import_bytes.as_u64(), 64 * 1024 * 1024);
    }

    #[test]
    fn sizes_take_units_or_plain_numbers_and_unknown_keys_are_ignored() {
        let config = Config::from_toml("max_import_bytes = \"1MiB\"\nlisten = \"0.0.0.0:8080\"\n");
        assert_eq!(config.unwrap().max_import_bytes.as_u64(), 1024 * 1024);
        let config = Config::from_toml("max_import_bytes = 2048\n").unwrap();
        assert_eq!(config.max_import_bytes.as_u64(), 2048);
    }

    #[test]
    fn a_bad_value_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        std::fs::write(&path, "max_import_bytes = \"lots\"\n").unwrap();

        let error = Config::load(Some(&path)).unwrap_err();

        assert!(matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")));
    }

    #[test]
    fn no_file_gives_the_defaults() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
    }
}
```

`src/cli.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use clap::Parser;

    fn cli(database_url: Option<String>, command: Command) -> Cli {
        Cli {
            database_url,
            config: None,
            command: Some(command),
        }
    }

    #[test]
    fn no_subcommand_means_serve() {
        let parsed = Cli::try_parse_from(["okf-mcp"]).unwrap();
        assert!(parsed.command.is_none());
        let parsed = Cli::try_parse_from(["okf-mcp", "migrate"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::Migrate)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migrate_applies_the_schema() {
        let db = crate::db::pg_support::TestDb::start().await;

        run(cli(Some(db.url.clone()), Command::Migrate), &FixedClock(chrono::Utc::now()))
            .await
            .unwrap();

        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(applied, 1);
    }

    #[tokio::test]
    async fn a_missing_database_url_is_explained() {
        let error = run(cli(None, Command::Migrate), &FixedClock(chrono::Utc::now()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("DATABASE_URL"));
    }

    #[tokio::test]
    async fn serve_is_not_there_yet() {
        let error = run(cli(None, Command::Serve), &FixedClock(chrono::Utc::now()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("milestone 4"));
    }
}
```

The tests build `Cli` directly rather than parsing, so a `DATABASE_URL` in the developer's environment cannot leak in. (`crate::db::pg_support` is `pub(crate)` for exactly this.)

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib config cli`
Expected: compile errors (`cannot find type Config`, `Cli`).

- [ ] **Step 4: Implement**

`src/error.rs`:

```rust
    /// The configuration file is unreadable or invalid.
    #[error("{path}: {message}")]
    Config {
        /// The file, as given.
        path: String,
        /// What is wrong with it.
        message: String,
    },
```

`src/config.rs`:

```rust
//! The configuration file (TOML; the design's "Configuration"). M3 reads only
//! what it uses; later milestones add their fields. Unknown keys are ignored,
//! so one file serves every version of the server.

use std::path::Path;

use bytesize::ByteSize;
use serde::Deserialize;

use crate::error::OkfError;

/// The default import limit: 64 MiB, uncompressed.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;

/// Settings read from the configuration file.
// `#[serde(default)]` fills every missing key from `Default::default()`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The largest uncompressed total an import may expand to (`"64MiB"`, or
    /// a number of bytes). `MiB` is binary; `MB` and `M` are decimal. For a
    /// tarball it counts the whole decompressed tar stream (headers and
    /// 512-byte padding included); for a directory, the files' contents.
    pub max_import_bytes: ByteSize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_import_bytes: ByteSize::b(DEFAULT_MAX_IMPORT_BYTES),
        }
    }
}

impl Config {
    /// Parses a configuration file's text.
    ///
    /// # Errors
    /// The TOML parser's error if the text is not valid TOML or a value has
    /// the wrong type.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Reads the configuration file, or returns the defaults without one.
    ///
    /// # Arguments
    /// * `path` - the file (`--config` or `OKF_MCP_CONFIG`), if any.
    ///
    /// # Errors
    /// [`OkfError::Config`] if the file can't be read or parsed.
    pub fn load(path: Option<&Path>) -> Result<Self, OkfError> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let error = |message: String| OkfError::Config {
            path: path.display().to_string(),
            message,
        };
        let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
        Self::from_toml(&text).map_err(|e| error(e.to_string()))
    }
}
```

`src/cli.rs`:

```rust
//! The `okf-mcp` command line: administration directly against the
//! database (design "Process and routes"). `serve`, the default, arrives in M4.
//!
//! Every subcommand that touches the database runs the embedded migrations
//! first (idempotent), so a fresh database needs no separate step.

use std::path::PathBuf;

// `Context` adds a message to an error (anyhow's `.context(..)`), like
// `raise X from e` with a better message.
use anyhow::Context;
use clap::{Parser, Subcommand};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::config::Config;
use crate::db;

/// `okf-mcp`: an MCP server for OKF knowledge catalogs.
#[derive(Debug, Parser)]
#[command(name = "okf-mcp", version, about)]
pub struct Cli {
    /// The Postgres URL.
    #[arg(long, env = "DATABASE_URL", global = true, hide_env_values = true)]
    pub database_url: Option<String>,
    /// The TOML configuration file.
    #[arg(long, env = "OKF_MCP_CONFIG", global = true)]
    pub config: Option<PathBuf>,
    /// What to do (`serve` when omitted).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve MCP over HTTP (arrives in milestone 4).
    Serve,
    /// Apply the schema migrations and exit.
    Migrate,
}

/// Runs one command line.
///
/// # Arguments
/// * `cli` - the parsed command line.
/// * `clock` - the source of every timestamp written (`&dyn Clock`: any
///   clock, real or fixed).
///
/// # Errors
/// Any failure, with context, for `main` to print.
pub async fn run(cli: Cli, clock: &dyn Clock) -> anyhow::Result<()> {
    // Read now so a bad file fails every command; `import` (Task 8) uses it.
    let _config = Config::load(cli.config.as_deref())?;
    let _ = clock; // The `catalog`, `key` and `import` subcommands (Tasks 5-8) use it.
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            anyhow::bail!("the MCP server arrives in milestone 4; see `okf-mcp --help`")
        }
        Command::Migrate => {
            connect(cli.database_url.as_deref()).await?;
            println!("migrations applied");
        }
    }
    Ok(())
}

/// Connects to the database and applies the migrations.
async fn connect(url: Option<&str>) -> anyhow::Result<PgPool> {
    let url = url.context("no database: set DATABASE_URL or pass --database-url")?;
    let pool = db::connect(url).await.context("connect to the database")?;
    db::migrate(&pool).await?;
    Ok(pool)
}
```

(Task 5 deletes the `let _ = clock;` line; Task 8 renames `_config` to `config`.)

`src/main.rs`:

```rust
//! The `okf-mcp` binary: parses the command line and runs it
//! ([`okf_mcp::cli::run`]).

use clap::Parser;
use okf_mcp::cli::Cli;
use okf_mcp::clock::SystemClock;
use tracing_subscriber::EnvFilter;

/// Installs a `tracing` subscriber on stderr (`RUST_LOG`, default `warn`),
/// then runs the command line.
// `#[tokio::main]` wraps `main` in a tokio runtime, so it can `.await`.
// Returning `anyhow::Result` prints the error chain and exits non-zero.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    okf_mcp::cli::run(Cli::parse(), &SystemClock).await
}
```

- [ ] **Step 5: Design edits**

In `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`:
- "Configuration": after the example TOML, add "Each milestone reads only its own fields (M3: `max_import_bytes`); unknown keys are ignored, so one file serves every version."
- "Process and routes", subcommand table: after the table, add "Every subcommand that touches the database applies the embedded migrations first (idempotent); `migrate` exists for deploy hooks that want them as a separate step."

The binary name `okf-mcp` needs no design edit: it is already the design's name.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib config cli`, then `cargo run -- --help` and `cargo run -- migrate` (no `DATABASE_URL`: the error names it).
Expected: PASS; the help lists `serve` and `migrate`.

- [ ] **Step 7: Gates, changelog, commit**

Run the gates. Changelog: Added: `okf-mcp` CLI (`migrate`, `serve` placeholder), `config::Config` (`max_import_bytes`); Changed: the binary is `okf-mcp`. Commit:

```bash
git add Cargo.toml Cargo.lock src/cli.rs src/config.rs src/main.rs src/lib.rs src/db.rs src/error.rs docs/superpowers/specs changelog/
git commit -m "cli: okf-mcp command line with migrate, and the config file"
```

---

### Task 5: `store::catalogs` and `okf-mcp catalog`

Catalog rows: create, list, find, delete (refusing while unpublished work exists, unless `--force`). Every change bumps `server_state.admin_generation` in the same transaction, so a running server (M4) notices.

**Files:**
- Create: `src/store/mod.rs`, `src/store/catalogs.rs`; modify `src/lib.rs` (`pub mod store;`)
- Modify: `src/error.rs` (`CatalogNotSelected`, `Refused`)
- Modify: `src/cli.rs` (`catalog` subcommand)
- Create: `changelog/<ts>-m3-task-05-catalogs.md`

- [ ] **Step 1: Write the failing tests**

`src/store/catalogs.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_list_and_find() {
        let db = test_db().await;

        let pg = create(&db.pool, "pg", "pgrace's catalog", now()).await.unwrap();
        create(&db.pool, "aa-work", "", now()).await.unwrap();

        assert_eq!((pg.name.as_str(), pg.generation, pg.created_at), ("pg", 0, now()));
        let names: Vec<String> = list(&db.pool).await.unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["aa-work", "pg"]);
        assert_eq!(find(&db.pool, "pg").await.unwrap(), pg);
        assert!(matches!(
            find(&db.pool, "nope").await,
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "nope"
        ));
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_and_duplicate_names_are_refused() {
        let db = test_db().await;
        create(&db.pool, "pg", "", now()).await.unwrap();

        for name in ["", "Bad Name", "a/b", "ünï"] {
            assert!(matches!(create(&db.pool, name, "", now()).await, Err(OkfError::Refused { .. })), "{name}");
        }
        let duplicate = create(&db.pool, "pg", "", now()).await;

        assert!(matches!(duplicate, Err(OkfError::Refused { reason }) if reason.contains("already exists")));
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_unpublished_work_unless_forced() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        let changeset: i64 = sqlx::query_scalar(
            "INSERT INTO changesets (catalog_id, session_id, actor, state, created_at, updated_at) \
             VALUES ($1, 's', 'a', 'open', $2, $2) RETURNING id",
        )
        .bind(pg.id)
        .bind(now())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO changeset_entries (changeset_id, path, content) VALUES ($1, 'a.md', $2)")
            .bind(changeset)
            .bind(b"x".to_vec())
            .execute(&db.pool)
            .await
            .unwrap();

        let refused = delete(&db.pool, "pg", false).await;
        assert!(matches!(refused, Err(OkfError::Refused { reason }) if reason.contains("--force")));

        delete(&db.pool, "pg", true).await.unwrap();

        assert!(list(&db.pool).await.unwrap().is_empty());
        let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM changeset_entries")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(entries, 0);
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_open_changeset_does_not_block_delete() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        sqlx::query(
            "INSERT INTO changesets (catalog_id, session_id, actor, state, created_at, updated_at) \
             VALUES ($1, 's', 'a', 'open', $2, $2)",
        )
        .bind(pg.id)
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();

        delete(&db.pool, "pg", false).await.unwrap();
    }
}
```

`src/cli.rs` tests (add):

```rust
    #[test]
    fn catalog_subcommands_parse() {
        let parsed = Cli::try_parse_from(["okf-mcp", "catalog", "create", "pg", "--description", "D"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Catalog(CatalogCommand::Create { ref name, ref description }))
                if name == "pg" && description == "D"
        ));
        let parsed = Cli::try_parse_from(["okf-mcp", "catalog", "delete", "pg", "--force"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::Catalog(CatalogCommand::Delete { force: true, .. }))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_create_runs_against_the_database() {
        let db = crate::db::pg_support::TestDb::start().await;
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
        };

        run(cli(Some(db.url.clone()), Command::Catalog(create)), &FixedClock(chrono::Utc::now()))
            .await
            .unwrap();

        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM catalogs")
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(names, ["pg"]);
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib store::catalogs cli`
Expected: compile errors (`unresolved module store`).

- [ ] **Step 3: Implement**

`src/error.rs`:

```rust
    /// The named catalog does not exist (from M4, also: the key may not reach it).
    #[error("catalog {catalog:?} does not exist or is not available")]
    CatalogNotSelected {
        /// The catalog name as given.
        catalog: String,
    },
    /// An administrative request was refused; the reason says why and what to do.
    #[error("{reason}")]
    Refused {
        /// Why.
        reason: String,
    },
```

`src/store/mod.rs`:

```rust
//! The catalog store: async functions over a Postgres pool, one module per
//! group of tables (design "Storage"). Every function that writes takes `now`
//! from the caller, so every stored timestamp comes from the server clock.

pub mod catalogs;
```

`src/store/catalogs.rs`:

```rust
//! Catalog rows, the per-catalog lock, and the administrative generation.

use chrono::{DateTime, Utc};
// `PgExecutor` is "anything a query can run on": a pool, a connection or a
// transaction. Functions generic over it work inside or outside a transaction.
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::OkfError;

/// A catalog row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Catalog {
    /// Database id.
    pub id: i64,
    /// Unique name, `[a-z0-9-]+`.
    pub name: String,
    /// Free text.
    pub description: String,
    /// Bumped by every publish and import.
    pub generation: i64,
    /// When it was created (server clock).
    pub created_at: DateTime<Utc>,
}

/// Is `name` a valid catalog name (non-empty, ASCII lowercase letters, digits, `-`)?
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Creates an empty catalog.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the new catalog's name.
/// * `description` - free text.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new row.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken name; [`OkfError::Database`]
/// otherwise.
pub async fn create(
    pool: &PgPool,
    name: &str,
    description: &str,
    now: DateTime<Utc>,
) -> Result<Catalog, OkfError> {
    if !is_valid_name(name) {
        return Err(OkfError::Refused {
            reason: format!("catalog name {name:?} must be lowercase letters, digits and '-'"),
        });
    }
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query_as::<_, Catalog>(
        "INSERT INTO catalogs (name, description, created_at) VALUES ($1, $2, $3) \
         RETURNING id, name, description, generation, created_at",
    )
    .bind(name)
    .bind(description)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let catalog = match inserted {
        Ok(catalog) => catalog,
        Err(error) if is_unique_violation(&error) => {
            return Err(OkfError::Refused {
                reason: format!("catalog {name:?} already exists"),
            })
        }
        Err(error) => return Err(error.into()),
    };
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(catalog)
}

/// Every catalog, by name.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<Catalog>, OkfError> {
    let catalogs = sqlx::query_as::<_, Catalog>(
        "SELECT id, name, description, generation, created_at FROM catalogs ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(catalogs)
}

/// The catalog called `name`.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is none; [`OkfError::Database`].
pub async fn find<'e>(executor: impl PgExecutor<'e>, name: &str) -> Result<Catalog, OkfError> {
    sqlx::query_as::<_, Catalog>(
        "SELECT id, name, description, generation, created_at FROM catalogs WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| OkfError::CatalogNotSelected {
        catalog: name.to_string(),
    })
}

/// Deletes a catalog with its documents, history, changesets and grants
/// (cascading foreign keys).
///
/// # Arguments
/// * `force` - delete even with unpublished changes in an open changeset.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is no such catalog;
/// [`OkfError::Refused`] if it has unpublished changes and `force` is false.
pub async fn delete(pool: &PgPool, name: &str, force: bool) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = find(&mut *tx, name).await?;
    lock(&mut tx, catalog.id).await?;
    let unpublished: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM changesets c \
         JOIN changeset_entries e ON e.changeset_id = c.id \
         WHERE c.catalog_id = $1 AND c.state = 'open')",
    )
    .bind(catalog.id)
    .fetch_one(&mut *tx)
    .await?;
    if unpublished && !force {
        return Err(OkfError::Refused {
            reason: format!(
                "catalog {name:?} has unpublished changes; pass --force to delete them too"
            ),
        });
    }
    sqlx::query("DELETE FROM catalogs WHERE id = $1")
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Takes the catalog's publish lock for the rest of the transaction (design
/// "Publish"): publishes, imports and deletes of one catalog run one at a time.
///
/// # Errors
/// [`OkfError::Database`].
pub(crate) async fn lock(conn: &mut PgConnection, catalog_id: i64) -> Result<(), OkfError> {
    // The two-key form, under this crate's namespace, so the lock cannot
    // collide with another application's advisory locks on a shared database.
    let catalog_key =
        i32::try_from(catalog_id).expect("catalog ids come from an identity starting at 1");
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(CATALOG_LOCK_NAMESPACE)
        .bind(catalog_key)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The first key of every catalog lock (`okf` in ASCII), separating this
/// crate's advisory locks from anyone else's.
const CATALOG_LOCK_NAMESPACE: i32 = 0x006f_6b66;

/// Bumps `server_state.admin_generation` inside the caller's transaction.
///
/// # Errors
/// [`OkfError::Database`].
pub(crate) async fn bump_admin_generation(conn: &mut PgConnection) -> Result<(), OkfError> {
    sqlx::query("UPDATE server_state SET admin_generation = admin_generation + 1")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The current administrative generation (design "Noticing changes made
/// outside the server").
///
/// # Errors
/// [`OkfError::Database`].
pub async fn admin_generation<'e>(executor: impl PgExecutor<'e>) -> Result<i64, OkfError> {
    let generation = sqlx::query_scalar("SELECT admin_generation FROM server_state")
        .fetch_one(executor)
        .await?;
    Ok(generation)
}

/// Is this a unique-constraint violation (SQLSTATE 23505)?
pub(crate) fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|db_error| db_error.is_unique_violation())
}
```

`lock(&mut tx, ..)` passes a `&mut Transaction`, which Rust converts to `&mut PgConnection` automatically (deref coercion).

`src/cli.rs`: add the subcommand and its handler, and use `clock` for real (delete Task 4's `let _ = clock;` line; `_config` stays until Task 8):

```rust
use crate::store::catalogs;

    // In `enum Command`:
    /// Create, list or delete catalogs.
    #[command(subcommand)]
    Catalog(CatalogCommand),

/// `okf-mcp catalog ...`.
#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    /// Create an empty catalog.
    Create {
        /// Its name (`[a-z0-9-]+`).
        name: String,
        /// Free text.
        #[arg(long, default_value = "")]
        description: String,
    },
    /// List catalogs.
    List,
    /// Delete a catalog with its documents, history, pending changes and grants.
    Delete {
        /// Its name.
        name: String,
        /// Delete even if a session has unpublished changes.
        #[arg(long)]
        force: bool,
    },
}

        // In `run`'s match:
        Command::Catalog(command) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            run_catalog(&pool, command, clock).await?;
        }

/// Runs `okf-mcp catalog ...`.
async fn run_catalog(pool: &PgPool, command: CatalogCommand, clock: &dyn Clock) -> anyhow::Result<()> {
    match command {
        CatalogCommand::Create { name, description } => {
            let catalog = catalogs::create(pool, &name, &description, clock.now()).await?;
            println!("created catalog {}", catalog.name);
        }
        CatalogCommand::List => {
            for catalog in catalogs::list(pool).await? {
                println!(
                    "{}\tgeneration {}\t{}",
                    catalog.name, catalog.generation, catalog.description
                );
            }
        }
        CatalogCommand::Delete { name, force } => {
            catalogs::delete(pool, &name, force).await?;
            println!("deleted catalog {name}");
        }
    }
    Ok(())
}
```

`src/lib.rs`: `pub mod store;`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib store::catalogs cli`
Expected: PASS.

- [ ] **Step 5: Gates, changelog, commit**

Run the gates. Changelog: Added: `store::catalogs` (`create`, `list`, `find`, `delete`, `admin_generation`, the per-catalog lock) and `okf-mcp catalog create|list|delete`. Commit:

```bash
git add src/store src/lib.rs src/error.rs src/cli.rs changelog/
git commit -m "store: catalogs, the per-catalog lock and okf-mcp catalog"
```

---

### Task 6: API keys: `keys`, `store::keys` and `okf-mcp key`

Keys are generated from the OS random source, shown once and stored only as SHA-256 (`bytea`). Grants tie a key to catalogs, or `--all`. Every change bumps `admin_generation`. M4's auth layer loads the keys with `store::keys::list` and compares with `keys::matches`.

**Files:**
- Modify: `Cargo.toml` (`getrandom`, `base64`, `sha2`, `subtle`)
- Create: `src/keys.rs`; modify `src/lib.rs` (`pub mod keys;`)
- Create: `src/store/keys.rs`; modify `src/store/mod.rs` (`pub mod keys;`)
- Modify: `src/cli.rs` (`key` subcommand)
- Create: `changelog/<ts>-m3-task-06-keys.md`

- [ ] **Step 1: Dependencies**

```toml
# API keys: 32 bytes from the OS random source, base64url-encoded, stored
# as SHA-256, compared in constant time.
getrandom = "0.4.3"
base64 = "0.23.1"
sha2 = "0.11.1"
subtle = "2.6.1"
```

- [ ] **Step 2: Write the failing tests**

`src/keys.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_prefixed_url_safe_and_unique() {
        let key = generate().unwrap();

        assert!(key.starts_with(KEY_PREFIX));
        assert_eq!(key.len(), KEY_PREFIX.len() + 43);
        assert!(key[KEY_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(key, generate().unwrap());
    }

    #[test]
    fn hash_is_sha256() {
        // The FIPS 180-2 test vector for "abc".
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(hash("abc"), expected);
    }

    #[test]
    fn matches_compares_hashes() {
        let key = generate().unwrap();
        let stored = hash(&key);

        assert!(matches(&stored, &key));
        assert!(!matches(&stored, "okf_wrong"));
        assert!(!matches(&stored[..31], &key), "a short hash is false, not a panic");
    }
}
```

`src/store/keys.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::catalogs;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    fn new_key<'a>(id: &'a str, grants: Grants<'a>, default: Option<&'a str>) -> NewKey<'a> {
        NewKey {
            id,
            access: Access::Write,
            grants,
            default_catalog: default,
            actor: "claude-code/test",
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_stores_only_the_hash_and_the_grants() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        catalogs::create(&db.pool, "work", "", now()).await.unwrap();
        let names = ["work".to_string(), "pg".to_string()];

        let key = create(&db.pool, &new_key("laptop", Grants::Catalogs(&names), Some("pg")), now())
            .await
            .unwrap();

        let keys = list(&db.pool).await.unwrap();
        assert_eq!(keys.len(), 1);
        let record = &keys[0];
        assert_eq!(record.id, "laptop");
        assert_eq!(record.access, Access::Write);
        assert_eq!(record.catalogs, ["pg", "work"]);
        assert_eq!(record.default_catalog.as_deref(), Some("pg"));
        assert!(crate::keys::matches(&record.sha256, &key));
        assert_eq!(record.created_at, now());
        assert_eq!(record.revoked_at, None);
        assert_eq!(catalogs::admin_generation(&db.pool).await.unwrap(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_requests_are_refused() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        let pg = ["pg".to_string()];
        let missing = ["nope".to_string()];
        create(&db.pool, &new_key("a", Grants::All, None), now()).await.unwrap();

        let unknown = create(&db.pool, &new_key("b", Grants::Catalogs(&missing), None), now()).await;
        let duplicate = create(&db.pool, &new_key("a", Grants::All, None), now()).await;
        let bad_id = create(&db.pool, &new_key("has space", Grants::All, None), now()).await;
        let bad_default = create(&db.pool, &new_key("c", Grants::Catalogs(&pg), Some("work")), now()).await;

        assert!(matches!(unknown, Err(OkfError::CatalogNotSelected { .. })));
        assert!(matches!(duplicate, Err(OkfError::Refused { reason }) if reason.contains("already exists")));
        assert!(matches!(bad_id, Err(OkfError::Refused { .. })));
        assert!(matches!(bad_default, Err(OkfError::Refused { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_grant_and_ungrant() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        catalogs::create(&db.pool, "work", "", now()).await.unwrap();
        let pg = ["pg".to_string()];
        create(&db.pool, &new_key("k", Grants::Catalogs(&pg), Some("pg")), now()).await.unwrap();

        grant(&db.pool, "k", "work").await.unwrap();
        grant(&db.pool, "k", "work").await.unwrap(); // already granted: a no-op
        ungrant(&db.pool, "k", "pg").await.unwrap();
        revoke(&db.pool, "k", now()).await.unwrap();

        let record = &list(&db.pool).await.unwrap()[0];
        assert_eq!(record.catalogs, ["work"]);
        // Losing the default catalog's grant clears the default.
        assert_eq!(record.default_catalog, None);
        assert_eq!(record.revoked_at, Some(now()));
        assert!(matches!(revoke(&db.pool, "k", now()).await, Err(OkfError::Refused { .. })));
        assert!(matches!(grant(&db.pool, "nope", "pg").await, Err(OkfError::Refused { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_catalog_clears_it_as_a_default() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        create(&db.pool, &new_key("k", Grants::All, Some("pg")), now()).await.unwrap();
        let unknown_default = create(&db.pool, &new_key("j", Grants::All, Some("nope")), now()).await;

        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();

        // A re-created catalog of the same name is not the old default.
        assert_eq!(list(&db.pool).await.unwrap()[0].default_catalog, None);
        assert!(matches!(unknown_default, Err(OkfError::CatalogNotSelected { .. })));
    }
}
```

`src/cli.rs` (add):

```rust
    #[test]
    fn key_create_needs_catalogs_or_all_but_not_both() {
        let parsed = Cli::try_parse_from(["okf-mcp", "key", "create", "k", "--access", "write", "--catalogs", "pg,work"]).unwrap();
        let Some(Command::Key(KeyCommand::Create(args))) = parsed.command else {
            panic!("expected key create");
        };
        assert_eq!((args.access, args.catalogs, args.all), (Access::Write, vec!["pg".to_string(), "work".to_string()], false));

        let both = Cli::try_parse_from(["okf-mcp", "key", "create", "k", "--access", "read", "--catalogs", "pg", "--all"]);
        let neither = Cli::try_parse_from(["okf-mcp", "key", "create", "k", "--access", "read"]);
        assert_eq!(both.unwrap_err().kind(), clap::error::ErrorKind::ArgumentConflict);
        assert_eq!(neither.unwrap_err().kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib keys cli`
Expected: compile errors (`unresolved module keys`).

- [ ] **Step 4: Implement**

`src/keys.rs`:

```rust
//! API keys: generation, hashing and constant-time comparison. A key is
//! shown once, when created; only its SHA-256 is stored, and keys are never
//! logged.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
// `Engine as _` imports the trait for its `encode` method without binding a name.
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::OkfError;

/// Every key starts with this, so people and secret scanners can recognize one.
pub const KEY_PREFIX: &str = "okf_";

/// Generates a new key: [`KEY_PREFIX`] plus 32 random bytes in unpadded base64url.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn generate() -> Result<String, OkfError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| OkfError::Io {
        path: "<os random source>".to_string(),
        source: std::io::Error::other(error),
    })?;
    Ok(format!("{KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// The SHA-256 of a key's text, as stored in `api_keys.sha256`.
pub fn hash(key: &str) -> [u8; 32] {
    // `digest` returns a fixed-size array type; `.into()` turns it into `[u8; 32]`.
    Sha256::digest(key.as_bytes()).into()
}

/// Does a presented key match a stored hash? Compared in constant time, so
/// the comparison's duration says nothing about how much of it matched.
pub fn matches(stored: &[u8], presented: &str) -> bool {
    stored.ct_eq(&hash(presented)).into()
}
```

`src/store/keys.rs`:

```rust
//! API key rows and their catalog grants.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::store::catalogs::{self, bump_admin_generation, is_unique_violation};

/// What a key may do. Each level includes the ones before it, so the
/// derived `Ord` (`Read < Write < Admin`) answers "at least write?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Access {
    /// The read tools and export.
    Read,
    /// Read, plus the write tools and publish.
    Write,
    /// Write, plus import into granted catalogs and adopting any stranded changeset.
    Admin,
}

impl Access {
    /// The spelling stored in `api_keys.access`.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Admin => "admin",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Returns
    /// `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "read" => Some(Access::Read),
            "write" => Some(Access::Write),
            "admin" => Some(Access::Admin),
            _ => None,
        }
    }
}

/// Which catalogs a new key reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grants<'a> {
    /// Every catalog, present and future.
    All,
    /// These catalogs, by name.
    Catalogs(&'a [String]),
}

/// A key to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewKey<'a> {
    /// Unique id, `[A-Za-z0-9._-]+`.
    pub id: &'a str,
    /// Its access level.
    pub access: Access,
    /// Its catalogs.
    pub grants: Grants<'a>,
    /// Its default catalog; must be granted.
    pub default_catalog: Option<&'a str>,
    /// The actor its writes are recorded as (`claude-code/claude-opus-5-5`).
    pub actor: &'a str,
}

/// A stored key (never the key itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRecord {
    /// Its id.
    pub id: String,
    /// SHA-256 of the key.
    pub sha256: Vec<u8>,
    /// Its access level.
    pub access: Access,
    /// Granted every catalog.
    pub all_catalogs: bool,
    /// Granted catalogs by name, sorted (empty with `all_catalogs`).
    pub catalogs: Vec<String>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it was revoked; a revoked key authenticates nothing.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// The row as selected, before `access` is parsed.
#[derive(sqlx::FromRow)]
struct KeyRow {
    id: String,
    sha256: Vec<u8>,
    access: String,
    all_catalogs: bool,
    catalogs: Vec<String>,
    default_catalog: Option<String>,
    actor: String,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

/// Is `id` a valid key id?
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Creates a key and its grants.
///
/// # Returns
/// The key itself: print it once; it is not stored and cannot be shown again.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken id, or a default catalog that
/// is not granted; [`OkfError::CatalogNotSelected`] for an unknown catalog.
pub async fn create(pool: &PgPool, new: &NewKey<'_>, now: DateTime<Utc>) -> Result<String, OkfError> {
    if !is_valid_id(new.id) {
        return Err(OkfError::Refused {
            reason: format!("key id {:?} must be letters, digits, '.', '_' and '-'", new.id),
        });
    }
    let mut tx = pool.begin().await?;
    let granted: Vec<i64> = match new.grants {
        Grants::All => Vec::new(),
        Grants::Catalogs(names) => {
            let mut ids = Vec::with_capacity(names.len());
            for name in names {
                ids.push(catalogs::find(&mut *tx, name).await?.id);
            }
            ids
        }
    };
    let default_catalog_id = match new.default_catalog {
        None => None,
        Some(default) => {
            let granted = match new.grants {
                Grants::All => true,
                Grants::Catalogs(names) => names.iter().any(|name| name == default),
            };
            if !granted {
                return Err(OkfError::Refused {
                    reason: format!("default catalog {default:?} is not granted to the key"),
                });
            }
            // An unknown catalog is `CatalogNotSelected`; any other error propagates.
            Some(catalogs::find(&mut *tx, default).await?.id)
        }
    };
    let key = crate::keys::generate()?;
    let inserted = sqlx::query(
        "INSERT INTO api_keys (id, sha256, access, all_catalogs, default_catalog_id, actor, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new.id)
    .bind(crate::keys::hash(&key).to_vec())
    .bind(new.access.as_str())
    .bind(matches!(new.grants, Grants::All))
    .bind(default_catalog_id)
    .bind(new.actor)
    .bind(now)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(error) if is_unique_violation(&error) => {
            return Err(OkfError::Refused {
                reason: format!("key {:?} already exists", new.id),
            })
        }
        Err(error) => return Err(error.into()),
    }
    for catalog_id in granted {
        sqlx::query(
            "INSERT INTO key_catalogs (key_id, catalog_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(new.id)
        .bind(catalog_id)
        .execute(&mut *tx)
        .await?;
    }
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(key)
}

/// Every key, by id, with its granted catalogs.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<KeyRecord>, OkfError> {
    let rows = sqlx::query_as::<_, KeyRow>(
        "SELECT k.id, k.sha256, k.access, k.all_catalogs, d.name AS default_catalog, k.actor, \
                k.created_at, k.revoked_at, \
                COALESCE(array_agg(c.name ORDER BY c.name) FILTER (WHERE c.name IS NOT NULL), \
                         '{}'::text[]) AS catalogs \
         FROM api_keys k \
         LEFT JOIN catalogs d ON d.id = k.default_catalog_id \
         LEFT JOIN key_catalogs g ON g.key_id = k.id \
         LEFT JOIN catalogs c ON c.id = g.catalog_id \
         GROUP BY k.id, d.name ORDER BY k.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| KeyRecord {
            id: row.id,
            sha256: row.sha256,
            access: Access::parse(&row.access).expect("the schema checks api_keys.access"),
            all_catalogs: row.all_catalogs,
            catalogs: row.catalogs,
            default_catalog: row.default_catalog,
            actor: row.actor,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        })
        .collect())
}

/// Revokes a key: it authenticates nothing from then on (M4 notices within
/// `poll_interval`).
///
/// # Errors
/// [`OkfError::Refused`] if there is no such key or it is already revoked.
pub async fn revoke(pool: &PgPool, id: &str, now: DateTime<Utc>) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let revoked = sqlx::query("UPDATE api_keys SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL")
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    if revoked.rows_affected() == 0 {
        return Err(OkfError::Refused {
            reason: format!("no active key {id:?}"),
        });
    }
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Grants a key a catalog (a no-op if already granted).
///
/// # Errors
/// [`OkfError::Refused`] if there is no such key;
/// [`OkfError::CatalogNotSelected`] if there is no such catalog.
pub async fn grant(pool: &PgPool, id: &str, catalog: &str) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    require_key(&mut tx, id).await?;
    sqlx::query("INSERT INTO key_catalogs (key_id, catalog_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Removes a key's grant of a catalog; if it was the key's default catalog,
/// the default is cleared.
///
/// # Errors
/// As [`grant`].
pub async fn ungrant(pool: &PgPool, id: &str, catalog: &str) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    require_key(&mut tx, id).await?;
    sqlx::query("DELETE FROM key_catalogs WHERE key_id = $1 AND catalog_id = $2")
        .bind(id)
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE api_keys SET default_catalog_id = NULL WHERE id = $1 AND default_catalog_id = $2")
        .bind(id)
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Refuses an unknown key id.
async fn require_key(conn: &mut sqlx::PgConnection, id: &str) -> Result<(), OkfError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM api_keys WHERE id = $1)")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    if exists {
        Ok(())
    } else {
        Err(OkfError::Refused {
            reason: format!("no key {id:?}"),
        })
    }
}
```

`src/cli.rs`:

```rust
use clap::Args;
use crate::store::keys::{self, Access, Grants, NewKey};

    // In `enum Command`:
    /// Create, list, revoke or grant API keys.
    #[command(subcommand)]
    Key(KeyCommand),

/// `okf-mcp key ...`.
#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Create a key and print it once.
    Create(KeyCreateArgs),
    /// List keys (never the keys themselves).
    List,
    /// Revoke a key.
    Revoke {
        /// The key's id.
        id: String,
    },
    /// Grant a key a catalog.
    Grant {
        /// The key's id.
        id: String,
        /// The catalog.
        catalog: String,
    },
    /// Remove a key's grant of a catalog.
    Ungrant {
        /// The key's id.
        id: String,
        /// The catalog.
        catalog: String,
    },
}

/// `okf-mcp key create`'s options.
#[derive(Debug, Args)]
pub struct KeyCreateArgs {
    /// Unique id (`[A-Za-z0-9._-]+`).
    pub id: String,
    /// Access level.
    #[arg(long, value_enum)]
    pub access: Access,
    /// Granted catalogs, comma-separated.
    #[arg(long, value_delimiter = ',', required_unless_present = "all", conflicts_with = "all")]
    pub catalogs: Vec<String>,
    /// Grant every catalog, present and future.
    #[arg(long)]
    pub all: bool,
    /// The key's default catalog.
    #[arg(long)]
    pub default: Option<String>,
    /// The actor its writes are recorded as (default: the id).
    #[arg(long)]
    pub actor: Option<String>,
}

        // In `run`'s match:
        Command::Key(command) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            run_key(&pool, command, clock).await?;
        }

/// Runs `okf-mcp key ...`.
async fn run_key(pool: &PgPool, command: KeyCommand, clock: &dyn Clock) -> anyhow::Result<()> {
    match command {
        KeyCommand::Create(args) => {
            let grants = if args.all {
                Grants::All
            } else {
                Grants::Catalogs(&args.catalogs)
            };
            let new = NewKey {
                id: &args.id,
                access: args.access,
                grants,
                default_catalog: args.default.as_deref(),
                actor: args.actor.as_deref().unwrap_or(&args.id),
            };
            let key = keys::create(pool, &new, clock.now()).await?;
            // The key goes to stdout alone, so `$(okf-mcp key create ...)` captures it.
            println!("{key}");
            eprintln!("store this key now: it is not shown again");
        }
        KeyCommand::List => {
            for key in keys::list(pool).await? {
                let catalogs = if key.all_catalogs {
                    "*".to_string()
                } else {
                    key.catalogs.join(",")
                };
                let state = if key.revoked_at.is_some() { "revoked" } else { "active" };
                println!(
                    "{}\t{}\t{}\t{catalogs}\tdefault {}\t{}",
                    key.id,
                    key.access.as_str(),
                    state,
                    key.default_catalog.as_deref().unwrap_or("-"),
                    key.actor
                );
            }
        }
        KeyCommand::Revoke { id } => {
            keys::revoke(pool, &id, clock.now()).await?;
            println!("revoked key {id}");
        }
        KeyCommand::Grant { id, catalog } => {
            keys::grant(pool, &id, &catalog).await?;
            println!("granted {catalog} to key {id}");
        }
        KeyCommand::Ungrant { id, catalog } => {
            keys::ungrant(pool, &id, &catalog).await?;
            println!("removed {catalog} from key {id}");
        }
    }
    Ok(())
}
```

In the design's "Storage" table, the `api_keys` row's `default_catalog` becomes `default_catalog_id` (a reference to `catalogs`, cleared when that catalog is deleted).

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib keys cli`
Expected: PASS.

- [ ] **Step 6: Gates, changelog, commit**

Run the gates. Changelog: Added: `keys` (generate, hash, constant-time `matches`), `store::keys` (create, list, revoke, grant, ungrant), `okf-mcp key ...`. Commit:

```bash
git add Cargo.toml Cargo.lock src/keys.rs src/store src/lib.rs src/cli.rs docs/superpowers/specs changelog/
git commit -m "keys: API key generation, storage, grants and okf-mcp key"
```

---

### Task 7: `archive`: tarballs and directories, with the import rules

Pure reading and writing of catalog files (design "Import" and "Export"). Reading applies every import rule and lists every problem at once. The size cap counts the decompressed stream, so a small upload cannot expand without bound (verified fact 7).

**Files:**
- Modify: `Cargo.toml` (`tar`, `flate2`)
- Create: `src/archive.rs`; modify `src/lib.rs` (`pub mod archive;`)
- Modify: `src/error.rs` (`ImportRefused`)
- Create: `changelog/<ts>-m3-task-07-archive.md`

- [ ] **Step 1: Dependencies**

```toml
# Catalog archives: .tar.gz import and export.
tar = "0.4.46"
flate2 = "1.1.10"
```

- [ ] **Step 2: Write the failing tests**

`src/archive.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use flate2::write::GzEncoder;

    const NO_LIMIT: u64 = u64::MAX;

    fn stored(content: &[u8], executable: bool) -> StoredFile {
        StoredFile {
            content: content.to_vec(),
            executable,
        }
    }

    fn modified() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    /// A tarball built entry by entry with raw header names, as a hostile or
    /// foreign tool might write it.
    fn raw_tarball(entries: &[(EntryType, &[u8], u32, &[u8])]) -> Vec<u8> {
        let mut builder = Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (entry_type, name, mode, content) in entries {
            let mut header = Header::new_ustar();
            header.set_entry_type(*entry_type);
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_size(content.len() as u64);
            header.set_mode(*mode);
            if matches!(entry_type, EntryType::Symlink | EntryType::Link) {
                header.set_link_name("index.md").unwrap();
            }
            header.set_cksum();
            builder.append(&header, *content).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn an_export_reads_back_exactly() {
        let long = format!("{}/x.md", "d".repeat(300));
        let files = [
            ExportFile { path: "index.md", content: b"# Index\n", executable: false, modified: modified() },
            ExportFile { path: "tools/run.sh", content: b"#!/bin/sh\n\0\xff", executable: true, modified: modified() },
            ExportFile { path: &long, content: b"long", executable: false, modified: modified() },
        ];

        let tarball = write_tarball(Vec::new(), &files).unwrap();
        let read = read_tarball(tarball.as_slice(), NO_LIMIT).unwrap();

        assert_eq!(read.len(), 3);
        assert_eq!(read["tools/run.sh"], stored(b"#!/bin/sh\n\0\xff", true));
        assert_eq!(read[long.as_str()], stored(b"long", false));
        // Export writes 0644/0755 and the documents' modification times.
        let mut archive = Archive::new(GzDecoder::new(tarball.as_slice()));
        let modes: Vec<(u32, u64)> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.header().mode().unwrap(), entry.header().mtime().unwrap())
            })
            .collect();
        let mtime = u64::try_from(modified().timestamp()).unwrap();
        assert_eq!(modes, [(0o644, mtime), (0o755, mtime), (0o644, mtime)]);
    }

    #[test]
    fn git_archive_extras_are_skipped_and_modes_decide_the_execute_bit() {
        let tarball = raw_tarball(&[
            (EntryType::XGlobalHeader, b"pax_global_header", 0o666, b"52 comment=67ca473b\n"),
            (EntryType::Directory, b"infra/", 0o775, b""),
            (EntryType::Regular, b"infra/run.sh", 0o775, b"#!/bin/sh\n"),
            (EntryType::Regular, b"infra/a.md", 0o664, b"# A\n"),
        ]);

        let read = read_tarball(tarball.as_slice(), NO_LIMIT).unwrap();

        assert_eq!(read.keys().collect::<Vec<_>>(), ["infra/a.md", "infra/run.sh"]);
        assert!(read["infra/run.sh"].executable);
        assert!(!read["infra/a.md"].executable);
    }

    #[test]
    fn every_bad_entry_is_listed_and_the_import_refused() {
        let tarball = raw_tarball(&[
            (EntryType::Symlink, b"link", 0o644, b""),
            (EntryType::Link, b"hard", 0o644, b""),
            (EntryType::Fifo, b"fifo", 0o644, b""),
            (EntryType::Regular, b"../escape", 0o644, b""),
            (EntryType::Regular, b"/abs", 0o644, b""),
            (EntryType::Regular, b"bad\xffname", 0o644, b""),
            (EntryType::Regular, b"./a", 0o644, b""),
            (EntryType::Regular, b"a//b", 0o644, b""),
            (EntryType::Regular, b"ok.md", 0o644, b"x"),
            (EntryType::Regular, b"ok.md", 0o644, b"y"),
        ]);

        let refused = read_tarball(tarball.as_slice(), NO_LIMIT);

        let Err(OkfError::ImportRefused { problems }) = refused else {
            panic!("expected ImportRefused, got {refused:?}");
        };
        assert_eq!(problems.len(), 9, "{problems:#?}");
        assert!(problems[0].starts_with("link: "));
        assert!(problems[8].contains("more than once"));
    }

    #[test]
    fn the_cap_counts_uncompressed_bytes() {
        let big = vec![b'a'; 1024 * 1024];
        let files = [ExportFile { path: "big.txt", content: &big, executable: false, modified: modified() }];
        let tarball = write_tarball(Vec::new(), &files).unwrap();
        assert!(tarball.len() < 16 * 1024, "compresses well");

        let refused = read_tarball(tarball.as_slice(), 512 * 1024);

        assert!(matches!(refused, Err(OkfError::ImportRefused { problems }) if problems[0].contains("limit")));
        assert!(read_tarball(tarball.as_slice(), 4 * 1024 * 1024).is_ok());
    }

    #[test]
    fn garbage_is_not_an_archive() {
        let refused = read_tarball(&b"not a tarball"[..], NO_LIMIT);
        assert!(matches!(refused, Err(OkfError::ImportRefused { problems }) if problems[0].contains(".tar.gz")));
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_reads_every_file_but_git_and_caches() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, content: &[u8]| {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        };
        write("index.md", b"# Index\n");
        write("tools/run.sh", b"#!/bin/sh\n");
        write("img/logo.png", b"\x89PNG\0\xff");
        write(".claude/skills/s/SKILL.md", b"skill");
        write(".gitattributes", b"log.md merge=union\n");
        write(".git/config", b"[core]");
        write("sub/.git", b"gitdir: elsewhere");
        write("tools/__pycache__/okf.cpython-314.pyc", b"\0");
        write(".worktrees/s1/index.md", b"a session");
        std::fs::set_permissions(root.join("tools/run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let read = read_directory(root, NO_LIMIT).unwrap();

        assert_eq!(
            read.keys().collect::<Vec<_>>(),
            [".claude/skills/s/SKILL.md", ".gitattributes", "img/logo.png", "index.md", "tools/run.sh"]
        );
        assert!(read["tools/run.sh"].executable);
        assert_eq!(read["img/logo.png"].content, b"\x89PNG\0\xff");
        assert!(matches!(read_directory(root, 8), Err(OkfError::ImportRefused { .. })));
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib archive`
Expected: compile error, `unresolved module archive`.

- [ ] **Step 4: Implement**

`src/error.rs`:

```rust
    /// An import was refused; every problem found is listed.
    // `.problems` in the format arguments refers to the field (thiserror syntax).
    #[error("import refused: {}", .problems.join("; "))]
    ImportRefused {
        /// One line per problem: the entry, and what is wrong with it.
        problems: Vec<String>,
    },
```

`src/archive.rs`:

```rust
//! Catalog archives (design "Import" and "Export"): reading a `.tar.gz` or a
//! directory into stored files with the import rules, and writing a
//! `.tar.gz`. Pure: no database.
//!
//! Import rules for a tarball: only regular files become documents; the pax
//! global header `git archive` writes and directory entries are skipped; any
//! other entry type (symlink, hardlink, device, FIFO), a path that is not
//! UTF-8, absolute, has a `..` component or is not in normal form, and a
//! duplicate path each refuse the whole import, and every problem is listed.
//! The uncompressed total is capped.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Archive, Builder, EntryType, Header};

use crate::error::OkfError;
use crate::okf::paths::basename;
use crate::okf::workspace::{DiskWorkspace, StoredFile, Workspace};

/// A catalog's files: catalog-relative path to file, sorted by path.
pub type Files = BTreeMap<String, StoredFile>;

/// One file to export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportFile<'a> {
    /// Catalog-relative path.
    pub path: &'a str,
    /// The exact bytes.
    pub content: &'a [u8],
    /// Written with mode 0755 when set, else 0644.
    pub executable: bool,
    /// The entry's modification time.
    pub modified: DateTime<Utc>,
}

/// Reads a `.tar.gz` for import.
///
/// # Arguments
/// * `input` - the gzip-compressed tar stream.
/// * `max_bytes` - the largest uncompressed stream allowed (`max_import_bytes`).
///
/// # Returns
/// Every regular file, by path.
///
/// # Errors
/// [`OkfError::ImportRefused`] listing every rule violation, or saying the
/// input is not a readable `.tar.gz` or expands past `max_bytes`.
pub fn read_tarball(input: impl Read, max_bytes: u64) -> Result<Files, OkfError> {
    let mut archive = Archive::new(Capped {
        inner: GzDecoder::new(input),
        remaining: max_bytes,
        exceeded: false,
    });
    let result = read_entries(&mut archive);
    // Check the cap first: once it trips, every later read fails, and the
    // read error says less than this does.
    if archive.into_inner().exceeded {
        return Err(too_large(max_bytes));
    }
    result
}

/// Reads every entry, collecting rule violations instead of stopping at the first.
fn read_entries<R: Read>(archive: &mut Archive<R>) -> Result<Files, OkfError> {
    let mut files = Files::new();
    let mut problems = Vec::new();
    for entry in archive.entries().map_err(not_an_archive)? {
        let mut entry = entry.map_err(not_an_archive)?;
        let raw = entry.path_bytes().into_owned();
        let shown = String::from_utf8_lossy(&raw).into_owned();
        match entry.header().entry_type() {
            // `git archive`'s pax global header (it holds the commit id) and
            // directory entries carry no file.
            EntryType::XGlobalHeader | EntryType::Directory => continue,
            EntryType::Regular => {}
            other => {
                problems.push(format!("{shown}: {other:?} entries are not allowed"));
                continue;
            }
        }
        let path = match checked_path(&raw) {
            Ok(path) => path,
            Err(reason) => {
                problems.push(format!("{shown}: {reason}"));
                continue;
            }
        };
        let mode = entry.header().mode().map_err(not_an_archive)?;
        let mut content = Vec::new();
        entry.read_to_end(&mut content).map_err(not_an_archive)?;
        let file = StoredFile {
            content,
            executable: mode & 0o111 != 0,
        };
        if files.insert(path.clone(), file).is_some() {
            problems.push(format!("{path}: appears more than once"));
        }
    }
    if problems.is_empty() {
        Ok(files)
    } else {
        Err(OkfError::ImportRefused { problems })
    }
}

/// A tar entry name as a catalog path, or why it can't be one.
fn checked_path(raw: &[u8]) -> Result<String, &'static str> {
    let path = std::str::from_utf8(raw).map_err(|_| "the name is not UTF-8")?;
    if path.is_empty() {
        return Err("the name is empty");
    }
    if path.starts_with('/') {
        return Err("absolute paths are not allowed");
    }
    if path.split('/').any(|part| part == "..") {
        return Err("'..' components are not allowed");
    }
    if path.split('/').any(|part| part.is_empty() || part == ".") {
        return Err("the path is not in normal form ('./', '//' or a trailing '/')");
    }
    Ok(path.to_string())
}

/// Reads a catalog directory for import: every file M1's listing contract
/// returns (symlinks only inside the catalog), except `.git` (a directory or
/// a worktree's link file), `.worktrees/` and `__pycache__/`. Dotfiles are kept.
///
/// # Arguments
/// * `root` - the catalog directory (a checkout, say).
/// * `max_bytes` - the largest total content allowed (`max_import_bytes`).
///
/// # Errors
/// [`OkfError::Io`] if the directory or a file can't be read;
/// [`OkfError::ImportRefused`] past `max_bytes`.
pub fn read_directory(root: &Path, max_bytes: u64) -> Result<Files, OkfError> {
    let workspace = DiskWorkspace::new(root)?;
    let mut files = Files::new();
    let mut total: u64 = 0;
    for path in workspace.list_files()? {
        if basename(&path) == ".git" || path.split('/').any(|part| part == "__pycache__") {
            continue;
        }
        let full = workspace.root().join(&path);
        let io_error = |source| OkfError::Io {
            path: path.clone(),
            source,
        };
        let content = std::fs::read(&full).map_err(io_error)?;
        total = total.saturating_add(content.len() as u64);
        if total > max_bytes {
            return Err(too_large(max_bytes));
        }
        let executable = is_executable(&full).map_err(io_error)?;
        files.insert(path, StoredFile { content, executable });
    }
    Ok(files)
}

/// Does the file at `path` have any execute bit set?
#[cfg(unix)]
fn is_executable(path: &Path) -> io::Result<bool> {
    // `PermissionsExt` is the Unix-only extension that exposes the mode bits.
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o111 != 0)
}

/// Elsewhere there are no execute bits.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

/// Writes a `.tar.gz` of `files`, in the order given, with no top-level
/// directory, modes 0644/0755 and each file's modification time.
///
/// # Arguments
/// * `out` - where to write (`impl Write`: a file, stdout, a `Vec<u8>`).
/// * `files` - the files.
///
/// # Returns
/// `out`, after the gzip stream is finished.
///
/// # Errors
/// [`OkfError::Io`] if writing fails.
pub fn write_tarball<W: Write>(out: W, files: &[ExportFile<'_>]) -> Result<W, OkfError> {
    let mut builder = Builder::new(GzEncoder::new(out, Compression::default()));
    for file in files {
        let mut header = Header::new_ustar();
        header.set_entry_type(EntryType::Regular);
        header.set_size(file.content.len() as u64);
        header.set_mode(if file.executable { 0o755 } else { 0o644 });
        // Before 1970 cannot happen for a server-written time; 0 is a safe floor.
        header.set_mtime(u64::try_from(file.modified.timestamp()).unwrap_or(0));
        // `append_data` sets the path (adding a GNU long-name entry for a long
        // one) and the header checksum.
        builder
            .append_data(&mut header, file.path, file.content)
            .map_err(|source| export_error(file.path, source))?;
    }
    let encoder = builder
        .into_inner()
        .map_err(|source| export_error("<archive>", source))?;
    encoder
        .finish()
        .map_err(|source| export_error("<archive>", source))
}

/// A reader that fails once more than `remaining` bytes pass through it, and
/// remembers that it did.
struct Capped<R> {
    inner: R,
    remaining: u64,
    exceeded: bool,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        match self.remaining.checked_sub(read as u64) {
            Some(left) => {
                self.remaining = left;
                Ok(read)
            }
            None => {
                self.exceeded = true;
                Err(io::Error::other("uncompressed size limit exceeded"))
            }
        }
    }
}

fn too_large(max_bytes: u64) -> OkfError {
    OkfError::ImportRefused {
        problems: vec![format!(
            "the archive expands past the {max_bytes}-byte limit (max_import_bytes)"
        )],
    }
}

fn not_an_archive(error: io::Error) -> OkfError {
    OkfError::ImportRefused {
        problems: vec![format!("not a readable .tar.gz: {error}")],
    }
}

fn export_error(path: &str, source: io::Error) -> OkfError {
    OkfError::Io {
        path: path.to_string(),
        source,
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib archive`
Expected: PASS. (If `EntryType`'s `Debug` spelling differs from `Symlink`, adjust the `starts_with("link: ")` assertion only; the message format stays `"<entry>: <Type> entries are not allowed"`.)

- [ ] **Step 6: Gates, changelog, commit**

Run the gates. Changelog: Added: `archive::{read_tarball, read_directory, write_tarball, Files, ExportFile}` with the design's import rules; `OkfError::ImportRefused`. Commit:

```bash
git add Cargo.toml Cargo.lock src/archive.rs src/lib.rs src/error.rs changelog/
git commit -m "archive: tarball and directory reading with the import rules; tarball writing"
```

---

### Task 8: `store::documents`, `store::import` and `okf-mcp import`

Import loads a catalog's files (design "Import"): into an empty catalog, or with `--replace` as the differences, under the catalog's lock, as `import` revisions, bumping the catalog's `generation`. It builds a snapshot of the incoming files to derive each document's columns and to report lint (import never refuses on lint).

**Files:**
- Create: `src/store/documents.rs`, `src/store/import.rs`; modify `src/store/mod.rs`
- Modify: `src/cli.rs` (`import` subcommand; `_config` becomes `config`)
- Create: `changelog/<ts>-m3-task-08-import.md`

- [ ] **Step 1: Write the failing tests**

`src/store/documents.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::snapshot::Snapshot;
    use crate::okf::workspace::MemWorkspace;

    #[test]
    fn derived_columns_come_from_the_frontmatter() {
        let snapshot = Snapshot::build(&MemWorkspace::from_files([
            (
                "infra/time.md",
                "---\ntype: System\ntitle: Time server\nstatus: draft\ntags: [ntp, gps]\n---\nBody\n",
            ),
            ("infra/index.md", "# Infra\n"),
            ("log.md", "# Log\n"),
            ("AGENTS.md", "# Agents\n"),
        ]))
        .unwrap();

        let concept = Derived::of(snapshot.get("infra/time.md"));
        assert_eq!(
            concept,
            Derived {
                kind: Some("concept"),
                concept_type: Some("System".to_string()),
                title: Some("Time server".to_string()),
                status: Some("draft".to_string()),
                tags: vec!["ntp".to_string(), "gps".to_string()],
            }
        );
        assert_eq!(Derived::of(snapshot.get("infra/index.md")).kind, Some("index"));
        assert_eq!(Derived::of(snapshot.get("log.md")).kind, Some("log"));
        assert_eq!(Derived::of(snapshot.get("AGENTS.md")).kind, Some("machinery"));
        assert_eq!(Derived::of(None), Derived::default());
    }
}
```

`src/store/import.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use chrono::TimeZone;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, minute, 0).unwrap()
    }

    fn options(replace: bool, now: DateTime<Utc>) -> ImportOptions<'static> {
        ImportOptions {
            replace,
            actor: "test/import",
            now,
        }
    }

    fn files(entries: &[(&str, &[u8], bool)]) -> Files {
        entries
            .iter()
            .map(|(path, content, executable)| {
                let file = StoredFile {
                    content: content.to_vec(),
                    executable: *executable,
                };
                (path.to_string(), file)
            })
            .collect()
    }

    /// `(path, revision, deleted, content)` of every row, by path.
    async fn rows(pool: &PgPool) -> Vec<(String, i32, bool, Vec<u8>)> {
        sqlx::query_as(
            "SELECT path, revision, deleted_at IS NOT NULL, content FROM documents ORDER BY path",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    const CONCEPT: &[u8] = b"---\ntype: Note\ntitle: A\ntags: [x]\n---\nSee [b](b.md).\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_fills_an_empty_catalog() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        let incoming = files(&[
            ("a.md", CONCEPT, false),
            ("index.md", b"# Index\n- [A](a.md)\n", false),
            ("tools/run.sh", b"#!/bin/sh\n", true),
            ("img/logo.png", b"\x89PNG\0\xff", false),
        ]);

        let report = import(&db.pool, "pg", incoming, &options(false, at(1))).await.unwrap();

        assert_eq!((report.added, report.updated, report.deleted, report.unchanged), (4, 0, 0, 0));
        // a.md links to a missing b.md: reported, not refused.
        assert!(report.lint.errors.iter().any(|f| f.rule == "broken-link"));
        let derived: (Option<String>, Option<String>, Option<String>, Vec<String>, chrono::DateTime<Utc>, String) =
            sqlx::query_as(
                "SELECT kind, concept_type, title, tags, updated_at, updated_by FROM documents WHERE path = 'a.md'",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            derived,
            (Some("concept".into()), Some("Note".into()), Some("A".into()), vec!["x".into()], at(1), "test/import".into())
        );
        let png_kind: Option<String> = sqlx::query_scalar("SELECT kind FROM documents WHERE path = 'img/logo.png'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(png_kind, None);
        let executable: bool = sqlx::query_scalar("SELECT executable FROM documents WHERE path = 'tools/run.sh'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(executable);
        let history: Vec<(String, i32, String)> =
            sqlx::query_as("SELECT path, revision, op FROM revisions ORDER BY path")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(history.len(), 4);
        assert!(history.iter().all(|(_, revision, op)| *revision == 1 && op == "import"));
        assert_eq!(catalogs::find(&db.pool, "pg").await.unwrap().generation, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_import_needs_replace_and_writes_only_differences() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        let first = files(&[("keep.md", b"# Keep\n", false), ("change.md", b"# Old\n", false), ("drop.md", b"# Drop\n", false)]);
        import(&db.pool, "pg", first.clone(), &options(false, at(1))).await.unwrap();

        let refused = import(&db.pool, "pg", first, &options(false, at(2))).await;
        assert!(matches!(refused, Err(OkfError::Refused { reason }) if reason.contains("--replace")));

        let second = files(&[("keep.md", b"# Keep\n", false), ("change.md", b"# New\n", false), ("new.md", b"# New\n", false)]);
        let report = import(&db.pool, "pg", second, &options(true, at(3))).await.unwrap();

        assert_eq!((report.added, report.updated, report.deleted, report.unchanged), (1, 1, 1, 1));
        assert_eq!(
            rows(&db.pool).await,
            [
                ("change.md".to_string(), 2, false, b"# New\n".to_vec()),
                ("drop.md".to_string(), 2, true, Vec::new()),
                ("keep.md".to_string(), 1, false, b"# Keep\n".to_vec()),
                ("new.md".to_string(), 1, false, b"# New\n".to_vec()),
            ]
        );
        let drop_history: Vec<(i32, String, Option<Vec<u8>>)> =
            sqlx::query_as("SELECT revision, op, content FROM revisions WHERE path = 'drop.md' ORDER BY revision")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(drop_history[1], (2, "delete".to_string(), None));
        assert_eq!(catalogs::find(&db.pool, "pg").await.unwrap().generation, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recreated_path_continues_its_revisions_and_a_same_import_changes_nothing() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        import(&db.pool, "pg", files(&[("a.md", b"1", false)]), &options(false, at(1))).await.unwrap();
        import(&db.pool, "pg", files(&[("b.md", b"b", false)]), &options(true, at(2))).await.unwrap();

        import(&db.pool, "pg", files(&[("a.md", b"3", false), ("b.md", b"b", false)]), &options(true, at(3)))
            .await
            .unwrap();
        let again = import(&db.pool, "pg", files(&[("a.md", b"3", false), ("b.md", b"b", false)]), &options(true, at(4)))
            .await
            .unwrap();

        assert_eq!(rows(&db.pool).await[0], ("a.md".to_string(), 3, false, b"3".to_vec()));
        assert_eq!((again.added, again.updated, again.deleted, again.unchanged), (0, 0, 0, 2));
        assert_eq!(catalogs::find(&db.pool, "pg").await.unwrap().generation, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn markdown_that_is_not_text_and_unknown_catalogs_are_refused() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();

        let binary_markdown = import(&db.pool, "pg", files(&[("a.md", b"\xff\xfe", false)]), &options(false, at(1))).await;
        let unknown = import(&db.pool, "nope", files(&[("a.md", b"# A\n", false)]), &options(false, at(1))).await;

        assert!(matches!(binary_markdown, Err(OkfError::ImportRefused { problems }) if problems[0].starts_with("a.md")));
        assert!(matches!(unknown, Err(OkfError::CatalogNotSelected { .. })));
        assert!(rows(&db.pool).await.is_empty());
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib store::documents store::import`
Expected: compile errors (`unresolved module documents`, `import`).

- [ ] **Step 3: Implement `store::documents`**

```rust
//! Document rows: the columns derived from content, and a catalog's
//! published documents.

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

use crate::error::OkfError;
use crate::okf::snapshot::{DocKind, SnapshotDoc};
use crate::okf::yaml::Value;

/// A live (published, not deleted) document.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DocumentRow {
    /// Catalog-relative path.
    pub path: String,
    /// The exact bytes.
    pub content: Vec<u8>,
    /// The execute bit.
    pub executable: bool,
    /// Its current revision.
    pub revision: i32,
    /// When that revision was written (server clock).
    pub updated_at: DateTime<Utc>,
}

/// Every live document of a catalog, by path.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn published<'e>(
    executor: impl PgExecutor<'e>,
    catalog_id: i64,
) -> Result<Vec<DocumentRow>, OkfError> {
    let rows = sqlx::query_as::<_, DocumentRow>(
        "SELECT path, content, executable, revision, updated_at FROM documents \
         WHERE catalog_id = $1 AND deleted_at IS NULL ORDER BY path",
    )
    .bind(catalog_id)
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// The columns derived from a document's content (design "Storage"),
/// recomputed on every write so they never disagree with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Derived {
    /// `concept`, `index`, `log` or `machinery`; `None` for non-markdown files.
    pub kind: Option<&'static str>,
    /// Frontmatter `type`, if a string.
    pub concept_type: Option<String>,
    /// The title (frontmatter `title`, else the first heading).
    pub title: Option<String>,
    /// Frontmatter `status`, if a string.
    pub status: Option<String>,
    /// Frontmatter `tags`: the string items of a list.
    pub tags: Vec<String>,
}

impl Derived {
    /// The derived columns of a snapshot document.
    ///
    /// # Arguments
    /// * `doc` - the document; `None` (a file the snapshot does not hold, such
    ///   as an image or a script) gives all-empty columns.
    pub fn of(doc: Option<&SnapshotDoc>) -> Self {
        let Some(doc) = doc else {
            return Self::default();
        };
        let data = doc.frontmatter().data();
        let text = |key: &str| {
            data.and_then(|data| data.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let tags = match data.and_then(|data| data.get("tags")) {
            Some(Value::Seq(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        Self {
            kind: Some(kind_name(doc.kind())),
            concept_type: text("type"),
            title: doc.title().map(str::to_string),
            status: text("status"),
            tags,
        }
    }
}

/// The spelling stored in `documents.kind`.
fn kind_name(kind: DocKind) -> &'static str {
    match kind {
        DocKind::Concept => "concept",
        DocKind::Index => "index",
        DocKind::Log => "log",
        DocKind::Machinery => "machinery",
    }
}
```

- [ ] **Step 4: Implement `store::import`**

```rust
//! Import (design "Import"): load a catalog's files into the database.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

use crate::archive::Files;
use crate::error::OkfError;
use crate::okf::analysis::lint::{lint, LintReport};
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::catalogs;
use crate::store::documents::Derived;

/// How to import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOptions<'a> {
    /// Replace a non-empty catalog's contents (write the differences).
    pub replace: bool,
    /// Recorded as `updated_by` and the revisions' `actor`.
    pub actor: &'a str,
    /// The time of every row written (server clock).
    pub now: DateTime<Utc>,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// New paths (or paths whose document had been deleted).
    pub added: usize,
    /// Live documents whose content or execute bit changed.
    pub updated: usize,
    /// Live documents missing from the import, now deleted.
    pub deleted: usize,
    /// Live documents imported unchanged.
    pub unchanged: usize,
    /// Lint of the imported files (reported, never refused on).
    pub lint: LintReport,
}

/// A document row as import compares it.
#[derive(sqlx::FromRow)]
struct ExistingRow {
    path: String,
    content: Vec<u8>,
    executable: bool,
    revision: i32,
    deleted_at: Option<DateTime<Utc>>,
}

impl ExistingRow {
    fn is_live(&self) -> bool {
        self.deleted_at.is_none()
    }

    /// Live with exactly this content and execute bit?
    fn is_live_with(&self, file: &StoredFile) -> bool {
        self.is_live() && self.content == file.content && self.executable == file.executable
    }
}

/// Imports files into a catalog, in one transaction under the catalog's lock.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name (it must exist: `okf-mcp catalog create`).
/// * `files` - the files, from [`crate::archive`].
/// * `options` - replace, actor and time.
///
/// # Returns
/// The counts and the lint report.
///
/// # Errors
/// [`OkfError::ImportRefused`] if a markdown file is not UTF-8 text;
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Refused`] for a non-empty catalog without `replace`;
/// [`OkfError::Database`].
pub async fn import(
    pool: &PgPool,
    catalog: &str,
    files: Files,
    options: &ImportOptions<'_>,
) -> Result<ImportReport, OkfError> {
    refuse_markdown_that_is_not_text(&files)?;
    // Building a snapshot is CPU work (rayon across documents): run it on a
    // blocking thread so the async runtime's threads stay free. The closure
    // takes ownership of `files` (`move`) and hands it back.
    let (files, snapshot) = tokio::task::spawn_blocking(move || -> Result<_, OkfError> {
        let workspace = MemWorkspace::from_stored(files)?;
        let snapshot = Snapshot::build(&workspace)?;
        Ok((workspace.into_stored(), snapshot))
    })
    .await
    // A panic in the blocking task is re-raised here, as if it ran inline.
    .unwrap_or_else(|panic| std::panic::resume_unwind(panic.into_panic()))?;
    let mut report = ImportReport {
        lint: lint(&snapshot, options.now, None),
        ..ImportReport::default()
    };

    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    catalogs::lock(&mut tx, catalog.id).await?;
    let existing = existing_rows(&mut tx, catalog.id).await?;
    if !options.replace && existing.values().any(ExistingRow::is_live) {
        return Err(OkfError::Refused {
            reason: format!(
                "catalog {:?} is not empty; pass --replace to replace its contents",
                catalog.name
            ),
        });
    }
    for (path, file) in &files {
        let previous = existing.get(path);
        if previous.is_some_and(|row| row.is_live_with(file)) {
            report.unchanged += 1;
            continue;
        }
        let revision = previous.map_or(1, |row| row.revision + 1);
        let derived = Derived::of(snapshot.get(path));
        write_document(&mut tx, catalog.id, path, file, revision, &derived, options).await?;
        record_revision(&mut tx, catalog.id, path, revision, Some(file), options).await?;
        if previous.is_some_and(ExistingRow::is_live) {
            report.updated += 1;
        } else {
            report.added += 1;
        }
    }
    let missing = existing
        .values()
        .filter(|row| row.is_live() && !files.contains_key(&row.path));
    for row in missing {
        delete_document(&mut tx, catalog.id, &row.path, row.revision + 1, options).await?;
        record_revision(&mut tx, catalog.id, &row.path, row.revision + 1, None, options).await?;
        report.deleted += 1;
    }
    if report.added + report.updated + report.deleted > 0 {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(catalog.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(report)
}

/// A markdown file must be UTF-8 text: it is analyzed.
fn refuse_markdown_that_is_not_text(files: &Files) -> Result<(), OkfError> {
    let problems: Vec<String> = files
        .iter()
        .filter(|(path, file)| path.ends_with(".md") && std::str::from_utf8(&file.content).is_err())
        .map(|(path, _)| format!("{path}: a markdown file must be UTF-8 text"))
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(OkfError::ImportRefused { problems })
    }
}

/// Every row of the catalog, tombstones included, by path.
async fn existing_rows(
    conn: &mut PgConnection,
    catalog_id: i64,
) -> Result<HashMap<String, ExistingRow>, OkfError> {
    let rows = sqlx::query_as::<_, ExistingRow>(
        "SELECT path, content, executable, revision, deleted_at FROM documents WHERE catalog_id = $1",
    )
    .bind(catalog_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|row| (row.path.clone(), row)).collect())
}

/// Inserts or overwrites a document (reviving a tombstone).
async fn write_document(
    conn: &mut PgConnection,
    catalog_id: i64,
    path: &str,
    file: &StoredFile,
    revision: i32,
    derived: &Derived,
    options: &ImportOptions<'_>,
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
    .bind(catalog_id)
    .bind(path)
    .bind(&file.content)
    .bind(file.executable)
    .bind(revision)
    .bind(options.now)
    .bind(options.actor)
    .bind(derived.kind)
    .bind(&derived.concept_type)
    .bind(&derived.title)
    .bind(&derived.status)
    .bind(&derived.tags)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Turns a live document into a tombstone.
async fn delete_document(
    conn: &mut PgConnection,
    catalog_id: i64,
    path: &str,
    revision: i32,
    options: &ImportOptions<'_>,
) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE documents SET content = ''::bytea, executable = false, revision = $3, \
             deleted_at = $4, updated_at = $4, updated_by = $5, kind = NULL, \
             concept_type = NULL, title = NULL, status = NULL, tags = '{}' \
         WHERE catalog_id = $1 AND path = $2",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(revision)
    .bind(options.now)
    .bind(options.actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Appends a history row: an `import` with the content, or a `delete` (`file` is `None`).
async fn record_revision(
    conn: &mut PgConnection,
    catalog_id: i64,
    path: &str,
    revision: i32,
    file: Option<&StoredFile>,
    options: &ImportOptions<'_>,
) -> Result<(), OkfError> {
    let op = if file.is_some() { "import" } else { "delete" };
    sqlx::query(
        "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                message, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'import', $8)",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(revision)
    .bind(op)
    .bind(file.map(|file| &file.content))
    .bind(file.is_some_and(|file| file.executable))
    .bind(options.actor)
    .bind(options.now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
```

`src/store/mod.rs`: `pub mod documents;` and `pub mod import;`.

- [ ] **Step 5: The `import` subcommand**

In `src/cli.rs`, rename `_config` to `config`, and add:

```rust
use std::fs::File;
use crate::archive;
use crate::store::import::{self, ImportOptions};

    // In `enum Command`:
    /// Import a directory or a .tar.gz into an existing catalog.
    Import {
        /// The catalog.
        catalog: String,
        /// A directory, a .tar.gz file, or `-` for a .tar.gz on stdin.
        source: PathBuf,
        /// Replace a non-empty catalog's contents.
        #[arg(long)]
        replace: bool,
        /// Recorded as the revisions' actor.
        #[arg(long, default_value = "okf-mcp/import")]
        actor: String,
    },

        // In `run`'s match:
        Command::Import { catalog, source, replace, actor } => {
            let limit = config.max_import_bytes.as_u64();
            let files = if source.as_os_str() == "-" {
                archive::read_tarball(std::io::stdin().lock(), limit)?
            } else if source.is_dir() {
                archive::read_directory(&source, limit)?
            } else {
                let file = File::open(&source).with_context(|| format!("open {}", source.display()))?;
                archive::read_tarball(std::io::BufReader::new(file), limit)?
            };
            let pool = connect(cli.database_url.as_deref()).await?;
            let options = ImportOptions { replace, actor: &actor, now: clock.now() };
            let report = import::import(&pool, &catalog, files, &options).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
```

A CLI test (add to `src/cli.rs`'s tests) imports a temporary directory:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn import_reads_a_directory() {
        let db = crate::db::pg_support::TestDb::start().await;
        let clock = FixedClock(chrono::Utc::now());
        let create = CatalogCommand::Create { name: "pg".to_string(), description: String::new() };
        run(cli(Some(db.url.clone()), Command::Catalog(create)), &clock).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.md"), "# Index\n").unwrap();
        let import = Command::Import {
            catalog: "pg".to_string(),
            source: dir.path().to_path_buf(),
            replace: false,
            actor: "test".to_string(),
        };

        run(cli(Some(db.url.clone()), import), &clock).await.unwrap();

        let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM documents").fetch_all(&db.pool).await.unwrap();
        assert_eq!(paths, ["index.md"]);
    }
```

In the design's "Import" step 3, replace "write the differences as `import` revisions (unchanged files keep their revision, missing ones are deleted)" with "write the differences as `import` revisions (unchanged files keep their revision; missing ones are deleted, with `delete` revisions whose message is `import`)".

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib store cli`
Expected: PASS.

- [ ] **Step 7: Gates, changelog, commit**

Run the gates. Changelog: Added: `store::documents` (`DocumentRow`, `published`, `Derived`), `store::import` (`import`, `ImportOptions`, `ImportReport`), `okf-mcp import`. Commit:

```bash
git add src/store src/cli.rs docs/superpowers/specs changelog/
git commit -m "store: import a directory or tarball into a catalog, with okf-mcp import"
```

---

### Task 9: `store::export` and `okf-mcp export`

Export writes a catalog's live documents as a `.tar.gz` (design "Export"): no top-level directory, modes 0644/0755, each file's mtime its `updated_at`. M4's export route calls the same function.

**Files:**
- Create: `src/store/export.rs`; modify `src/store/mod.rs`
- Modify: `src/cli.rs` (`export` subcommand)
- Create: `changelog/<ts>-m3-task-09-export.md`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{self, Files};
    use crate::db::test_db;
    use crate::okf::workspace::StoredFile;
    use crate::store::import::{import, ImportOptions};
    use chrono::{TimeZone, Utc};

    #[tokio::test(flavor = "multi_thread")]
    async fn an_export_holds_the_live_documents_exactly() {
        let db = test_db().await;
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        catalogs::create(&db.pool, "pg", "", now).await.unwrap();
        let mut files = Files::new();
        files.insert("a.md".into(), StoredFile { content: b"# A\n".to_vec(), executable: false });
        files.insert("tools/run.sh".into(), StoredFile { content: b"#!/bin/sh\n\0".to_vec(), executable: true });
        files.insert("gone.md".into(), StoredFile { content: b"# Gone\n".to_vec(), executable: false });
        let options = ImportOptions { replace: false, actor: "t", now };
        import(&db.pool, "pg", files.clone(), &options).await.unwrap();
        files.remove("gone.md");
        let options = ImportOptions { replace: true, actor: "t", now };
        import(&db.pool, "pg", files.clone(), &options).await.unwrap();

        let tarball = export(&db.pool, "pg", Vec::new()).await.unwrap();

        // The tombstone is not exported; everything else reads back exactly.
        assert_eq!(archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap(), files);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_catalog_exports_an_empty_archive_and_unknown_ones_fail() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();

        let tarball = export(&db.pool, "pg", Vec::new()).await.unwrap();
        let unknown = export(&db.pool, "nope", Vec::new()).await;

        assert!(archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap().is_empty());
        assert!(matches!(unknown, Err(OkfError::CatalogNotSelected { .. })));
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib store::export`
Expected: compile error, `unresolved module export`.

- [ ] **Step 3: Implement**

`src/store/export.rs`:

```rust
//! Export (design "Export"): a catalog's live documents as a `.tar.gz`.

use std::io::Write;

use sqlx::PgPool;

use crate::archive::{self, ExportFile};
use crate::error::OkfError;
use crate::store::{catalogs, documents};

/// Writes a catalog's live documents to `out` as a `.tar.gz`.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name.
/// * `out` - where to write (a file, stdout, an HTTP body buffer).
///
/// # Returns
/// `out`, with the archive finished.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Database`]; [`OkfError::Io`] if writing fails.
pub async fn export<W: Write>(pool: &PgPool, catalog: &str, out: W) -> Result<W, OkfError> {
    let catalog = catalogs::find(pool, catalog).await?;
    let rows = documents::published(pool, catalog.id).await?;
    let files: Vec<ExportFile<'_>> = rows
        .iter()
        .map(|row| ExportFile {
            path: &row.path,
            content: &row.content,
            executable: row.executable,
            modified: row.updated_at,
        })
        .collect();
    archive::write_tarball(out, &files)
}
```

`src/store/mod.rs`: `pub mod export;`. `src/cli.rs`:

```rust
use crate::store::export;

    // In `enum Command`:
    /// Export a catalog as a .tar.gz.
    Export {
        /// The catalog.
        catalog: String,
        /// The file to write, or `-` for stdout.
        output: PathBuf,
    },

        // In `run`'s match:
        Command::Export { catalog, output } => {
            let pool = connect(cli.database_url.as_deref()).await?;
            if output.as_os_str() == "-" {
                // Build in memory, then lock stdout: a `StdoutLock` held across
                // `.await` would make this future impossible to `tokio::spawn`.
                let tarball = export::export(&pool, &catalog, Vec::new()).await?;
                std::io::Write::write_all(&mut std::io::stdout().lock(), &tarball)?;
            } else {
                let file = File::create(&output).with_context(|| format!("create {}", output.display()))?;
                // `BufWriter` batches small writes; `into_inner` flushes it and reports a failure.
                let written = export::export(&pool, &catalog, std::io::BufWriter::new(file)).await?;
                written.into_inner().map_err(|error| error.into_error())?;
            }
        }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib store::export`
Expected: PASS.

- [ ] **Step 5: Gates, changelog, commit**

Run the gates, then try it by hand: `okf-mcp catalog create t && okf-mcp import t tests/fixtures/catalog && okf-mcp export t /tmp/t.tgz && tar tzf /tmp/t.tgz | head` against any Postgres (`DATABASE_URL`). Changelog: Added: `store::export::export`, `okf-mcp export`. Commit:

```bash
git add src/store src/cli.rs changelog/
git commit -m "store: export a catalog as a tarball, with okf-mcp export"
```

---

### Task 10: `catalog::CatalogState`: load, rebuild and swap the snapshot (#32)

Each catalog the server holds is a `CatalogState`: an `RwLock<Arc<Loaded>>` built from the catalog's live rows through `MemWorkspace` and M2's `Snapshot::build`. `refresh` compares the catalog's `generation` with the loaded one and, when it moved, rebuilds off the async threads and swaps the new snapshot in. Readers holding the old `Arc` keep a consistent snapshot. M4's poll loop and M5's publish call `refresh`.

**Files:**
- Create: `src/catalog.rs`; modify `src/lib.rs` (`pub mod catalog;`)
- Create: `changelog/<ts>-m3-task-10-catalog-state.md`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::import::{import, ImportOptions};
    use chrono::Utc;

    fn files(entries: &[(&str, &[u8])]) -> Files {
        entries
            .iter()
            .map(|(path, content)| {
                let file = StoredFile { content: content.to_vec(), executable: false };
                (path.to_string(), file)
            })
            .collect()
    }

    async fn import_into(pool: &PgPool, entries: &[(&str, &[u8])], replace: bool) {
        let options = ImportOptions { replace, actor: "t", now: Utc::now() };
        import(pool, "pg", files(entries), &options).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_builds_the_snapshot_of_the_live_documents() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        // A plain link: `okf.py` does not count an image embed (`![..](..)`) as a link.
        import_into(&db.pool, &[("a.md", b"---\ntype: Note\n---\n[logo](logo.png)\n"), ("logo.png", b"\x89PNG\0")], false).await;

        let state = CatalogState::load(&db.pool, "pg").await.unwrap();

        let loaded = state.current();
        assert_eq!((state.name(), loaded.generation()), ("pg", 1));
        assert_eq!(loaded.snapshot().docs().len(), 1);
        // The image is in the workspace, so the link to it resolves.
        assert!(loaded.snapshot().get("a.md").unwrap().links()[0].target_exists);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_swaps_in_a_new_snapshot_only_when_the_catalog_changed() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        import_into(&db.pool, &[("a.md", b"# Old\n")], false).await;
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();
        let before = state.current();

        assert!(!state.refresh(&db.pool).await.unwrap(), "nothing changed yet");
        import_into(&db.pool, &[("a.md", b"# New\n")], true).await;
        assert!(state.refresh(&db.pool).await.unwrap());

        let after = state.current();
        assert_eq!(after.generation(), 2);
        assert_eq!(after.snapshot().get("a.md").unwrap().text(), "# New\n");
        // A reader that took the old snapshot still sees it, whole.
        assert_eq!(before.snapshot().get("a.md").unwrap().text(), "# Old\n");
        assert!(!state.refresh(&db.pool).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_catalog_cannot_be_refreshed() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();
        catalogs::delete(&db.pool, "pg", false).await.unwrap();

        assert!(matches!(state.refresh(&db.pool).await, Err(OkfError::CatalogNotSelected { .. })));
    }
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib catalog::`
Expected: compile error, `unresolved module catalog`.

- [ ] **Step 3: Implement**

`src/catalog.rs`:

```rust
//! Per-catalog state held in memory (design "In-memory snapshot"): the
//! snapshot of the catalog's live documents, rebuilt and swapped when the
//! catalog's `generation` moves.
//!
//! Readers call [`CatalogState::current`] and keep the `Arc` for as long as
//! they need: a swap never changes a snapshot someone is reading, and readers
//! never wait on the database.

use std::sync::{Arc, RwLock};

use sqlx::PgPool;

use crate::error::OkfError;
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::{catalogs, documents};

/// One loaded state of a catalog. Immutable.
#[derive(Debug)]
pub struct Loaded {
    generation: i64,
    snapshot: Snapshot,
}

impl Loaded {
    /// The catalog generation this was built from.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The snapshot of the live documents.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

/// A catalog held in memory.
#[derive(Debug)]
pub struct CatalogState {
    id: i64,
    name: String,
    // `RwLock<Arc<_>>`: the lock is held only to clone or replace the `Arc`
    // (a pointer copy), never while a snapshot is read or built.
    current: RwLock<Arc<Loaded>>,
}

impl CatalogState {
    /// Loads a catalog: its current generation and snapshot.
    ///
    /// # Errors
    /// [`OkfError::CatalogNotSelected`] if there is no such catalog;
    /// [`OkfError::Database`].
    pub async fn load(pool: &PgPool, name: &str) -> Result<Self, OkfError> {
        let catalog = catalogs::find(pool, name).await?;
        let loaded = build(pool, catalog.id).await?;
        Ok(Self {
            id: catalog.id,
            name: catalog.name,
            current: RwLock::new(Arc::new(loaded)),
        })
    }

    /// The catalog's database id.
    pub fn id(&self) -> i64 {
        self.id
    }

    /// The catalog's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current loaded state. Cheap: it clones an `Arc` (a reference count).
    pub fn current(&self) -> Arc<Loaded> {
        Arc::clone(&self.current.read().expect("catalog state lock poisoned"))
    }

    /// Rebuilds and swaps in the snapshot if the catalog changed since it was loaded.
    ///
    /// # Returns
    /// Whether a new snapshot was swapped in.
    ///
    /// # Errors
    /// [`OkfError::CatalogNotSelected`] if the catalog was deleted;
    /// [`OkfError::Database`].
    pub async fn refresh(&self, pool: &PgPool) -> Result<bool, OkfError> {
        let generation: Option<i64> = sqlx::query_scalar("SELECT generation FROM catalogs WHERE id = $1")
            .bind(self.id)
            .fetch_optional(pool)
            .await?;
        let Some(generation) = generation else {
            return Err(OkfError::CatalogNotSelected {
                catalog: self.name.clone(),
            });
        };
        if generation == self.current().generation {
            return Ok(false);
        }
        let loaded = build(pool, self.id).await?;
        let mut current = self.current.write().expect("catalog state lock poisoned");
        // Two refreshes may race; keep whichever saw the newer generation.
        if loaded.generation > current.generation {
            *current = Arc::new(loaded);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Reads a catalog's generation and live documents consistently, then builds
/// the snapshot on a blocking thread.
async fn build(pool: &PgPool, catalog_id: i64) -> Result<Loaded, OkfError> {
    // A repeatable-read transaction sees one moment of the database, so the
    // generation and the rows agree even if an import commits meanwhile.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let generation: i64 = sqlx::query_scalar("SELECT generation FROM catalogs WHERE id = $1")
        .bind(catalog_id)
        .fetch_one(&mut *tx)
        .await?;
    let rows = documents::published(&mut *tx, catalog_id).await?;
    tx.commit().await?;
    let snapshot = tokio::task::spawn_blocking(move || {
        let files = rows.into_iter().map(|row| {
            let file = StoredFile {
                content: row.content,
                executable: row.executable,
            };
            (row.path, file)
        });
        let workspace = MemWorkspace::from_stored(files)?;
        Snapshot::build(&workspace)
    })
    .await
    .unwrap_or_else(|panic| std::panic::resume_unwind(panic.into_panic()))?;
    Ok(Loaded {
        generation,
        snapshot,
    })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib catalog::`
Expected: PASS.

- [ ] **Step 5: Gates, changelog, commit**

Run the gates. Changelog: Added: `catalog::{CatalogState, Loaded}` with `load`, `current`, `refresh` (#32). Commit:

```bash
git add src/catalog.rs src/lib.rs changelog/
git commit -m "catalog: per-catalog snapshot state, rebuilt and swapped on change (#32)"
```

PR body: `Closes #32`.

---

### Task 11: Lint's commit times from revisions, and the git-era wording (#31)

`okf.py` fills `CommitTimes` from `git log`; the server has no git, and a document's last revision time is the same fact. `Loaded` carries `CommitTimes` built from each live document's `updated_at`, and `Loaded::lint` passes them, so `generated-after-commit` works again. Paths are catalog-relative by construction, which removes #31's original concern. (M5 adds the session's changeset paths as the dirty set.) The doc comments that still describe the git plan are updated in the same PR.

Before opening the PR, edit issue #31's body (`gh issue edit 31 --body-file ...`) to say it is now "commit times from revision times", linking the design's "Carried into M3 and later" entry.

**Files:**
- Modify: `src/store/documents.rs` (`commit_times`)
- Modify: `src/catalog.rs` (`Loaded::commit_times`, `Loaded::lint`)
- Modify: `src/okf/analysis/lint.rs` (module doc lines 4-5; `CommitTimes` and `CommitTimes::new` docs; `lint`'s `commits` argument doc)
- Modify: `src/okf/snapshot.rs` (module doc: "rebuilds it after each fetch or publish")
- Modify: `src/okf/workspace.rs` (`DiskWorkspace` doc: "serializes writes and git operations on a worktree ... planned for M3")
- Modify: `src/okf/mod.rs` (line 3), `src/error.rs` (enum doc)
- Create: `changelog/<ts>-m3-task-11-commit-times.md`

- [ ] **Step 1: Write the failing tests**

`src/store/documents.rs`:

```rust
    #[test]
    fn commit_times_are_the_documents_update_times() {
        use chrono::{TimeZone, Utc};
        let at = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        let rows = [DocumentRow {
            path: "a.md".to_string(),
            content: Vec::new(),
            executable: false,
            revision: 1,
            updated_at: at,
        }];

        let expected = crate::okf::analysis::lint::CommitTimes::new(
            [("a.md".to_string(), at.fixed_offset())].into_iter().collect(),
            std::collections::HashSet::new(),
        );
        assert_eq!(commit_times(&rows), expected);
    }
```

`src/catalog.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn lint_flags_a_stamp_later_than_the_documents_last_revision() {
        use chrono::{TimeDelta, TimeZone};
        let db = test_db().await;
        let imported = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        catalogs::create(&db.pool, "pg", "", imported).await.unwrap();
        let later = "---\ntype: Note\ngenerated: { by: t, at: 2026-11-02T12:10:00Z }\n---\nBody\n";
        let earlier = "---\ntype: Note\ngenerated: { by: t, at: 2026-11-02T11:00:00Z }\n---\nBody\n";
        let options = ImportOptions { replace: false, actor: "t", now: imported };
        let entries = files(&[("later.md", later.as_bytes()), ("earlier.md", earlier.as_bytes())]);
        import(&db.pool, "pg", entries, &options).await.unwrap();
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();

        let report = state.current().lint(imported + TimeDelta::minutes(30));

        let flagged: Vec<&str> = report
            .warnings
            .iter()
            .filter(|finding| finding.rule == "generated-after-commit")
            .map(|finding| finding.file.as_str())
            .collect();
        assert_eq!(flagged, ["later.md"]);
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib commit_times lint_flags`
Expected: compile errors (`cannot find function commit_times`, `no method named lint`).

- [ ] **Step 3: Implement**

`src/store/documents.rs`:

```rust
use std::collections::HashSet;

use crate::okf::analysis::lint::CommitTimes;

/// Lint's commit times from stored documents: each live document's last
/// revision time stands in for its last git commit (design "Read tools").
///
/// # Returns
/// Times keyed by catalog-relative path; no dirty paths (M5 adds a
/// session's changeset paths).
pub fn commit_times(rows: &[DocumentRow]) -> CommitTimes {
    let times = rows
        .iter()
        .map(|row| (row.path.clone(), row.updated_at.fixed_offset()))
        .collect();
    CommitTimes::new(times, HashSet::new())
}
```

`src/catalog.rs`: add a field `commit_times: CommitTimes` to `Loaded`, an accessor, and:

```rust
use chrono::{DateTime, Utc};
use crate::okf::analysis::lint::{lint, CommitTimes, LintReport};

    /// Lint's commit times: each live document's last revision time.
    pub fn commit_times(&self) -> &CommitTimes {
        &self.commit_times
    }

    /// Lints the snapshot with its commit times.
    ///
    /// # Arguments
    /// * `now` - the current time (server clock).
    pub fn lint(&self, now: DateTime<Utc>) -> LintReport {
        lint(&self.snapshot, now, Some(&self.commit_times))
    }
```

In `build`, compute `let commit_times = documents::commit_times(&rows);` before `rows` moves into the blocking task, and store it in `Loaded`.

Doc comments (no behaviour change):

- `src/okf/analysis/lint.rs` module doc: "The one check that needs commit times (`generated-after-commit`) takes them as data ([`CommitTimes`]), so this module stays pure: `okf.py` reads them from git, the server from document revision times (`crate::store::documents::commit_times`)."
- `CommitTimes` doc: "Each file's last commit (or revision) time, and the files with uncommitted changes: what `okf.py`'s `last_commit_times()` returns when git works." In `CommitTimes::new`, keep the argument docs, and replace the "Key spelling (for the caller filling this from git)" section with: "Both sets are looked up by [`SnapshotDoc::path`] exactly: catalog-relative, `/`-separated, no leading `/` or `./`. The server's stored paths already have that form. A caller filling this from git must strip the catalog's prefix when the catalog is not the repository root, or the check silently stops firing for those files."
- `lint`'s `commits` argument: "commit or revision times, or `None` when they are unknown (the `generated-after-commit` check is then skipped, as in `okf.py` without git)."
- `src/okf/snapshot.rs` module doc: "M3 keeps one per catalog in [`crate::catalog::CatalogState`] and rebuilds it after each publish or import".
- `src/okf/workspace.rs`, `DiskWorkspace`'s accepted-gap paragraph: replace "serializes writes and git operations on a worktree. That serialization is planned for M3 ..." with "does not write the same files concurrently. The server never writes through a `DiskWorkspace` (it stores catalogs in Postgres); import only reads one."
- `src/okf/mod.rs` line 3: "so it needs neither a database nor a real filesystem in tests."
- `src/error.rs` enum doc: "Later milestones add variants (auth, publish conflicts, lint); ..."

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Gates, changelog, commit**

Run the gates. `grep -rn "git" src/okf/analysis/lint.rs src/okf/workspace.rs src/okf/mod.rs src/error.rs src/okf/snapshot.rs` should show only mentions of what `okf.py` does. Changelog: Added: `store::documents::commit_times`, `Loaded::{commit_times, lint}` (#31); Changed: doc comments no longer describe a git-backed server. Commit:

```bash
git add src changelog/
git commit -m "lint: commit times from document revisions; drop git-era wording (#31)"
```

PR body: `Closes #31`.

---

### Task 12: Round trips: the synthetic corpus and the real catalog

The milestone's acceptance check (design "Milestones", M3): export then import reproduces a catalog's file contents and modes byte for byte. The synthetic corpus runs in every `cargo test`; the real catalog (a `git archive` of `OKF_PARITY_CATALOG`'s `HEAD`) runs with `-- --ignored`, as live parity does.

**Files:**
- Create: `tests/roundtrip.rs`
- Create: `changelog/<ts>-m3-task-12-roundtrip.md`

- [ ] **Step 1: Write the tests**

`tests/roundtrip.rs`:

```rust
//! Import then export reproduces a catalog's files exactly (content and
//! execute bit), and importing the export again changes nothing.
//!
//! `the_real_catalog_round_trips` is `#[ignore]`d: it needs a git checkout of
//! a catalog in `OKF_PARITY_CATALOG` (read in place, never copied here):
//!
//! `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`

mod support;

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use okf_mcp::archive::{self, Files};
use okf_mcp::db;
use okf_mcp::store::import::{import, ImportOptions, ImportReport};
use okf_mcp::store::{catalogs, export};
use support::pg::TestDb;

/// Imports `files` into a new catalog, exports it, reads the export back,
/// and imports the export again with `--replace`.
///
/// # Returns
/// The files read from the export, and the second import's report.
async fn round_trip(files: &Files) -> (Files, ImportReport) {
    let db = TestDb::start().await;
    db::migrate(&db.pool).await.unwrap();
    catalogs::create(&db.pool, "rt", "", Utc::now()).await.unwrap();
    let first = ImportOptions { replace: false, actor: "test/roundtrip", now: Utc::now() };
    import(&db.pool, "rt", files.clone(), &first).await.unwrap();

    let tarball = export::export(&db.pool, "rt", Vec::new()).await.unwrap();
    let exported = archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap();
    let again = ImportOptions { replace: true, actor: "test/roundtrip", now: Utc::now() };
    let report = import(&db.pool, "rt", exported.clone(), &again).await.unwrap();
    (exported, report)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_synthetic_corpus_round_trips() {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/catalog"));
    let files = archive::read_directory(root, u64::MAX).unwrap();
    assert!(files.len() > 40, "the fixture corpus is there");

    let (exported, again) = round_trip(&files).await;

    assert_eq!(exported, files);
    assert_eq!((again.added, again.updated, again.deleted), (0, 0, 0));
    assert_eq!(again.unchanged, files.len());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG"]
async fn the_real_catalog_round_trips() {
    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let archived = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["archive", "--format=tar.gz", "HEAD"])
        .output()
        .expect("run git archive");
    assert!(archived.status.success(), "git archive failed");
    let tracked = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
        .expect("run git ls-files");
    let tracked_count = tracked.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).count();
    let files = archive::read_tarball(archived.stdout.as_slice(), 256 * 1024 * 1024).unwrap();
    assert_eq!(files.len(), tracked_count, "every tracked file is imported");
    assert!(files.values().any(|file| file.executable), "the catalog has scripts");

    let (exported, again) = round_trip(&files).await;

    assert_eq!(exported, files);
    assert_eq!(again.unchanged, files.len());
}
```

- [ ] **Step 2: Run them**

Run: `cargo test --test roundtrip`, then `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`
Expected: PASS (the second needs the catalog checkout). A failure here is a real bug in Tasks 7-9: fix it there, with a plan correction, not in the test.

- [ ] **Step 3: Gates, changelog, commit**

Run the gates (the roundtrip `--ignored` run is now one of them). Changelog: Added: `tests/roundtrip.rs` (the synthetic corpus always; the real catalog with `-- --ignored`). Commit:

```bash
git add tests/roundtrip.rs changelog/
git commit -m "test: import/export round trips of the synthetic corpus and the real catalog"
```

---

### Task 13: Smoke test, README, design update and milestone changelog

**Files:**
- Modify: `tests/smoke.rs` (a storage smoke test)
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`
- Create: `changelog/<ts>-m3-storage.md` (the milestone note) and `changelog/<ts>-m3-task-13-wrap-up.md`

- [ ] **Step 1: The storage smoke test**

Add to `tests/smoke.rs` (keep the M2 test as it is; add `mod support;` at the top):

```rust
/// Import, load, analyze and export a real catalog through Postgres, timed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn storage_round_trip_on_a_real_catalog() {
    use okf_mcp::archive;
    use okf_mcp::catalog::CatalogState;
    use okf_mcp::store::import::{import, ImportOptions};
    use okf_mcp::store::{catalogs, export};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let db = support::pg::TestDb::start().await;
    okf_mcp::db::migrate(&db.pool).await.unwrap();
    catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();

    let started = Instant::now();
    let files = archive::read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    eprintln!("read directory: {} files in {:?}", files.len(), started.elapsed());

    let started = Instant::now();
    let options = ImportOptions { replace: false, actor: "smoke", now: Utc::now() };
    let report = import(&db.pool, "pg", files, &options).await.unwrap();
    eprintln!(
        "import: {} added, lint {} errors / {} warnings in {:?}",
        report.added,
        report.lint.errors.len(),
        report.lint.warnings.len(),
        started.elapsed()
    );

    let started = Instant::now();
    let state = CatalogState::load(&db.pool, "pg").await.unwrap();
    eprintln!("load snapshot: {:?}", started.elapsed());
    let loaded = state.current();

    let first = timed("links (first call, builds the matcher): unlinked mentions", || {
        links_report(loaded.snapshot(), true).summary.unlinked_mentions
    });
    let second = timed("links (cached matcher): unlinked mentions", || {
        links_report(loaded.snapshot(), true).summary.unlinked_mentions
    });
    assert!(second < first, "the cached matcher makes the second report cheaper");
    timed("lint with revision times: findings", || {
        let report = loaded.lint(Utc::now());
        report.errors.len() + report.warnings.len()
    });

    let started = Instant::now();
    let tarball = export::export(&db.pool, "pg", Vec::new()).await.unwrap();
    eprintln!("export: {} bytes in {:?}", tarball.len(), started.elapsed());
}
```

(Directory import includes untracked files in the checkout; the round-trip test in Task 12 uses `git archive` for exactness. Here, only timing matters.)

Run: `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`, and record every line it prints, with the M2 test's lines, in the milestone changelog.

- [ ] **Step 2: README**

Rewrite "Status": milestones 1-3 done; catalogs live in Postgres; the `okf-mcp` CLI administers them; the server arrives in M4. Add a "Command line" section:

```
okf-mcp migrate
okf-mcp catalog create pg --description "pgrace's catalog"
git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD | okf-mcp import pg -
okf-mcp key create laptop --access write --catalogs pg --default pg --actor claude-code/claude-opus-5-5
okf-mcp export pg pg.tar.gz
```

with a note that every subcommand reads `DATABASE_URL` (or `--database-url`) and `--config`/`OKF_MCP_CONFIG`. Update "Layout" (`db`, `store`, `archive`, `keys`, `catalog`, `cli`, `config`, `migrations/`, `tests/roundtrip.rs`, `tests/support/`) and "Testing" (Docker is required for `cargo test`; the roundtrip `--ignored` run).

- [ ] **Step 3: Design update**

In `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`:
- Status line: "M1 (core library), M2 (analysis) and M3 (storage) are delivered".
- Milestones table: M3 row "(delivered)" with a link to this plan and the PR range.
- After "What M2 shipped differently", add "What M3 shipped differently": the in-memory workspace holds bytes and execute bits; text means valid UTF-8; the configuration file holds only `max_import_bytes` until later milestones add theirs; subcommands run migrations on connect; `api_keys.default_catalog_id`; import writes rows one at a time in one transaction (with the measured time); lint's commit time is the last revision time, so after a first import it is the import time; every `Corrected (issue #N)` note from this plan that changes the design.
- "Carried into M3 and later": mark #31, #32, #33 and #35 done (with their PR numbers), leaving #34 for M4.

- [ ] **Step 4: Milestone changelog**

`changelog/<ts>-m3-storage.md`: Added (every module and subcommand, by task); Changed; the gate results with test counts; the smoke and roundtrip output; every plan correction (`grep -n "Corrected (issue" docs/superpowers/plans/2026-11-02-okf-mcp-m3-storage.md`); Deferred to later milestones (this plan's table, plus anything new).

- [ ] **Step 5: Gates and commit**

Run every gate, including both `--ignored` runs. Commit:

```bash
git add tests/smoke.rs README.md docs/superpowers/specs changelog/
git commit -m "docs: M3 smoke test, README, design update and changelog (M3.13)"
```

---

## Self-review (2026-11-02)

- **Design coverage (M3 row):** `config` (T4), `db` and migrations (T3), `store` (T5, T6, T8, T9), `archive` (T7), `catalog` with snapshot swap (T10) and the cached mention matcher (T2), import and export subcommands (T8, T9), `catalog` and `key` subcommands (T5, T6), revision-based commit times (T11), the byte-for-byte round trip (T12). The design's "Storage" tables and constraints are all in T3's migration; "Noticing changes made outside the server" is served by `admin_generation` (T5) and `CatalogState::refresh` (T10), with the poll loop in M4; the import rules are T7; the publish lock's advisory half is `catalogs::lock` (T5), used by import and delete.
- **Carried issues:** #31 (T11), #32 (T10), #33 (T2), #35 (T1), each its own task; #34 stays for M4.
- **Placeholders:** none; every code step has its code.
- **Names across tasks:** `StoredFile`, `MemWorkspace::{from_stored, stored, into_stored}` (T1) are used by T7, T8, T10; `archive::Files` (T7) by T8, T9, T12; `catalogs::{find, lock, bump_admin_generation, is_unique_violation}` (T5) by T6, T8, T9, T10; `documents::{DocumentRow, published, Derived}` (T8) by T9, T10, T11; `ImportOptions { replace, actor, now }` and `ImportReport` (T8) by T9's, T10's, T12's and T13's tests; `db::test_db` and `db::pg_support` (T3, made `pub(crate)` there) by every database unit test; `support::pg::TestDb` by T12 and T13.
