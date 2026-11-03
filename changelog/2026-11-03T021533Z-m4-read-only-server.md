# M4: read-only server (2026-11-03T02:15:33Z)

Milestone 4 of the OKF MCP server (design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`; plan:
`docs/superpowers/plans/2026-11-02-okf-mcp-m4-read-only-server.md`; epic #64). The
catalogs stored in M3 are now served over MCP streamable HTTP: `okf-mcp serve`, bearer-key
auth, catalog selection, every read tool that needs no changeset (`list_catalogs`,
`search`, `get_concept`, `read_document`, `history`, `open_questions`, `oldest`, `sprawl`,
`links_report`, `lint`), the export and import routes and `/healthz`, with a poll loop
that picks up imports, new or deleted catalogs and key changes made by the subcommands
without a restart. Deployable and useful on its own. Writes are M5.

## Pull requests

| Task | Issue | PR | What |
|---|---|---|---|
| 1 | #65 | #76 | `config`: `listen`, `allowed_hosts`, `default_catalog`, `poll_interval`, `session_idle_timeout`, and their validation |
| 2 | #66 | #77 | `auth`: `Principal`, the key cache, the bearer middleware (401) |
| 3 | #67 | #78 | `selection`: argument, connection, key default, server default, within the grants |
| 4 | #68 | #79 | `app`: the shared state and the poll loop |
| 5 | #69 | #80 | `mcp`: the server handler, the tool context, the error mapping, `list_catalogs` |
| 6 | #70 | #83 | `http` and `okf-mcp serve`: the router, `/healthz`, `/mcp`, tracing, timeouts, graceful shutdown; the end-to-end test harness |
| 7 | #34 | #81 | `search` across catalogs, and the `tags` wording |
| 8 | #71 | #82 | `get_concept` and `read_document` |
| 9 | #72 | #84 | `history` |
| 10 | #73 | #85 | The reports: `open_questions`, `oldest`, `sprawl`, `links_report`, `lint` |
| 11 | #74 | #86 | The archive routes: export and import over HTTP |
| 12 | #75 | this PR | Smoke test, README, design update, this changelog |

Each task has its own entry under `changelog/` (`*-m4-task-NN-*.md`) with the detail.

## Added

- Task 1: `Config::{listen, allowed_hosts, default_catalog, poll_interval,
  session_idle_timeout}` with public defaults and `Config::validate` (a zero interval, an
  empty `allowed_hosts` or a blank entry is refused). Dependency `humantime-serde`.
- Task 2: `auth::{Principal, Reach, KeyCache, Keys, bearer, require_key}`: keys looked up
  by SHA-256, 401 with `WWW-Authenticate: Bearer`; `OkfError::Forbidden`. Dependencies
  `axum` 0.8.9; dev `tower`.
- Task 3: `selection::{connection_catalogs, Selection, Sources, resolve,
  CATALOGS_HEADER}`. Dependency `form_urlencoded`.
- Task 4: `app::{App, Catalogs}` with `poll` and `poll_until`; `CatalogState::description`,
  `Loaded::{files, last_change}`. Dependency `tokio-util`.
- Task 5: `mcp::{OkfServer, ToolResult, reply, tool_error, blocking}`,
  `mcp::context::ToolContext`, `list_catalogs`; `OkfError::{kind, is_internal, to_json}`.
  Dependencies `rmcp` 3.8.0, `schemars` 1.2.3.
- Task 6: `http::{router, serve, AppError, REQUEST_TIMEOUT, timeout_layer}`, `/healthz`
  (`HEALTH_TIMEOUT`), `/mcp`; `okf-mcp serve [--listen]` (`OKF_MCP_LISTEN`), the default
  subcommand; `tests/support/server.rs` (`TestServer`, `call`, `structured`, `files`,
  `Client`) and `tests/server.rs`. Dependencies `tower-http` 0.6.11, tokio `net` and
  `signal`; dev `rmcp` client, `reqwest` 0.13.5, tokio `test-util`.
- Task 7: `okf::search::{search_catalogs, CatalogHit}` and the `search` tool.
- Task 8: `Doc::body`, `get_concept`, `read_document`.
- Task 9: `store::history::{revisions, RevisionRow}` and the `history` tool.
- Task 10: `JsonSchema` on the report types; `open_questions`, `oldest`, `sprawl`,
  `links_report`, `lint`.
- Task 11: `GET /catalogs/{name}/export` and `POST /catalogs/{name}/import[?replace=true]`.
- Task 12: `tests/smoke.rs`'s `server_on_a_real_catalog`; the README's "Running the
  server".

## Changed

- `src/main.rs` passes `Arc::new(SystemClock)` to `cli::run` and sets
  `DEFAULT_LOG_FILTER = "warn,okf_mcp=info,tower_http=info"` (Task 6).
- The design: "Configuration" (Task 1: the fields; Task 2: lookup by hash), "Tools"
  (Task 5: the text block is the JSON, the read set), "Process and routes" (Task 6:
  `/healthz`, `serve`, the timeouts), the `search` and `history` rows (Tasks 7 and 9:
  `limit` at most 200); in this task the status line, M4 marked delivered (PRs
  #76-#86), the `/mcp`, export and import rows and the timeout paragraph, "What M4
  shipped differently", "Open questions for M5", #34 and the cross-catalog search merge
  marked done in "Carried into M3 and later", and the CI note.

## Decisions

1. **Path tools search the whole read set; reports use one catalog.** Without a
   `catalog` argument, `get_concept`, `read_document` and `history` answer from the
   first selected catalog that has the path, naming it; the reports run on the
   `catalog` argument, else the first selected catalog. `search` spans the read set;
   `list_catalogs` lists every catalog the key reaches.
2. **Serve every MCP protocol version; fix changeset identity in M5.** rmcp 3.8 serves a
   client on `2026-07-28` statelessly (no `Mcp-Session-Id`); M4's tools need no session,
   so every version is accepted. M5 redesigns changeset identity (see "Follow-ups").

Also decided during M4: **CI is deferred** (pgrace, 2026-11-03). The gates run locally
before each merge; the design's "Milestones" says so.

## Deviations from the design

From the plan's "Simplifications and deviations relative to the design", all written
into the design's "What M4 shipped differently":

- Keys are looked up by SHA-256 in a `HashMap`, not compared in constant time.
- A tool result's text block is the JSON itself, not a summary.
- Report tools wrap `okf.py`'s lists in an object (`{"rows": [...]}`,
  `{"reports": [...]}`), so every output schema has an object root.
- `open_questions` reuses `oldest` with `open_questions_only`.
- The export is built in memory, not streamed.
- The import body cap is `max_import_bytes` plus 0.1% and 64 KiB of gzip slack; the exact
  limit is enforced on the decompressed stream.
- The catalog list is polled every tick, beside `admin_generation`; passes are
  serialized by one async mutex.
- A catalog that does not exist or that the key does not reach is 404 (or
  `CatalogNotSelected`), never 403.
- No timeout on `/mcp`.

## Plan corrections

Every dated note in the plan (`grep -n "Corrected" docs/superpowers/plans/2026-11-02-okf-mcp-m4-read-only-server.md`):

- issue #69 (verified fact 5): rmcp's macro finds the output schema in the written return
  type, so a `ToolResult<T>` alias leaves a tool without one; every `#[tool]` writes
  `Result<Json<T>, CallToolResult>`.
- issue #65 (two notes, Tasks 1 and 11): `max_import_bytes`' doc names the route's cap
  with slack (added by Task 11); `allowed_hosts` defaults through `Vec::from`; a blank
  entry is refused (review of PR #76).
- issue #66: the code shipped as planned; the tests also cover a stored hash that is not
  32 bytes, a revoked key, a `Basic` header, `WWW-Authenticate` on every 401 and a
  failed `reload`.
- issue #67: an empty connection list is unset; a header naming nothing falls back to
  the query; repeated headers and parameters are one list; non-ASCII headers decode as
  UTF-8 so a garbled name fails by name; `Refused` names `?catalogs=` (review of PR #78).
- issue #68: the key half of `poll` logs and carries on (`reload_keys_if_changed`); a
  generation that fails to build is remembered in `polling`'s `Broken` map and not
  rebuilt until it moves, while database errors are retried (review of PR #79); ten
  `app` tests, not five; `poll_until`'s test waits on a 30-second deadline.
- issue #69 (Task 5): `ToolResult`, `tool_error` and `reply` are `pub`; `read_set` and
  `one` use `std::slice::from_ref`; extra tests; `clippy::result_large_err` allowed
  where a function returns `CallToolResult` as its error.
- issue #70: `AppError::status()`; 503, not 408, through `timeout_layer()`;
  `is_internal` covers `Io` and `Config`, so every 500 is logged and redacted; a Ctrl-C
  registration failure no longer shuts down; requests logged at INFO by default; `serve`
  cancels and awaits the poll loop on every return; `/healthz` has its own 2-second
  deadline (review of PR #83, twice). An SSE `GET /mcp` stream held across SIGTERM
  ended within 0.05 s.
- issue #34: schemars keeps doc-comment line breaks, so `tags`' comment keeps "whole
  tags only" on one line; the merge test shows catalog order beating path; an unstable
  sort on a total order; `limit` capped at 200 (review of PR #81).
- issue #71: shipped as planned; `Doc::body` gained a doc test; more assertions; a
  test that both tools have object output schemas.
- issue #72: `limit` clamped to 1..=200; the shared test helper `files` moved to
  `archive` (review of PR #84); the tool description names `log.md` and indexes.
- issue #73: shipped as planned; doc comments gained Arguments and Returns; a fifth test
  calls the `#[tool]` methods with request `Parts` (review of PR #85).
- issue #74: shipped as planned, then review of PR #86 checked the key before reading
  the body, took the import out of `timeout_layer()`, bounded only its upload (503, the
  `Timeout` error JSON, nothing imported) and gave the import and refresh no deadline;
  `Content-Disposition` on the export.
- issue #75 (this task): the smoke test asserts that no tool errored and that the
  export is 200 and gzip, prints JSON sizes rather than an error flag, reads the
  checkout uncapped, and closes the client; the design's route rows and timeout
  paragraph changed beyond Step 3's list.

## Results

Final gates on this branch (`target/i75-logs/`), with Docker and `pg_knowledge_catalog` at
`4f51ac6`:

- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 342 unit, 6 golden parity (6 live ignored), 1 roundtrip (1 ignored),
  8 server, 0 smoke (3 ignored), 13 doc passed; 0 failed. M3 ended with 279 unit, 6
  golden parity, 1 roundtrip and 12 doc tests: M4 adds 63 unit tests, 8 end-to-end
  server tests and 1 doc test (`Doc::body`).
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: clean.
- Live parity: 6 passed of 6. Real-catalog round trip: 1 passed.
- Smoke (`OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test
  smoke -- --ignored --nocapture`): 3 passed in 4.60 s, started 2026-11-03T02:12:33Z.
  Load average at the start 28.83, 22.00, 20.82 (1, 5, 15 min; 32 CPUs, shared with
  other work), so the timings are upper bounds. The three tests run in parallel, so
  their lines interleave; here they are grouped by test, verbatim:

  ```
  every_analysis_runs_on_a_real_catalog
  snapshot: 183 documents, 0 secret findings in 29.001586ms
  links (with mentions): unlinked mentions: 193 in 280.432876ms
  lint (no commit times): findings: 11 in 7.894424ms
  snapshot plus lint: 36.89601ms
  oldest: rows: 5 in 201.751µs
  sprawl: rows: 5 in 11.998743ms
  search "time server": 20 hits in 277.311µs

  storage_round_trip_on_a_real_catalog
  read directory: 225 files in 1.819738ms
  import: 225 added, lint 0 errors / 11 warnings in 221.814583ms
  load snapshot: 24.641338ms
  links (first call, builds the matcher): unlinked mentions: 193 in 315.803886ms
  links (cached matcher): unlinked mentions: 193 in 41.687993ms
  lint with revision times: findings: 11 in 3.299984ms
  export: 450588 bytes in 46.845476ms

  server_on_a_real_catalog
  start Postgres, import 225 files and start the server: 3.174655231s
  search: 20 hits in 1.157866ms
  list_catalogs: 156 bytes of JSON in 488.372µs
  get_concept: 13801 bytes of JSON in 583.293µs
  read_document: 1961 bytes of JSON in 281.101µs
  history: 140 bytes of JSON in 717.173µs
  open_questions: 256 bytes of JSON in 401.662µs
  oldest: 1336 bytes of JSON in 621.723µs
  sprawl: 5392 bytes of JSON in 9.13649ms
  links_report: 62810 bytes of JSON in 304.904387ms
  links_report: 62810 bytes of JSON in 36.692801ms
  lint: 2349 bytes of JSON in 2.645222ms
  export route: 450588 bytes in 39.173572ms
  ```

  Over HTTP every tool but `links_report` answers in under 10 ms, the HTTP round trip,
  auth, selection and JSON encoding included. `links_report` takes 305 ms on the first
  call, which builds the mention matcher, and 37 ms on the second, which reuses it
  (about 8 times cheaper), so no per-tool timeout is needed on `/mcp` for M4's tools.
  The export route returns the real catalog's 450 KB tarball in 39 ms. The first line
  includes starting the Postgres container.

## Follow-ups

For M5 (recorded in the design's "Open questions for M5"):

- **Changeset identity without sessions**: MCP `2026-07-28` removes sessions, so "one
  changeset per (MCP session, catalog)" needs another key for such clients (Decision 2).
- **Bind it to the creating key**: an M4 session's streams are not bound to the key that
  opened it, so another valid key that learned an `Mcp-Session-Id` could attach to its
  `GET /mcp` stream or resume it with `Last-Event-ID`. M5 records the creating key and
  refuses any other before changesets depend on sessions.
- From M3, still open: the deadlock rule for writes beside `catalogs::delete`; whether
  `install/` and the root `docs/` and `tests/` are read-only machinery for the write
  tools; a backfill of the derived columns whenever `Derived::of` changes.
- From the plan's "Deliberately deferred" table, unchanged: `pending_changes`,
  `stranded_sessions`, `lint pending`, the write tools, changesets, overlays and publish
  (M5); discarding empty changesets on session close and the 24-hour prune (M5); the NTP
  offset check, `time_server` and the clock offset in `/healthz` (M7); `move_concept`,
  `split_concept`, `delete_concept` (M6); TLS to Postgres (M7).

Known, not filed:

- **testcontainers leaks a container on `StartupTimeout`.** Under heavy load a test can
  fail with `WaitContainer(StartupTimeout)`; the container it started keeps running,
  apparently because the failed start never returns the handle whose drop would remove
  it.
  Leftovers show in `docker ps --filter ancestor=postgres:17-alpine --filter
  label=org.testcontainers.managed-by=testcontainers` and need removing by hand.
- The import route's 503 path (an upload not read within `REQUEST_TIMEOUT`) has no test,
  and no test forces a refresh failure after a committed import (Task 11's notes).
