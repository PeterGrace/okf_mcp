# OKF MCP Server, Milestone 4 (Read-only server): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Serve the catalogs stored in M3 over MCP streamable HTTP: bearer-key auth, catalog selection, every read tool that needs no changeset (`list_catalogs`, `search`, `get_concept`, `read_document`, `history`, `open_questions`, `oldest`, `sprawl`, `links_report`, `lint`), the export and import routes and `/healthz`, with a poll loop that picks up CLI changes (imports, new or deleted catalogs, revoked keys) without a restart. Deployable and useful on its own.

**Architecture:** `okf-mcp serve` builds one shared `app::App` (the pool, the configuration, the clock, the key cache and one `catalog::CatalogState` per catalog) and an axum router (`http`). The router's auth layer (`auth`) turns a bearer key into an `Arc<Principal>` in the request extensions; rmcp's `StreamableHttpService` hands the request's `http::request::Parts` to every tool, so each tool (`mcp`) reads the principal and the connection's `X-OKF-Catalogs`, resolves its catalogs (`selection`), takes `Arc`s of their snapshots, and runs M2's analyses on a blocking thread. A background task polls `server_state.admin_generation` and the catalog list every `poll_interval`.

**Tech Stack:** Rust 2021 (toolchain 1.99.1), `axum` 0.8.9, `rmcp` 3.8.0 (`server`, `macros`, `transport-streamable-http-server`; dev: `client`, `transport-streamable-http-client-reqwest`), `schemars` 1.2.3, `tower-http` 0.6.11 (`trace`, `timeout`), `tokio-util` 0.7.20, `humantime-serde` 1.1.1, `form_urlencoded` 1.2.2; dev: `tower` 0.5.3 (`util`), `reqwest` 0.13.5; M3's `sqlx` 0.9.0, `tokio`, `testcontainers-modules` 0.15.0.

---

## Roadmap position

Milestone 4 of 7 in `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Milestones"). M1 (core library), M2 (analysis) and M3 (storage) are on `main` (`502c448`). M5 (writes) adds changesets, overlays and the write tools on top of this server; M6 restructuring; M7 deployment.

## Scope

In scope (design "Milestones", M4 row, and the sections it rests on: "Catalog selection", "Process and routes", "Configuration", "Tools" ("Read tools"), "Data flow" ("Read", "Noticing changes made outside the server"), "Errors", "Testing" (end-to-end)):

- `config`: `listen`, `allowed_hosts`, `default_catalog`, `poll_interval`, `session_idle_timeout`.
- `auth`: the key cache, `Principal`, the bearer middleware (HTTP 401 for a missing or unknown key; 403 for a route the key's access level does not allow).
- `selection`: argument, then connection (`X-OKF-Catalogs` header or `?catalogs=`), then the key's default, then the server's default; every step limited to the key's grants.
- `app`: every catalog's state and the key cache, and the poll loop (design "Noticing changes made outside the server").
- `mcp`: the rmcp `ServerHandler`, the tool context, the error mapping, and the read tools: `list_catalogs`, `search` (cross-catalog merge, and the `tags` wording, #34), `get_concept`, `read_document`, `history`, `open_questions`, `oldest`, `sprawl`, `links_report`, `lint`.
- `http`: the router, `/healthz`, `/mcp`, `GET /catalogs/{name}/export`, `POST /catalogs/{name}/import`, request tracing and timeouts, and graceful shutdown.
- `okf-mcp serve`.
- End-to-end tests over real HTTP with rmcp's client, and the M4 smoke test on the real catalog.

Deliberately deferred:

| Item | Goes to | Why |
|---|---|---|
| `pending_changes`, `stranded_sessions`, `lint pending`, the write tools, changesets, overlays, publish | M5 | Design milestone table: they need a changeset |
| How a changeset is identified when a client negotiates MCP `2026-07-28` (no sessions; see Decision 2), and binding it, and the session's streams, to the key that created it (an M4 session is not tied to a key) | M5 (design question) | Sessions only matter to changesets |
| Discarding empty changesets on session close or idle, the 24-hour prune | M5 | Design "Session lifecycle"; M4 creates no changesets. M4 does set rmcp's session idle timeout from `session_idle_timeout` |
| The NTP offset check, `time_server`, the clock offset in `/healthz` | M7 | Design milestone table |
| `move_concept`, `split_concept`, `delete_concept` | M6 | Design milestone table |
| TLS to Postgres (an `sqlx` TLS feature) | M7 | A deployment decision (M3 plan, verified fact 1) |
| Admin MCP tools | never | Design "Out of scope" |

## Decisions taken while planning (2026-11-02)

1. **Path tools search the whole read set; reports use one catalog.** Without a `catalog` argument, `get_concept`, `read_document` and `history` look through the selected catalogs in order and answer from the first that has the path, naming that catalog in the result. The whole-catalog reports (`open_questions`, `oldest`, `sprawl`, `links_report`, `lint`) run on one catalog: the `catalog` argument, else the first selected catalog. `search` spans the read set (design); `list_catalogs` lists every catalog the key reaches.
2. **Serve every MCP protocol version; fix changeset identity in M5.** MCP `2026-07-28` removes sessions, and rmcp 3.8 serves a client that negotiates it statelessly, with no `Mcp-Session-Id` (verified fact 3). M4's read tools need no session, so M4 accepts every version rmcp knows; rmcp's own client negotiates `2025-11-25` and gets a session. The design's "one changeset per (MCP session, catalog)" needs a session id that `2026-07-28` clients will not send; the M5 plan redesigns how a changeset is identified so it works with and without sessions. Task 12 records this as an open M5 question in the design.

## Simplifications and deviations relative to the design

- **Keys are looked up by hash in a `HashMap`, not compared in constant time.** The design says incoming keys "are hashed and compared in constant time". The cache maps each live key's SHA-256 to its principal, and a request's key is hashed and looked up. A timing difference in the lookup can reveal at most how a guess's *hash* compares with stored hashes, which does not help find a key (SHA-256 is preimage-resistant); scanning every key with `subtle` would cost O(keys) per request for no gain. `keys::matches` stays for callers that compare one stored hash. Task 2 updates the design's "Configuration" paragraph.
- **The text content of a tool result is the JSON itself, not a summary.** rmcp's `Json<T>` puts the serialized value in both `structuredContent` and one text block (verified fact 5); the design's "plus a short text summary" would mean a hand-written summary per tool for clients that ignore structured content. Task 12 records it.
- **Report tools wrap `okf.py`'s JSON lists in an object** (`oldest` returns `{"rows": [...]}`, `sprawl` `{"reports": [...]}`), so every output schema has an object root, which clients on older protocol versions require. The rows are unchanged, field for field.
- **`open_questions` reuses `oldest`.** `oldest` with `open_questions_only` already collects every concept's queued questions (with `okf.py`'s `str()` of each), so the tool numbers them instead of adding an analysis. Concepts come in `oldest`'s order (stalest first).
- **The import route's body cap has slack.** It is `max_import_bytes` plus 0.1% and 64 KiB, since gzip makes incompressible input slightly larger; the exact limit is enforced on the decompressed stream, as for the CLI.
- **Export is built in memory, not streamed.** `store::export` (M3) writes the whole `.tar.gz` into a `Vec<u8>` on a blocking thread; the route returns it with a `Content-Length`. The real catalog's export is about 1 MB and took 62 ms in M3's smoke test.
- **The catalog list is polled directly.** Every tick reads `admin_generation` (for the key cache) and the catalog list (`id`, `name`, `generation`); a new, deleted or re-created catalog shows up in the list whether or not `admin_generation` moved, and a moved `generation` triggers `CatalogState::refresh`. Two small queries per tick.
- **A path that is not in a granted catalog is "not available", never "forbidden".** The routes answer 404 for a catalog that does not exist *or* that the key does not reach (as `CatalogNotSelected` already words it), so a key cannot probe which catalogs exist; 403 is only for a reachable catalog the key's access level does not allow (a `read` key importing).
- **Request timeouts apply to every route but `/mcp`.** `/mcp`'s `GET` is a long-lived SSE stream that a timeout would cut; rmcp's keep-alive and `session_idle_timeout` bound it instead. `POST /mcp` (a tool call) has no timeout either: every M4 tool reads an in-memory snapshot, and the slowest, `links_report` with mentions, took 319 ms on the real catalog in M3's smoke test. A per-tool timeout can come with M5's write tools if they need one.

## Verified facts the code relies on

Each was checked against the source on 2026-11-02, and facts 1 to 10 also by a probe crate (`axum` 0.8.9, `rmcp` 3.8.0, `reqwest` 0.13.5, `schemars` 1.2.3, `humantime-serde` 1.1.1) whose tests passed; "(probe)" marks a detail only the probe showed. `R` is `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

1. **rmcp 3.8.0 features.** Default features are `base64`, `macros`, `server` (`R/rmcp-3.8.0/Cargo.toml:89`); `server` enables `schemars` (`:119`), and `transport-streamable-http-server` is at `:169`. `transport-streamable-http-server` adds the HTTP service; the client side needs `client` and `transport-streamable-http-client-reqwest` (no TLS feature is needed for `http://`). rmcp's `schemars` dependency is `1.0` (`Cargo.toml:856`), so the crate uses `schemars = "1.2.3"`; the derive honours `#[serde(rename)]`, `#[serde(skip)]`, `#[serde(flatten)]` and doc comments (descriptions), accepts `&'static str` and `f64`, and gives `serde_json::Value` the schema `true` (probe).
2. **The HTTP service.** `StreamableHttpService::new(factory, Arc<LocalSessionManager>, StreamableHttpServerConfig)` (`R/rmcp-3.8.0/src/transport/streamable_http_server/tower.rs:1413`) calls `factory` (a `Fn() -> Result<S, io::Error>`) once per session, or once per request when stateless. `StreamableHttpServerConfig::default()` (`:194-208`): `legacy_session_mode: true`, `allowed_hosts: ["localhost", "127.0.0.1", "::1"]`, a fresh `CancellationToken`. Builders: `with_allowed_hosts` (`:211`; an entry without a port matches any port; an empty list disables the check, `:219`), `with_cancellation_token` (`:262`; cancelling it ends every session). A `Host` outside the list gets HTTP 403 (`:920`). axum mounts it with `Router::route_service("/mcp", service)` (`R/axum-0.8.9/src/routing/mod.rs:185`).
3. **Protocol versions.** `ProtocolVersion::KNOWN_VERSIONS` is `2024-11-05` to `2026-07-28` (`R/rmcp-3.8.0/src/model.rs:202`); `LATEST` is `2026-07-28` (`:174`), `LATEST_WITH_INITIALIZE` `2025-11-25` (`:199`). `legacy_session_mode` "only applies to legacy protocol versions (< 2026-07-28) ... requests negotiating that version are always served statelessly" (`tower.rs:83-90`). rmcp's client negotiated `2025-11-25` and received an `Mcp-Session-Id` in the probe.
4. **The request reaches the tool.** The service injects the request's `http::request::Parts` into each request's extensions (`tower.rs:1276-1330`, `:2066`), so a tool takes `Extension(parts): Extension<http::request::Parts>` (`rmcp::handler::server::tool::Extension`), and anything an axum middleware inserted into the request's extensions is in `parts.extensions`. The probe's auth middleware (`axum::middleware::from_fn_with_state`, `R/axum-0.8.9/src/middleware/from_fn.rs:164`, applied with `Router::route_layer`, `routing/mod.rs:321`) inserted a principal that the tool read, per request, in both session and stateless mode; a request without a key got 401 before rmcp saw it.
5. **Tool results.** `#[tool_router(router = name, vis = "pub(crate)")]` on an `impl` block generates `fn name() -> ToolRouter<Self>` (`R/rmcp-macros-3.8.0/src/tool_router.rs:14-28`); routers from several `impl` blocks (in different modules) combine with `+` (`R/rmcp-3.8.0/src/handler/server/router/tool.rs:604`; probe). `#[tool_handler(router = self.tool_router)]` on `impl ServerHandler` serves a stored router; without `router` it calls `Self::tool_router()` on every request (`R/rmcp-macros-3.8.0/src/tool_handler.rs:21-22`). A tool returning `Result<Json<T>, CallToolResult>` (`T: Serialize + JsonSchema`) gets an output schema from `T`; `Ok` becomes `CallToolResult::structured(value)` (`R/rmcp-3.8.0/src/handler/server/wrapper/json.rs:30-42`), whose content is one text block holding the JSON plus `structuredContent` (`model.rs:4318`); `Err(CallToolResult::structured_error(value))` gives `isError: true` (`model.rs:4345`). Arguments that do not deserialize give an `isError: true` result with text `failed to deserialize parameters: ...` (probe), not a JSON-RPC error. Output schemas need not have an object root in 3.8 (`handler/server/common.rs:120`), but older clients expect one.
6. **Server identity.** `ServerHandler::get_info` returns `ServerConfig` (alias of `InitializeResult`; `ServerInfo` is deprecated, `model.rs:1341,1368`): `ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(Implementation::new(name, version)).with_instructions(text)` (`model.rs:1307,1313`). `Implementation::from_build_env()` (`model.rs:1719`) reads rmcp's own build environment (`env!("CARGO_CRATE_NAME")` is `rmcp` there), so the crate passes `env!("CARGO_PKG_NAME")` and `env!("CARGO_PKG_VERSION")` itself.
7. **Sessions.** `LocalSessionManager::default()` has a public `session_config: SessionConfig` whose `keep_alive: Option<Duration>` (default 5 minutes) closes a session after that much inactivity (`R/rmcp-3.8.0/src/transport/streamable_http_server/session/local.rs:1220-1267`).
8. **The client, for tests.** `StreamableHttpClientTransportConfig::with_uri(url)` (`R/rmcp-3.8.0/src/transport/streamable_http_client.rs:2138`), `.auth_header(key)` (sends `Authorization: Bearer key`, `:2168`), `.custom_headers(HashMap<HeaderName, HeaderValue>)` (`:2196`; `accept` and `mcp-session-id` are refused); `StreamableHttpClientTransport::from_config(config)` (`src/transport/common/reqwest/streamable_http_client.rs:441`); `().serve(transport).await` (`rmcp::ServiceExt`) gives a `RunningService<RoleClient, ()>` with `list_all_tools()` and `call_tool(CallToolRequestParams::new(name).with_arguments(map))` returning `CallToolResult` (`src/service/client.rs:1855,2061`; `model.rs:4441`), and `cancel().await` closes it. A client whose key is refused fails in `serve`.
9. **axum and tower-http.** `axum::serve(listener, router).with_graceful_shutdown(signal)` (`R/axum-0.8.9/src/serve/mod.rs:151`) waits for open connections, so the rmcp cancellation token must be cancelled first or SSE streams hold shutdown open. `axum::extract::DefaultBodyLimit::max(bytes)` (`R/axum-core-0.5.6/src/extract/default_body_limit.rs:150`) caps a `Bytes` body (HTTP 413 beyond it). Path parameters are written `{name}` in 0.8. `tower_http::timeout::TimeoutLayer::new` is deprecated since 0.6.7; use `TimeoutLayer::with_status_code(StatusCode, Duration)` (`R/tower-http-0.6.11/src/timeout/service.rs:28-34`). `tower_http::trace::TraceLayer::new_for_http()` (`R/tower-http-0.6.11/src/trace/layer.rs:182`) logs requests through `tracing`.
10. **humantime-serde 1.1.1.** `#[serde(with = "humantime_serde")]` on a `std::time::Duration` field reads `"10s"`, `"2h"`, `"90m"`; `"soon"` fails with "invalid value: string \"soon\", expected a duration" (probe, through `toml` 1.1).
11. **M3 API used here.** `CatalogState::{load, id, name, current, refresh}` and `Loaded::{generation, snapshot, lint}` (`src/catalog.rs`); `store::catalogs::{list, find, create, admin_generation, Catalog}` (`src/store/catalogs.rs:18,60,109,130,256`); `store::keys::{list, create, revoke, Access, Grants, NewKey, KeyRecord}` (`src/store/keys.rs`; `Access` derives `Ord`, `Read < Write < Admin`); `keys::hash(&str) -> [u8; 32]` (`src/keys.rs:43`); `store::export::export(pool, name, W) -> W` with `W: Write + Send + 'static` (`src/store/export.rs:35`); `store::import::{import, ImportOptions, ImportReport}` (`ImportReport: Serialize`; `src/store/import.rs:21-42,99`); `archive::{read_tarball, read_directory, Files}` (`src/archive.rs:42,72,188`); the `revisions` table (`migrations/0001_schema.sql:99-115`, primary key `(catalog_id, path, revision)`); `Snapshot::{get, docs, backlinks, resolve_concept}` and `SnapshotDoc::{path, kind, title, frontmatter, links, raw, doc}` (`src/okf/snapshot.rs`); `okf::search::{search, SearchQuery, SearchHit}` (`src/okf/search.rs:131-210`; `SearchHit.rank` is `#[serde(skip)]`); `oldest`, `sprawl`, `links_report`, `lint` and their report types (`src/okf/analysis/*.rs`); `paths::{normpath, escapes_root, basename}` (`src/okf/paths.rs`); `yaml::Value::to_json` (`src/okf/yaml.rs:57`); `crate::db::test_db()` and `tests/support/pg.rs`'s `TestDb::start()`.
12. **Brooke** (`brooke-reviews[bot]`) reviews each PR once and skips a branch behind `main`; `/prrb` re-triggers it, and no eyes reaction about a minute after a request means it was missed (catalog `/processes/subagent-pr-workflow.md`).

## Conventions for every task

- Work in the worktree and branch the coordinator gives you (`.worktrees/okf_mcp-<issue>` off `origin/main`). Never edit the main checkout.
- The user is a Python expert and a Rust novice: comment Rust-specific ideas (`async`/`.await`, `?`, `Arc`/`RwLock`, trait objects, closures, `spawn_blocking`, derives, macros such as `#[tool]`), as M1 to M3 do.
- No `.unwrap()` outside tests; `.expect("why this can't fail")` only for invariants (poisoned locks, literal values).
- Every public item has a doc comment with Arguments, Returns and Errors where they apply.
- **Never log an API key, `DATABASE_URL`, or document text.** Log a key's id at most.
- CPU work (an analysis, a search, building a tarball) runs on `tokio::task::spawn_blocking` (through `mcp::blocking`), never on an async thread.
- Every timestamp comes from `app.clock()`; tools never accept one.
- SQL is literal text (`&'static str`), parameters always bound.
- Tests: Arrange-Act-Assert, `#[cfg(test)] mod tests` beside the code. A test that needs Postgres is `#[tokio::test(flavor = "multi_thread")]` and starts its own database (`crate::db::test_db().await` in unit tests, `support::server::TestServer::start` in integration tests). No commented-out tests. Test keys are plain strings such as `"reader-key"`, never key-shaped strings (`okf_` plus 43 characters).
- **Gates** (all must pass before every push; paste the counts into the PR):
  - `cargo fmt --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test` (needs Docker)
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
  - `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored` (live parity; needs `python3`)
  - `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`
  - Cargo puts test-name filters after `--`: `cargo test --lib -- auth selection`.
- **Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m4-task-<NN>-<slug>.md`, with Added/Changed/Notes sections like `changelog/2026-11-02T173703Z-m3-task-13-wrap-up.md`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with the Claude Code attribution line.
- The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## File map

| File | Created/modified by | Responsibility |
|---|---|---|
| `Cargo.toml` | T1 (`humantime-serde`), T2 (`axum`; dev `tower`), T3 (`form_urlencoded`), T4 (`tokio-util`), T5 (`rmcp`, `schemars`), T6 (`tower-http`, tokio `net` and `signal`; dev `rmcp` client, `reqwest`) | Dependencies (keep both sides on conflict) |
| `src/config.rs` | T1 (modify) | The server's configuration fields and their validation |
| `src/error.rs` | T2 (`Forbidden`), T5 (`kind`, `to_json`) | Error variants and their JSON form |
| `src/auth.rs` | T2 (create) | `Principal`, `KeyCache`, `Keys`, `bearer`, `require_key` |
| `src/selection.rs` | T3 (create) | `connection_catalogs`, `Selection`, `resolve` |
| `src/lib.rs` | T2 (`auth`), T3 (`selection`), T4 (`app`), T5 (`mcp`), T6 (`http`) | `pub mod` lines (keep both sides) |
| `src/catalog.rs` | T4 (modify) | `CatalogState::description`, `Loaded::{files, last_change}` |
| `src/app.rs` | T4 (create) | `App`: the shared state, `poll`, `poll_until` |
| `src/mcp/mod.rs` | T5 (create); T7, T8, T9, T10 (one `+ Self::..._router()` line each) | `OkfServer`, `ServerHandler`, `ToolResult`, `reply`, `blocking` (keep both sides) |
| `src/mcp/context.rs` | T5 (create) | `ToolContext`: principal, selection, catalog states |
| `src/mcp/testing.rs` | T5 (create) | Test-only: an `App` over a test database with imported catalogs |
| `src/mcp/catalogs.rs` | T5 (create) | `list_catalogs` |
| `src/http/mod.rs` | T6 (create), T11 (one `.merge` line) | Router, `AppError`, `/healthz`, `serve` |
| `src/cli.rs` | T6 (modify) | `serve` and its `--listen` flag; `run` takes an `Arc<dyn Clock>` |
| `src/main.rs` | T6 (modify) | Passes `Arc::new(SystemClock)` |
| `tests/support/mod.rs`, `tests/support/server.rs` | T6 (create `server.rs`, add `pub mod server;`) | `TestServer`: Postgres, the app, the router on a port, clients |
| `tests/server.rs` | T6 (create), T11 (archive route tests) | End-to-end tests over HTTP |
| `src/okf/search.rs` | T7 (modify) | `search_catalogs`, `CatalogHit`, `JsonSchema` on `SearchHit` |
| `src/mcp/search.rs` | T7 (create) | `search` (#34) |
| `src/okf/doc.rs` | T8 (modify) | `Doc::body` |
| `src/mcp/documents.rs` | T8 (create) | `get_concept`, `read_document` |
| `src/store/history.rs`, `src/store/mod.rs` | T9 (create; add `pub mod history;`) | Revision rows of one path |
| `src/mcp/history.rs` | T9 (create) | `history` |
| `src/okf/analysis/{mod,lint,links,oldest,sprawl}.rs` | T10 (derive `JsonSchema`) | Report types gain schemas |
| `src/mcp/reports.rs` | T10 (create) | `open_questions`, `oldest`, `sprawl`, `links_report`, `lint` |
| `src/http/archives.rs` | T11 (create) | `GET /catalogs/{name}/export`, `POST /catalogs/{name}/import` |
| `tests/smoke.rs` | T12 (modify) | The real catalog over HTTP, with timings |
| `README.md` | T12 (modify) | Running the server, `mcp.json`, routes |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | T1 (configuration), T2 (key lookup), T5 (tool results, read set), T6 (routes, `/healthz`), T12 (M4 delivered, what M4 shipped differently, the M5 session question) | Design |
| `changelog/*-m4-*.md` | every task | Iteration notes |

## Task dependencies

```
T1 config ──────────────────┐
T2 auth ──> T3 selection ───┼──> T5 mcp core + list_catalogs ──┬──> T6 http + serve ──> T11 archive routes ──┐
T2 ─────────────────────────┴─> T4 app + poll (needs T1, T2) ──┘   T7 search (#34) ───────────────────────────┤
                                                                   T8 get_concept, read_document ──> (T11) ───┼──> T12 smoke, README, design, changelog
                                                                   T9 history ────────────────────────────────┤
                                                                   T10 reports ───────────────────────────────┘
```

Edges, explicitly:

- T2 -> T3
- T1 -> T4; T2 -> T4
- T3 -> T5; T4 -> T5
- T5 -> T6; T5 -> T7; T5 -> T8; T5 -> T9; T5 -> T10
- T6 -> T11; T8 -> T11 (its test reads the imported catalog with `get_concept`)
- every task -> T12

Tasks with no dependencies, which can start straight away: **T1, T2**. T7, T8, T9 and T10 can run beside T6 and each other: each creates its own `src/mcp/<tool>.rs` and adds one router line to `src/mcp/mod.rs` (keep both sides). T1 and T2 both touch `Cargo.toml`; T2, T3, T4 and T5 each add a `pub mod` line to `src/lib.rs`. Keep both sides.

---
### Task 1: `config`: the server's fields

The server reads five more settings: where to listen, which `Host` names rmcp accepts, the server's default catalog, how often to poll, and how long an idle MCP session lives. Durations are written as `"10s"` or `"2h"`. A value that would break the server (a zero interval, an empty host list, which rmcp treats as "accept any host") is refused when the file is loaded.

**Files:**
- Modify: `Cargo.toml` (add `humantime-serde`)
- Modify: `src/config.rs`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Configuration")
- Create: `changelog/<ts>-m4-task-01-config.md`

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` `[dependencies]`, after `getrandom`:

```toml
# Durations with units in the configuration ("10s", "2h").
humantime-serde = "1.1.1"
```

- [ ] **Step 2: Write the failing tests**

Replace the `tests` module of `src/config.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = Config::from_toml("").unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(config.max_import_bytes.as_u64(), 64 * 1024 * 1024);
        assert_eq!(config.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(config.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert_eq!(config.default_catalog, None);
        assert_eq!(config.poll_interval, Duration::from_secs(10));
        assert_eq!(config.session_idle_timeout, Duration::from_secs(2 * 60 * 60));
    }

    #[test]
    fn every_field_reads_from_the_file_and_unknown_keys_are_ignored() {
        let text = "max_import_bytes = \"1MiB\"\n\
                    listen = \"127.0.0.1:9000\"\n\
                    allowed_hosts = [\"okf-mcp.example\"]\n\
                    default_catalog = \"pg\"\n\
                    poll_interval = \"90s\"\n\
                    session_idle_timeout = \"30m\"\n\
                    time_server = \"ntp.example\"\n";

        let config = Config::from_toml(text).unwrap();

        assert_eq!(config.max_import_bytes.as_u64(), 1024 * 1024);
        assert_eq!(config.listen.to_string(), "127.0.0.1:9000");
        assert_eq!(config.allowed_hosts, ["okf-mcp.example"]);
        assert_eq!(config.default_catalog.as_deref(), Some("pg"));
        assert_eq!(config.poll_interval, Duration::from_secs(90));
        assert_eq!(config.session_idle_timeout, Duration::from_secs(30 * 60));
    }

    #[test]
    fn sizes_take_plain_numbers() {
        let config = Config::from_toml("max_import_bytes = 2048\n").unwrap();
        assert_eq!(config.max_import_bytes.as_u64(), 2048);
    }

    #[test]
    fn a_bad_value_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        std::fs::write(&path, "poll_interval = \"soon\"\n").unwrap();

        let error = Config::load(Some(&path)).unwrap_err();

        assert!(matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")));
    }

    #[test]
    fn values_that_would_break_the_server_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        for (text, needle) in [
            ("poll_interval = \"0s\"\n", "poll_interval"),
            ("session_idle_timeout = \"0s\"\n", "session_idle_timeout"),
            ("allowed_hosts = []\n", "allowed_hosts"),
            (
                "allowed_hosts = [\"\"]\n",
                "allowed_hosts has a blank entry",
            ),
            (
                "allowed_hosts = [\"localhost\", \"  \"]\n",
                "allowed_hosts has a blank entry",
            ),
        ] {
            std::fs::write(&path, text).unwrap();

            let error = Config::load(Some(&path)).unwrap_err();

            assert!(error.to_string().contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn no_file_gives_the_defaults() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib -- config`
Expected: compile errors (no field `listen`, `Duration` not imported).

- [ ] **Step 4: Implement**

In `src/config.rs`, replace the module doc, the imports, the constant, the struct, its `Default` and `Config::load` with:

```rust
//! The configuration file (TOML; the design's "Configuration"). Each
//! milestone reads the fields it uses (M4: everything but `time_server`,
//! which the M7 clock check reads). Unknown keys are ignored, so one file
//! serves every version of the server.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::time::Duration;

use bytesize::ByteSize;
use serde::Deserialize;

use crate::error::OkfError;

/// The default import limit: 64 MiB, uncompressed.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;

/// The default listen address: every IPv4 interface, port 8080.
pub const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080));

/// rmcp's default `Host` allow-list (loopback only): right for a local run,
/// wrong behind an ingress, where the config must name the public host.
pub const DEFAULT_ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// How often the server looks for changes made outside it.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long an MCP session may sit idle before it is closed.
pub const DEFAULT_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// Settings read from the configuration file.
// `#[serde(default)]` fills every missing key from `Default::default()`.
// serde ignores keys the struct does not name unless told otherwise
// (`deny_unknown_fields`), which is what lets later milestones' keys through.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The largest uncompressed total an import may expand to (`"64MiB"`, or
    /// a number of bytes). `MiB` is binary; `MB` and `M` are decimal. For a
    /// tarball it counts the whole decompressed tar stream (headers and
    /// 512-byte padding included); for a directory, the files' contents.
    pub max_import_bytes: ByteSize,
    /// The address `serve` listens on (`"0.0.0.0:8080"`); `--listen` overrides it.
    pub listen: SocketAddr,
    /// The `Host` names (optionally `host:port`) MCP requests may carry, the
    /// DNS-rebinding check. Behind an ingress, the public hostname.
    pub allowed_hosts: Vec<String>,
    /// The catalog a key with no default of its own reads and writes (design
    /// "Catalog selection", step 4).
    pub default_catalog: Option<String>,
    /// How often to look for imports, catalog and key changes made by the
    /// subcommands (`"10s"`).
    // `with = "humantime_serde"` parses the field through that module's
    // functions instead of `Duration`'s own serde impl (which wants
    // `{secs, nanos}`).
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    /// How long an MCP session may be idle before it is closed (`"2h"`).
    #[serde(with = "humantime_serde")]
    pub session_idle_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_import_bytes: ByteSize::b(DEFAULT_MAX_IMPORT_BYTES),
            listen: DEFAULT_LISTEN,
            allowed_hosts: Vec::from(DEFAULT_ALLOWED_HOSTS.map(String::from)),
            default_catalog: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            session_idle_timeout: DEFAULT_SESSION_IDLE_TIMEOUT,
        }
    }
}
```

> **Corrected (issue #65), 2026-11-02.** Two changes to the code above. (1) The
> `max_import_bytes` doc comment no longer ends "The import route also caps the request
> body at this size": the route does not exist until Task 11, and its cap is
> `max_import_bytes` plus slack (`body_limit`), not this size. Task 11 adds a sentence
> saying so. (2) `allowed_hosts` is built with `Vec::from(array)`, which moves the
> mapped array into the `Vec`, instead of `.to_vec()`, which copies it.
> Review of PR #76 added a third: `validate` also refuses a blank `allowed_hosts` entry
> (`""` or whitespace), which would otherwise reject every request with no hint why;
> the code below and Step 2's refusal test include it.

Keep `from_toml` as it is. Add `validate` after it and call it from `load`:

```rust
    /// Checks the values serde cannot: ones that would break the server.
    ///
    /// # Returns
    /// `Ok(())`, or the problem in words.
    ///
    /// # Errors
    /// A zero `poll_interval` (tokio's interval panics on zero) or
    /// `session_idle_timeout`, an empty `allowed_hosts` (which rmcp reads as
    /// "accept any `Host`"), or a blank entry in it (which no `Host` matches,
    /// so every request would get 403 with no hint why).
    pub fn validate(&self) -> Result<(), String> {
        if self.poll_interval.is_zero() {
            return Err("poll_interval must be more than zero".to_string());
        }
        if self.session_idle_timeout.is_zero() {
            return Err("session_idle_timeout must be more than zero".to_string());
        }
        if self.allowed_hosts.is_empty() {
            return Err(
                "allowed_hosts is empty; list the host names clients connect to".to_string(),
            );
        }
        // `trim` returns a borrowed slice (`&str`) without the surrounding
        // whitespace, so the check allocates nothing.
        if self.allowed_hosts.iter().any(|host| host.trim().is_empty()) {
            return Err("allowed_hosts has a blank entry; remove it or name a host".to_string());
        }
        Ok(())
    }

    /// Reads the configuration file, or returns the defaults without one.
    ///
    /// # Arguments
    /// * `path` - the file (`--config` or `OKF_MCP_CONFIG`), if any.
    ///
    /// # Returns
    /// The file's settings, or [`Config::default`] when `path` is `None`.
    ///
    /// # Errors
    /// [`OkfError::Config`] if the file can't be read or parsed, or fails
    /// [`Config::validate`].
    pub fn load(path: Option<&Path>) -> Result<Self, OkfError> {
        // `let ... else` is a pattern match that must diverge (here `return`)
        // when the pattern does not match: an early return for `None`.
        let Some(path) = path else {
            return Ok(Self::default());
        };
        // A closure (Python's lambda) that wraps a message with the path.
        let error = |message: String| OkfError::Config {
            path: path.display().to_string(),
            message,
        };
        let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
        let config = Self::from_toml(&text).map_err(|e| error(e.to_string()))?;
        config.validate().map_err(error)?;
        Ok(config)
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- config`
Expected: 6 passed.

- [ ] **Step 6: Update the design**

In "Configuration", after "Each milestone reads only its own fields (M3: `max_import_bytes`)", make it read: "Each milestone reads only its own fields (M3: `max_import_bytes`; M4: the rest except `time_server`, which M7's clock check reads); unknown keys are ignored, so one file serves every version. A zero `poll_interval` or `session_idle_timeout`, or an empty `allowed_hosts` (which rmcp would read as "accept any host") or a blank entry in it, is refused at load."

- [ ] **Step 7: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-01-config.md` (Added: the five fields, `Config::validate`; Notes: `time_server` waits for M7). Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/config.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: the server's configuration fields (M4.01)"
```

---

### Task 2: `auth`: principals, the key cache and the bearer middleware

A request authenticates with `Authorization: Bearer <key>`. The server keeps every live key in memory, keyed by its SHA-256, and the middleware turns a presented key into an `Arc<Principal>` (the key's id, access level, reachable catalogs, default catalog and actor) in the request's extensions, where rmcp passes it on to the tools (verified fact 4). A missing, malformed, unknown or revoked key gets HTTP 401. `Principal::require` answers "may this key do X to catalog C?" for the routes and tools.

**Files:**
- Modify: `Cargo.toml` (add `axum`; dev `tower`)
- Modify: `src/error.rs` (add `Forbidden`)
- Create: `src/auth.rs`
- Modify: `src/lib.rs` (`pub mod auth;`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Configuration", the key sentence)
- Create: `changelog/<ts>-m4-task-02-auth.md`

- [ ] **Step 1: Dependencies and the error variant**

In `Cargo.toml` `[dependencies]`, first in the list:

```toml
# The HTTP server: routes, the auth middleware, extractors.
axum = "0.8.9"
```

and in `[dev-dependencies]`:

```toml
# `ServiceExt::oneshot`, to call an axum router in tests without a socket.
tower = { version = "0.5.3", features = ["util"] }
```

In `src/error.rs`, add after `CatalogNotSelected`:

```rust
    /// The key reaches the catalog but its access level does not allow this.
    #[error("forbidden: {reason}")]
    Forbidden {
        /// What was refused, and the access it needs.
        reason: String,
    },
```

- [ ] **Step 2: Write the failing tests**

Create `src/auth.rs` with only the tests module first (the code goes above it in Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use chrono::Utc;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    fn record(id: &str, key: &str, revoked: bool) -> KeyRecord {
        KeyRecord {
            id: id.to_string(),
            sha256: keys::hash(key).to_vec(),
            access: Access::Read,
            all_catalogs: false,
            catalogs: vec!["pg".to_string()],
            default_catalog: Some("pg".to_string()),
            actor: format!("actor-{id}"),
            created_at: Utc::now(),
            revoked_at: revoked.then(Utc::now),
        }
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().unwrap());
        headers
    }

    #[test]
    fn bearer_reads_the_token_with_any_scheme_case() {
        assert_eq!(bearer(&headers("Bearer reader-key")), Some("reader-key"));
        assert_eq!(bearer(&headers("bearer  reader-key ")), Some("reader-key"));
        assert_eq!(bearer(&headers("Basic reader-key")), None);
        assert_eq!(bearer(&headers("Bearer ")), None);
        assert_eq!(bearer(&headers("reader-key")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn the_cache_finds_live_keys_only() {
        let cache = KeyCache::from_records(vec![
            record("reader", "reader-key", false),
            record("old", "old-key", true),
        ]);

        let found = cache.find("reader-key").expect("live key");

        assert_eq!(found.key_id, "reader");
        assert_eq!(found.actor, "actor-reader");
        assert_eq!(found.default_catalog.as_deref(), Some("pg"));
        assert!(cache.find("old-key").is_none());
        assert!(cache.find("no-such-key").is_none());
    }

    #[test]
    fn require_checks_reach_before_access() {
        let principal = Principal::from(record("reader", "reader-key", false));

        assert!(principal.require(Access::Read, "pg").is_ok());
        assert!(matches!(
            principal.require(Access::Admin, "pg"),
            Err(OkfError::Forbidden { .. })
        ));
        // A catalog the key does not reach is "not available", whatever the access.
        assert!(matches!(
            principal.require(Access::Read, "work"),
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "work"
        ));
        let mut everything = record("all", "all-key", false);
        everything.all_catalogs = true;
        everything.catalogs.clear();
        assert!(Principal::from(everything).reaches("work"));
    }

    /// A router whose one route echoes the principal's key id.
    fn app(cache: KeyCache) -> Router {
        let keys = Arc::new(Keys::new(cache));
        Router::new()
            .route(
                "/",
                get(|request: Request| async move {
                    request
                        .extensions()
                        .get::<Arc<Principal>>()
                        .map(|principal| principal.key_id.clone())
                        .unwrap_or_default()
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(keys, require_key))
    }

    async fn status_and_body(router: Router, authorization: Option<&str>) -> (StatusCode, String) {
        let mut request = axum::http::Request::builder().uri("/");
        if let Some(value) = authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        let response = router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn the_middleware_admits_a_known_key_and_refuses_the_rest() {
        let cache = || KeyCache::from_records(vec![record("reader", "reader-key", false)]);

        let admitted = status_and_body(app(cache()), Some("Bearer reader-key")).await;
        let unknown = status_and_body(app(cache()), Some("Bearer other-key")).await;
        let missing = status_and_body(app(cache()), None).await;

        assert_eq!(admitted, (StatusCode::OK, "reader".to_string()));
        assert_eq!(unknown.0, StatusCode::UNAUTHORIZED);
        assert_eq!(missing.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keys_load_from_the_database_and_reload_after_a_revoke() {
        use crate::store::{catalogs, keys as key_rows};
        let db = crate::db::test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        let names = ["pg".to_string()];
        let new = key_rows::NewKey {
            id: "reader",
            access: Access::Read,
            grants: key_rows::Grants::Catalogs(&names),
            default_catalog: Some("pg"),
            actor: "tester",
        };
        let key = key_rows::create(&db.pool, &new, Utc::now()).await.unwrap();

        let loaded = Keys::load(&db.pool).await.unwrap();
        assert_eq!(loaded.find(&key).unwrap().key_id, "reader");
        key_rows::revoke(&db.pool, "reader", Utc::now()).await.unwrap();
        // The cache keeps the key until it is reloaded (the poll loop does that).
        assert!(loaded.find(&key).is_some());
        loaded.reload(&db.pool).await.unwrap();

        assert!(loaded.find(&key).is_none());
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib -- auth`
Expected: compile errors (`bearer`, `KeyCache`, `Keys`, `Principal`, `require_key` not found).

- [ ] **Step 4: Implement**

Put this above the tests in `src/auth.rs`:

```rust
//! API-key authentication (design "Data flow", "Read", step 1): the
//! in-memory key cache, the [`Principal`] a request acts as, and the axum
//! middleware that requires a key on every route but `/healthz`.
//!
//! Keys are looked up by their SHA-256 in a `HashMap`. A timing difference
//! in that lookup can tell an attacker only how the *hash* of a guess
//! compares with stored hashes, which does not help find a key, so there is
//! no constant-time scan (the design's deviation, recorded in the M4 plan).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::keys;
use crate::store::keys::{self as key_rows, Access, KeyRecord};

/// The catalogs a key may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Every catalog, present and future (`key create --all`).
    All,
    /// These catalogs, by name.
    Catalogs(BTreeSet<String>),
}

/// Who a request acts as: one live API key (never the key itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The key's id.
    pub key_id: String,
    /// What it may do.
    pub access: Access,
    /// Which catalogs it reaches.
    pub reach: Reach,
    /// Its default catalog (design "Catalog selection", step 3).
    pub default_catalog: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
}

impl Principal {
    /// Does the key reach this catalog?
    ///
    /// # Arguments
    /// * `catalog` - a catalog name.
    ///
    /// # Returns
    /// `true` for a key granted every catalog, or this one.
    pub fn reaches(&self, catalog: &str) -> bool {
        match &self.reach {
            Reach::All => true,
            Reach::Catalogs(names) => names.contains(catalog),
        }
    }

    /// Checks that the key may do something needing `access` to `catalog`.
    ///
    /// # Arguments
    /// * `access` - the level the operation needs.
    /// * `catalog` - the catalog it acts on.
    ///
    /// # Errors
    /// [`OkfError::CatalogNotSelected`] if the key does not reach the
    /// catalog (worded like an unknown catalog, so a key cannot probe which
    /// catalogs exist); [`OkfError::Forbidden`] if it does but its level is
    /// lower than `access`.
    pub fn require(&self, access: Access, catalog: &str) -> Result<(), OkfError> {
        if !self.reaches(catalog) {
            return Err(OkfError::CatalogNotSelected {
                catalog: catalog.to_string(),
            });
        }
        // `Access` derives `Ord` in declaration order: Read < Write < Admin.
        if self.access < access {
            return Err(OkfError::Forbidden {
                reason: format!(
                    "key {:?} has {} access; this needs {}",
                    self.key_id,
                    self.access.as_str(),
                    access.as_str()
                ),
            });
        }
        Ok(())
    }
}

// `From` is Rust's conversion trait: `Principal::from(record)` (or
// `record.into()`) builds a principal from a stored key.
impl From<KeyRecord> for Principal {
    fn from(record: KeyRecord) -> Self {
        let reach = if record.all_catalogs {
            Reach::All
        } else {
            Reach::Catalogs(record.catalogs.into_iter().collect())
        };
        Self {
            key_id: record.id,
            access: record.access,
            reach,
            default_catalog: record.default_catalog,
            actor: record.actor,
        }
    }
}

/// Every live key, by SHA-256. Immutable: a reload builds a new one.
#[derive(Debug, Default)]
pub struct KeyCache {
    by_hash: HashMap<[u8; 32], Arc<Principal>>,
}

impl KeyCache {
    /// Builds the cache from stored keys, leaving out revoked ones.
    ///
    /// # Arguments
    /// * `records` - the keys, as [`key_rows::list`] returns them.
    ///
    /// # Returns
    /// The cache.
    pub fn from_records(records: Vec<KeyRecord>) -> Self {
        let by_hash = records
            .into_iter()
            .filter(|record| record.revoked_at.is_none())
            .filter_map(|record| {
                // The schema checks `length(sha256) = 32`, so this fails only
                // on a row edited by hand; such a key cannot authenticate.
                let hash: [u8; 32] = record.sha256.as_slice().try_into().ok()?;
                Some((hash, Arc::new(Principal::from(record))))
            })
            .collect();
        Self { by_hash }
    }

    /// The principal of a presented key.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked key.
    pub fn find(&self, presented: &str) -> Option<Arc<Principal>> {
        self.by_hash.get(&keys::hash(presented)).cloned()
    }
}

/// The key cache the server shares, replaced whole on reload.
#[derive(Debug)]
pub struct Keys {
    // As in `CatalogState`: a `std` lock held only to clone or swap the
    // `Arc`, never across an `.await`.
    cache: RwLock<Arc<KeyCache>>,
}

impl Keys {
    /// Wraps a cache.
    ///
    /// # Arguments
    /// * `cache` - the keys to start with.
    ///
    /// # Returns
    /// The shared cache.
    pub fn new(cache: KeyCache) -> Self {
        Self {
            cache: RwLock::new(Arc::new(cache)),
        }
    }

    /// Loads every live key from the database.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Returns
    /// The shared cache.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    pub async fn load(pool: &PgPool) -> Result<Self, OkfError> {
        Ok(Self::new(KeyCache::from_records(key_rows::list(pool).await?)))
    }

    /// Re-reads every key and swaps the new cache in.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Errors
    /// [`OkfError::Database`]; the old cache stays in place.
    pub async fn reload(&self, pool: &PgPool) -> Result<(), OkfError> {
        let cache = Arc::new(KeyCache::from_records(key_rows::list(pool).await?));
        *self.cache.write().expect("key cache lock poisoned") = cache;
        Ok(())
    }

    /// The principal of a presented key, from the current cache.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked key.
    pub fn find(&self, presented: &str) -> Option<Arc<Principal>> {
        self.cache
            .read()
            .expect("key cache lock poisoned")
            .find(presented)
    }
}

/// The token of an `Authorization: Bearer <token>` header.
///
/// # Arguments
/// * `headers` - the request's headers.
///
/// # Returns
/// The token (trimmed), or `None` without a header, with another scheme
/// (the scheme is matched case-insensitively, as RFC 6750 allows) or with an
/// empty token.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    // `then_some` turns a `bool` into `Some(value)` or `None`.
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// axum middleware: admits a request with a live key, inserting its
/// `Arc<Principal>` into the request's extensions, and answers anything
/// else with 401.
///
/// # Arguments
/// * `keys` - the shared key cache (the layer's state).
/// * `request` - the incoming request.
/// * `next` - the rest of the stack.
///
/// # Returns
/// The route's response, or 401 with `WWW-Authenticate: Bearer`.
// Used as `axum::middleware::from_fn_with_state(keys, require_key)`; axum
// fills each argument from the request (the extractor pattern).
pub async fn require_key(
    State(keys): State<Arc<Keys>>,
    mut request: Request,
    next: Next,
) -> Response {
    let principal = bearer(request.headers()).and_then(|key| keys.find(key));
    let Some(principal) = principal else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "missing or unknown API key\n",
        )
            .into_response();
    };
    tracing::debug!(key = %principal.key_id, "authenticated");
    request.extensions_mut().insert(principal);
    next.run(request).await
}
```

Add `pub mod auth;` to `src/lib.rs` (alphabetical, after `archive`), and mention `auth` in the crate doc's module list.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- auth`
Expected: 5 passed.

- [ ] **Step 6: Update the design**

In "Configuration", replace "incoming keys are hashed and compared in constant time" with "incoming keys are hashed and looked up by hash (a timing difference in the lookup reveals nothing about a key, so there is no constant-time scan)".

- [ ] **Step 7: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-02-auth.md`. Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/auth.rs src/error.rs src/lib.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: API-key auth: principals, key cache, bearer middleware (M4.02)"
```

---

### Task 3: `selection`: which catalogs a call acts on

Design "Catalog selection": the tool argument, else the connection's `X-OKF-Catalogs` header (or `?catalogs=` on the MCP URL), else the key's default, else the server's default; the first that is set wins. Every name must exist and be reachable by the key, or the call fails naming it (catalogs are never silently dropped). The first name is the default write target; the whole list is the read set. This task is pure: it takes the principal, the sources and an "exists?" test, so it needs no server.

**Files:**
- Modify: `Cargo.toml` (add `form_urlencoded`)
- Create: `src/selection.rs`
- Modify: `src/lib.rs` (`pub mod selection;`)
- Create: `changelog/<ts>-m4-task-03-selection.md`

- [ ] **Step 1: Dependency**

In `Cargo.toml` `[dependencies]`, after `flate2`:

```toml
# Decoding `?catalogs=` on the MCP URL.
form_urlencoded = "1.2.2"
```

- [ ] **Step 2: Write the failing tests**

Create `src/selection.rs` with the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Reach;
    use crate::store::keys::Access;

    fn principal(reach: &[&str], default: Option<&str>) -> Principal {
        Principal {
            key_id: "k".to_string(),
            access: Access::Read,
            reach: if reach.is_empty() {
                Reach::All
            } else {
                Reach::Catalogs(reach.iter().map(|name| name.to_string()).collect())
            },
            default_catalog: default.map(str::to_string),
            actor: "a".to_string(),
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    const EXISTING: [&str; 3] = ["pg", "work", "home"];

    fn exists(name: &str) -> bool {
        EXISTING.contains(&name)
    }

    fn select(principal: &Principal, sources: Sources<'_>) -> Result<Vec<String>, OkfError> {
        resolve(principal, sources, exists).map(|selection| selection.catalogs().to_vec())
    }

    #[test]
    fn the_first_source_that_is_set_wins() {
        let key = principal(&[], Some("home"));
        let argument = names(&["work"]);
        let connection = names(&["pg", "work"]);
        let all = Sources {
            argument: Some(&argument),
            connection: Some(&connection),
            server_default: Some("pg"),
        };

        assert_eq!(select(&key, all).unwrap(), ["work"]);
        let no_argument = Sources { argument: None, ..all };
        assert_eq!(select(&key, no_argument).unwrap(), ["pg", "work"]);
        let key_default = Sources { connection: None, ..no_argument };
        assert_eq!(select(&key, key_default).unwrap(), ["home"]);
        let server_default = principal(&[], None);
        assert_eq!(select(&server_default, key_default).unwrap(), ["pg"]);
    }

    #[test]
    fn an_empty_argument_list_counts_as_unset() {
        let key = principal(&[], None);
        let empty: Vec<String> = Vec::new();
        let connection = names(&["work"]);
        let sources = Sources {
            argument: Some(&empty),
            connection: Some(&connection),
            server_default: None,
        };

        assert_eq!(select(&key, sources).unwrap(), ["work"]);
    }

    #[test]
    fn the_first_name_is_the_write_target_and_duplicates_collapse() {
        let key = principal(&[], None);
        let connection = names(&["work", "pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
        };

        let selection = resolve(&key, sources, exists).unwrap();

        assert_eq!(selection.catalogs(), ["work", "pg"]);
        assert_eq!(selection.write_target(), "work");
    }

    #[test]
    fn an_unknown_or_ungranted_catalog_fails_naming_it() {
        let key = principal(&["pg"], None);
        for list in [&["pg", "nope"][..], &["work"][..]] {
            let connection = names(list);
            let sources = Sources {
                argument: None,
                connection: Some(&connection),
                server_default: None,
            };

            let error = select(&key, sources).unwrap_err();

            assert!(
                matches!(&error, OkfError::CatalogNotSelected { catalog } if catalog == list[list.len() - 1]),
                "{list:?}: {error}"
            );
        }
    }

    #[test]
    fn an_ungranted_server_default_is_an_error_too() {
        let key = principal(&["work"], None);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: Some("pg"),
        };

        assert!(matches!(
            select(&key, sources),
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "pg"
        ));
    }

    #[test]
    fn nothing_set_means_nothing_selected() {
        let key = principal(&[], None);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: None,
        };

        let error = select(&key, sources).unwrap_err();

        assert!(matches!(error, OkfError::Refused { .. }));
        assert!(error.to_string().contains("X-OKF-Catalogs"));
    }

    fn parts(uri: &str, header: Option<&str>) -> Parts {
        let mut request = axum::http::Request::builder().uri(uri);
        if let Some(value) = header {
            request = request.header(CATALOGS_HEADER, value);
        }
        request.body(()).unwrap().into_parts().0
    }

    #[test]
    fn the_connection_reads_the_header_before_the_query() {
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=home", Some(" pg, work ,"))),
            Some(names(&["pg", "work"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?x=1&catalogs=home%2Cpg", None)),
            Some(names(&["home", "pg"]))
        );
        assert_eq!(connection_catalogs(&parts("/mcp", Some(" , "))), None);
        assert_eq!(connection_catalogs(&parts("/mcp", None)), None);
    }
}
```

- [ ] **Step 3: Run them to see them fail**

Run: `cargo test --lib -- selection`
Expected: compile errors.

- [ ] **Step 4: Implement**

Above the tests:

```rust
//! Catalog selection (design "Catalog selection"): which catalogs a tool
//! call reads, and which one it would write.
//!
//! The sources, in order: the call's argument; the connection (the
//! `X-OKF-Catalogs` header, else `?catalogs=` on the MCP URL); the key's
//! default catalog; the server's `default_catalog`. The first that is set
//! wins, and every name in it must exist and be reachable by the key.

use axum::http::request::Parts;

use crate::auth::Principal;
use crate::error::OkfError;

/// The header naming a connection's catalogs (`pg,work`); HTTP header names
/// are case-insensitive, and `http` stores them lowercase.
pub const CATALOGS_HEADER: &str = "x-okf-catalogs";

/// The MCP URL's query parameter with the same meaning.
pub const CATALOGS_QUERY: &str = "catalogs";

/// The resolved catalogs of one call: never empty, no duplicates, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    catalogs: Vec<String>,
}

impl Selection {
    /// The read set, in order.
    pub fn catalogs(&self) -> &[String] {
        &self.catalogs
    }

    /// The default write target: the first catalog.
    pub fn write_target(&self) -> &str {
        // `resolve` never builds an empty selection.
        &self.catalogs[0]
    }
}

/// Where a call's catalogs can come from, before the key's default.
// `Copy`: three borrowed fields, so tests can build variants with `..base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sources<'a> {
    /// The tool call's `catalog` (as a one-name list) or `catalogs`.
    pub argument: Option<&'a [String]>,
    /// The connection's list ([`connection_catalogs`]).
    pub connection: Option<&'a [String]>,
    /// The server's `default_catalog`.
    pub server_default: Option<&'a str>,
}

/// Splits a comma-separated list, trimming names and dropping empty ones.
fn split_names(list: &str) -> Option<Vec<String>> {
    let names: Vec<String> = list
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// The catalogs a connection names: the `X-OKF-Catalogs` header, else the
/// `catalogs` query parameter.
///
/// # Arguments
/// * `parts` - the HTTP request (rmcp passes it to every tool).
///
/// # Returns
/// The names, or `None` if neither is set (or both are empty, or the header
/// is not ASCII).
pub fn connection_catalogs(parts: &Parts) -> Option<Vec<String>> {
    if let Some(header) = parts.headers.get(CATALOGS_HEADER) {
        return header.to_str().ok().and_then(split_names);
    }
    let query = parts.uri.query()?;
    // `parse` yields decoded `(key, value)` pairs (`%2C` becomes `,`).
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == CATALOGS_QUERY)
        .and_then(|(_, value)| split_names(&value))
}

/// Resolves a call's catalogs.
///
/// # Arguments
/// * `principal` - the calling key.
/// * `sources` - the argument, the connection and the server default.
/// * `exists` - whether a catalog of that name is loaded.
///
/// # Returns
/// The selection: the first source that is set, without duplicates.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] naming the first catalog that does not
/// exist or that the key does not reach; [`OkfError::Refused`] if no source
/// names a catalog.
// `impl Fn(&str) -> bool`: any function or closure taking a name, like a
// Python callable argument.
pub fn resolve(
    principal: &Principal,
    sources: Sources<'_>,
    exists: impl Fn(&str) -> bool,
) -> Result<Selection, OkfError> {
    let key_default = principal.default_catalog.as_deref();
    // An empty list (`catalogs: []`) counts as unset, so the selection is
    // never empty.
    let listed = sources.argument.filter(|list| !list.is_empty());
    let names: Vec<&str> = if let Some(list) = listed.or(sources.connection) {
        list.iter().map(String::as_str).collect()
    } else if let Some(name) = key_default.or(sources.server_default) {
        vec![name]
    } else {
        return Err(OkfError::Refused {
            reason: "no catalog selected: pass `catalog`, set the X-OKF-Catalogs header, \
                     or give the key a default catalog"
                .to_string(),
        });
    };
    let mut catalogs: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !exists(name) || !principal.reaches(name) {
            return Err(OkfError::CatalogNotSelected {
                catalog: name.to_string(),
            });
        }
        // A few names at most, so a linear `contains` beats a set.
        if !catalogs.iter().any(|seen| seen == name) {
            catalogs.push(name.to_string());
        }
    }
    Ok(Selection { catalogs })
}
```

Add `pub mod selection;` to `src/lib.rs` and the crate doc.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- selection`
Expected: 7 passed.

- [ ] **Step 6: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-03-selection.md`. Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/selection.rs src/lib.rs changelog/
git commit -m "feat: catalog selection from argument, connection, key and server defaults (M4.03)"
```

---

### Task 4: `app`: the shared state and the poll loop

`App` is what every route and tool shares: the pool, the configuration, the clock, the key cache, and one `CatalogState` per catalog, in a map swapped whole when the set of catalogs changes. `App::poll` is one pass of design "Noticing changes made outside the server": a moved `admin_generation` reloads the key cache; the catalog list adds new catalogs, drops deleted ones, reloads a catalog deleted and created again under the same name (a new id), and refreshes any whose `generation` moved. `App::poll_until` runs it every `poll_interval` until shutdown. `CatalogState` also learns the catalog's description, file count and last change, for `list_catalogs`.

**Files:**
- Modify: `Cargo.toml` (add `tokio-util`; tokio's `time` feature in `[dependencies]`)
- Modify: `src/catalog.rs`
- Create: `src/app.rs`
- Modify: `src/lib.rs` (`pub mod app;`)
- Create: `changelog/<ts>-m4-task-04-app.md`

- [ ] **Step 1: Dependencies**

In `Cargo.toml` `[dependencies]`, add after `tokio`:

```toml
# `CancellationToken`: one signal that stops the poll loop, rmcp's sessions
# and the HTTP server together (rmcp's config takes this type).
tokio-util = "0.7.20"
```

and add `"time"` to the `[dependencies]` tokio features (`tokio::time::interval` for the poll loop): `features = ["macros", "rt-multi-thread", "sync", "time"]`. The `[dev-dependencies]` tokio line, which only added `time`, can then go.

- [ ] **Step 2: Write the failing tests for `catalog.rs`**

Add to the tests module of `src/catalog.rs`:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn load_records_the_description_file_count_and_last_change() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "pgrace's world", Utc::now())
            .await
            .unwrap();
        let empty = CatalogState::load(&db.pool, "pg").await.unwrap();
        assert_eq!(empty.description(), "pgrace's world");
        assert_eq!(empty.current().files(), 0);
        assert_eq!(empty.current().last_change(), None);
        let entries: &[(&str, &[u8])] = &[("a.md", b"# A\n"), ("b.txt", b"b\n")];
        import_into(&db.pool, entries, false).await;

        empty.refresh(&db.pool).await.unwrap();

        let loaded = empty.current();
        assert_eq!(loaded.files(), 2);
        assert!(loaded.last_change().is_some());
    }
```

- [ ] **Step 3: Implement the `catalog.rs` changes**

In `Loaded`, add two fields and their accessors:

```rust
    files: usize,
    last_change: Option<DateTime<Utc>>,
```

```rust
    /// How many files the catalog holds (every stored file, markdown or not).
    pub fn files(&self) -> usize {
        self.files
    }

    /// The newest document change (`updated_at`), or `None` for an empty catalog.
    pub fn last_change(&self) -> Option<DateTime<Utc>> {
        self.last_change
    }
```

In `build`, before `rows` moves into the closure, next to `commit_times`:

```rust
    let files = rows.len();
    let last_change = rows.iter().map(|row| row.updated_at).max();
```

and add `files, last_change` to the `Loaded { .. }` it returns. In `CatalogState`, add a `description: String` field, set it from `catalog.description` in `load`, and add:

```rust
    /// The catalog's description (`catalog create --description`).
    pub fn description(&self) -> &str {
        &self.description
    }
```

Run: `cargo test --lib -- catalog`
Expected: every `catalog` test passes, the new one included.

- [ ] **Step 4: Write the failing tests for `app.rs`**

Create `src/app.rs` with the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::clock::SystemClock;
    use crate::db::test_db;
    use crate::okf::workspace::StoredFile;
    use crate::store::import::{import, ImportOptions};
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use chrono::Utc;

    async fn import_files(pool: &PgPool, catalog: &str, files: &[(&str, &str)], replace: bool) {
        let files: Files = files
            .iter()
            .map(|(path, text)| {
                let file = StoredFile {
                    content: text.as_bytes().to_vec(),
                    executable: false,
                };
                (path.to_string(), file)
            })
            .collect();
        let options = ImportOptions {
            replace,
            actor: "tester",
            now: Utc::now(),
        };
        import(pool, catalog, files, &options).await.unwrap();
    }

    async fn new_key(pool: &PgPool, id: &str) -> String {
        let new = NewKey {
            id,
            access: Access::Read,
            grants: Grants::All,
            default_catalog: None,
            actor: "tester",
        };
        key_rows::create(pool, &new, Utc::now()).await.unwrap()
    }

    async fn app(pool: &PgPool) -> Arc<App> {
        App::load(pool.clone(), Config::default(), Arc::new(SystemClock))
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_holds_every_catalog_and_live_key() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        catalogs::create(&db.pool, "work", "", Utc::now()).await.unwrap();
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let key = new_key(&db.pool, "reader").await;

        let app = app(&db.pool).await;

        let names: Vec<String> = app.catalogs().keys().cloned().collect();
        assert_eq!(names, ["pg", "work"]);
        assert_eq!(app.catalog("pg").unwrap().current().files(), 1);
        assert_eq!(app.keys().find(&key).unwrap().key_id, "reader");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_notices_imports_new_and_deleted_catalogs_and_revoked_keys() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        catalogs::create(&db.pool, "gone", "", Utc::now()).await.unwrap();
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        let pg_before = app.catalog("pg").unwrap();

        // Changes made by the subcommands, beside the running server.
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        catalogs::create(&db.pool, "work", "", Utc::now()).await.unwrap();
        catalogs::delete(&db.pool, "gone", false).await.unwrap();
        key_rows::revoke(&db.pool, "reader", Utc::now()).await.unwrap();
        app.poll().await.unwrap();

        let names: Vec<String> = app.catalogs().keys().cloned().collect();
        assert_eq!(names, ["pg", "work"]);
        // The same state, refreshed in place: readers holding it see the new snapshot.
        let pg_after = app.catalog("pg").unwrap();
        assert!(Arc::ptr_eq(&pg_before, &pg_after));
        assert_eq!(pg_after.current().files(), 1);
        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn overlapping_polls_leave_the_newest_key_list() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;

        key_rows::revoke(&db.pool, "reader", Utc::now()).await.unwrap();
        // `join!` runs both passes concurrently; `polling` serializes them.
        let (first, second) = tokio::join!(app.poll(), app.poll());

        first.unwrap();
        second.unwrap();
        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_reloads_a_catalog_created_again_under_the_same_name() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "first", Utc::now()).await.unwrap();
        let app = app(&db.pool).await;
        let first_id = app.catalog("pg").unwrap().id();

        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        catalogs::create(&db.pool, "pg", "second", Utc::now()).await.unwrap();
        app.poll().await.unwrap();

        let pg = app.catalog("pg").unwrap();
        assert_ne!(pg.id(), first_id);
        assert_eq!(pg.description(), "second");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_until_stops_when_cancelled() {
        let db = test_db().await;
        let config = Config {
            poll_interval: std::time::Duration::from_millis(20),
            ..Config::default()
        };
        let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&app).poll_until(shutdown.clone()));
        catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();

        // Within a few intervals the loop has picked the new catalog up.
        let mut seen = false;
        for _ in 0..100 {
            if app.catalog("pg").is_some() {
                seen = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        shutdown.cancel();

        assert!(seen, "the poll loop never loaded the new catalog");
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("poll_until returns after cancel")
            .unwrap();
    }
}
```

- [ ] **Step 5: Run them to see them fail**

Run: `cargo test --lib -- app::`
Expected: compile errors (`App` not found).

- [ ] **Step 6: Implement `app.rs`**

Above the tests:

```rust
//! The server's shared state (design "Architecture"): the database, the
//! configuration, the clock, the key cache and every catalog held in
//! memory, and the poll loop that notices changes made outside the server
//! (design "Noticing changes made outside the server").

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::auth::Keys;
use crate::catalog::CatalogState;
use crate::clock::Clock;
use crate::config::Config;
use crate::error::OkfError;
use crate::store::catalogs;

/// Every loaded catalog, by name.
pub type Catalogs = BTreeMap<String, Arc<CatalogState>>;

/// The state every route and tool shares. Built once by [`App::load`] and
/// shared as an `Arc<App>`.
pub struct App {
    pool: PgPool,
    config: Config,
    // `Arc<dyn Clock>`: any clock (the real one, or a fixed one in tests),
    // shared between threads.
    clock: Arc<dyn Clock>,
    keys: Arc<Keys>,
    // As in `CatalogState`: the lock is held only to clone or swap the `Arc`.
    catalogs: RwLock<Arc<Catalogs>>,
    // The `admin_generation` the key cache was loaded at. Written only under
    // `polling`, so `Relaxed` ordering is enough.
    admin_generation: AtomicI64,
    // Held for a whole `poll` pass. Without it, two overlapping passes (the
    // loop and a test's or a later milestone's direct call) could swap an
    // older key list in after a newer one while storing the newer counter,
    // and a revoked key would keep working until the next admin change.
    // tokio's `Mutex`: it is held across `.await`s.
    polling: tokio::sync::Mutex<()>,
}

impl App {
    /// Loads the key cache and every catalog's snapshot.
    ///
    /// # Arguments
    /// * `pool` - the database (migrated).
    /// * `config` - the server's settings.
    /// * `clock` - the source of every timestamp.
    ///
    /// # Returns
    /// The shared state.
    ///
    /// A catalog whose snapshot fails to build is logged and left out, as
    /// [`App::poll`] does, and the next poll tries it again: one bad catalog
    /// never takes the others offline.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the counter, the keys or the list.
    pub async fn load(
        pool: PgPool,
        config: Config,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, OkfError> {
        // Read the counter before the keys: a change committed between the
        // two moves it again, so the next poll reloads.
        let admin_generation = catalogs::admin_generation(&pool).await?;
        let keys = Arc::new(Keys::load(&pool).await?);
        let mut loaded = Catalogs::new();
        for row in catalogs::list(&pool).await? {
            match CatalogState::load(&pool, &row.name).await {
                Ok(state) => {
                    loaded.insert(row.name, Arc::new(state));
                }
                // Deleted between the list and the load.
                Err(OkfError::CatalogNotSelected { .. }) => {}
                // Not in the map, so the first poll tries to load it again.
                Err(error) => {
                    tracing::error!(catalog = %row.name, %error, "load failed; retrying at the next poll");
                }
            }
        }
        if let Some(name) = &config.default_catalog {
            if !loaded.contains_key(name) {
                tracing::warn!(catalog = %name, "default_catalog does not exist (yet)");
            }
        }
        Ok(Arc::new(Self {
            pool,
            config,
            clock,
            keys,
            catalogs: RwLock::new(Arc::new(loaded)),
            admin_generation: AtomicI64::new(admin_generation),
            polling: tokio::sync::Mutex::new(()),
        }))
    }

    /// The database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The server's settings.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The source of every timestamp.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// The key cache (shared with the auth layer).
    pub fn keys(&self) -> &Arc<Keys> {
        &self.keys
    }

    /// Every loaded catalog, as of now. Cheap (an `Arc` clone); one call
    /// gives a consistent view to check names against and take states from.
    pub fn catalogs(&self) -> Arc<Catalogs> {
        Arc::clone(&self.catalogs.read().expect("catalog map lock poisoned"))
    }

    /// One catalog's state.
    ///
    /// # Arguments
    /// * `name` - the catalog's name.
    ///
    /// # Returns
    /// Its state, or `None` if no such catalog is loaded.
    pub fn catalog(&self, name: &str) -> Option<Arc<CatalogState>> {
        self.catalogs().get(name).cloned()
    }

    /// One pass of the poll loop: reloads the key cache if `admin_generation`
    /// moved, and brings the catalogs in line with the catalog list.
    ///
    /// A catalog that fails to load or refresh, or a key reload that fails,
    /// is logged and retried at the next pass; the rest of the pass goes
    /// ahead. Passes never overlap: a call
    /// made while another runs waits for it, then makes its own pass.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the counter or the catalog list.
    pub async fn poll(&self) -> Result<(), OkfError> {
        // The guard (`_` prefix: kept only for its lifetime) releases the
        // mutex when the function returns.
        let _polling = self.polling.lock().await;
        let admin_generation = catalogs::admin_generation(&self.pool).await?;
        if admin_generation != self.admin_generation.load(Ordering::Relaxed) {
            // A failed reload leaves the counter unstored, so the next pass
            // retries it; the catalog changes below go ahead regardless.
            match self.keys.reload(&self.pool).await {
                Ok(()) => self
                    .admin_generation
                    .store(admin_generation, Ordering::Relaxed),
                Err(error) => {
                    tracing::error!(%error, "key reload failed; retrying at the next poll");
                }
            }
        }
        let current = self.catalogs();
        let mut next = Catalogs::new();
        for row in catalogs::list(&self.pool).await? {
            // The same id: the same catalog. A different id under the same
            // name: deleted and created again, so load it from scratch.
            let existing = current.get(&row.name).filter(|state| state.id() == row.id);
            let state = match existing {
                Some(state) => {
                    if row.generation > state.current().generation() {
                        if let Err(error) = state.refresh(&self.pool).await {
                            tracing::error!(catalog = %row.name, %error, "refresh failed; keeping the old snapshot");
                        }
                    }
                    Arc::clone(state)
                }
                None => match CatalogState::load(&self.pool, &row.name).await {
                    Ok(state) => {
                        tracing::info!(catalog = %row.name, "catalog loaded");
                        Arc::new(state)
                    }
                    Err(error) => {
                        tracing::error!(catalog = %row.name, %error, "load failed; retrying at the next poll");
                        continue;
                    }
                },
            };
            next.insert(row.name, state);
        }
        *self.catalogs.write().expect("catalog map lock poisoned") = Arc::new(next);
        Ok(())
    }

    /// Runs [`App::poll`] every `poll_interval` until `shutdown` is cancelled.
    ///
    /// # Arguments
    /// * `shutdown` - cancelled when the server stops.
    // `self: Arc<Self>`: the method takes a shared handle by value, so the
    // spawned task owns one and the `App` lives as long as the loop.
    pub async fn poll_until(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticks = tokio::time::interval(self.config.poll_interval);
        // After a slow pass, wait a full interval instead of catching up.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick completes at once, and the state was just loaded.
        ticks.tick().await;
        loop {
            // `select!` waits for whichever branch is ready first.
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {
                    if let Err(error) = self.poll().await {
                        tracing::error!(%error, "poll failed");
                    }
                }
            }
        }
    }
}
```

Add `pub mod app;` to `src/lib.rs` and the crate doc.

- [ ] **Step 7: Run the tests**

Run: `cargo test --lib -- app:: catalog`
Expected: the five `app` tests and every `catalog` test pass.

- [ ] **Step 8: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-04-app.md` (Notes: the catalog list is read every tick, so a new catalog appears without `admin_generation`; a failed refresh keeps the old snapshot; passes are serialized, so direct `poll` calls beside the loop are safe). Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/app.rs src/catalog.rs src/lib.rs changelog/
git commit -m "feat: shared app state and the poll loop (M4.04)"
```

---

### Task 5: `mcp`: the server handler, the tool context, errors and `list_catalogs`

The rmcp side. `OkfServer` is the `ServerHandler`; each group of tools lives in its own module with its own `#[tool_router]` block, joined in `OkfServer::tools` (verified fact 5), so Tasks 7 to 10 add a module and one line each. A tool takes the request's `Parts` (verified fact 4), builds a `ToolContext` (the principal, the connection's catalogs, one view of the catalog map), and calls a plain function that tests call directly. Domain errors become `isError: true` results with a JSON body `{"error": kind, "message": text, ...}`; database errors are logged and reported only as "internal error". This task adds the first tool, `list_catalogs`.

**Files:**
- Modify: `Cargo.toml` (add `rmcp`, `schemars`)
- Modify: `src/error.rs` (`kind`, `is_internal`, `to_json`)
- Create: `src/mcp/mod.rs`, `src/mcp/context.rs`, `src/mcp/catalogs.rs`, `src/mcp/testing.rs`
- Modify: `src/lib.rs` (`pub mod mcp;`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Tools")
- Create: `changelog/<ts>-m4-task-05-mcp.md`

- [ ] **Step 1: Dependencies**

In `Cargo.toml` `[dependencies]`:

```toml
# MCP: the server handler, `#[tool]` macros, and the streamable HTTP service.
rmcp = { version = "3.8.0", default-features = false, features = ["server", "macros", "transport-streamable-http-server"] }
# JSON schemas of tool parameters and results (rmcp builds on schemars 1).
schemars = "1.2.3"
```

- [ ] **Step 2: Errors as JSON (test first)**

Add to `src/error.rs` a tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_json_names_the_kind_and_what_to_act_on() {
        let not_found = OkfError::NotFound {
            path: "/x.md".to_string(),
        };
        let unselected = OkfError::CatalogNotSelected {
            catalog: "work".to_string(),
        };
        let refused = OkfError::ImportRefused {
            problems: vec!["a: bad".to_string()],
        };

        assert_eq!(
            not_found.to_json(),
            serde_json::json!({"error": "NotFound", "message": "/x.md: no such concept or document", "path": "/x.md"})
        );
        assert_eq!(unselected.to_json()["catalog"], "work");
        assert_eq!(refused.to_json()["problems"], serde_json::json!(["a: bad"]));
    }

    #[test]
    fn internal_errors_hide_their_details() {
        let error = OkfError::Database {
            source: sqlx::Error::PoolTimedOut,
        };

        let body = error.to_json();

        assert!(error.is_internal());
        assert_eq!(body["error"], "Database");
        assert!(!body["message"].as_str().unwrap().contains("pool"));
    }
}
```

and implement, after the enum:

```rust
impl OkfError {
    /// The variant's name, as clients see it in `error`.
    ///
    /// # Returns
    /// E.g. `"NotFound"`.
    pub fn kind(&self) -> &'static str {
        match self {
            OkfError::NotFound { .. } => "NotFound",
            OkfError::InvalidPath { .. } => "InvalidPath",
            OkfError::FrontmatterParse { .. } => "FrontmatterParse",
            OkfError::Shape { .. } => "Shape",
            OkfError::Io { .. } => "Io",
            OkfError::ImportRefused { .. } => "ImportRefused",
            OkfError::Database { .. } => "Database",
            OkfError::Migrate { .. } => "Migrate",
            OkfError::Config { .. } => "Config",
            OkfError::CatalogNotSelected { .. } => "CatalogNotSelected",
            OkfError::Refused { .. } => "Refused",
            OkfError::Forbidden { .. } => "Forbidden",
        }
    }

    /// Is this the server's fault, with details for the log only?
    ///
    /// # Returns
    /// `true` for database and migration failures.
    pub fn is_internal(&self) -> bool {
        matches!(self, OkfError::Database { .. } | OkfError::Migrate { .. })
    }

    /// The error as clients see it: `{"error": kind, "message": text}`, plus
    /// the path, catalog or problem list it concerns.
    ///
    /// # Returns
    /// The JSON body. An internal error's message says only that the server
    /// log has the details.
    pub fn to_json(&self) -> serde_json::Value {
        let message = if self.is_internal() {
            "internal error; the server log has the details".to_string()
        } else {
            self.to_string()
        };
        let mut body = serde_json::json!({ "error": self.kind(), "message": message });
        match self {
            OkfError::NotFound { path }
            | OkfError::InvalidPath { path, .. }
            | OkfError::FrontmatterParse { path, .. }
            | OkfError::Shape { path, .. }
            | OkfError::Io { path, .. } => body["path"] = path.as_str().into(),
            OkfError::CatalogNotSelected { catalog } => body["catalog"] = catalog.as_str().into(),
            OkfError::ImportRefused { problems } => body["problems"] = problems.clone().into(),
            OkfError::Database { .. }
            | OkfError::Migrate { .. }
            | OkfError::Config { .. }
            | OkfError::Refused { .. }
            | OkfError::Forbidden { .. } => {}
        }
        body
    }
}
```

Run: `cargo test --lib -- error`
Expected: 2 passed.

- [ ] **Step 3: Test support**

Create `src/mcp/testing.rs`:

```rust
//! Test support for the tools: an [`App`] over a throwaway database holding
//! catalogs, and principals and contexts to call tools with.

use std::sync::Arc;

use chrono::Utc;

use crate::app::App;
use crate::archive::Files;
use crate::auth::{Principal, Reach};
use crate::clock::SystemClock;
use crate::config::Config;
use crate::db::pg_support::TestDb;
use crate::mcp::context::ToolContext;
use crate::okf::workspace::StoredFile;
use crate::store::catalogs;
use crate::store::import::{import, ImportOptions};
use crate::store::keys::Access;

/// An app and the database under it. Fields drop in order: the app (and its
/// pool) before the container.
pub(crate) struct TestApp {
    pub(crate) app: Arc<App>,
    _db: TestDb,
}

/// Literal files, as an import takes them.
pub(crate) fn files(entries: &[(&str, &str)]) -> Files {
    entries
        .iter()
        .map(|(path, text)| {
            let file = StoredFile {
                content: text.as_bytes().to_vec(),
                executable: false,
            };
            (path.to_string(), file)
        })
        .collect()
}

/// Creates each catalog, imports its files, and loads the app.
pub(crate) async fn app_with(catalogs_and_files: &[(&str, &[(&str, &str)])], config: Config) -> TestApp {
    let db = crate::db::test_db().await;
    for (name, entries) in catalogs_and_files {
        catalogs::create(&db.pool, name, "", Utc::now()).await.unwrap();
        let options = ImportOptions {
            replace: false,
            actor: "tester",
            now: Utc::now(),
        };
        import(&db.pool, name, files(entries), &options).await.unwrap();
    }
    let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
        .await
        .unwrap();
    TestApp { app, _db: db }
}

/// A `read` key reaching `reach` (every catalog when empty), with a default.
pub(crate) fn principal(reach: &[&str], default: Option<&str>) -> Arc<Principal> {
    Arc::new(Principal {
        key_id: "tester".to_string(),
        access: Access::Read,
        reach: if reach.is_empty() {
            Reach::All
        } else {
            Reach::Catalogs(reach.iter().map(|name| name.to_string()).collect())
        },
        default_catalog: default.map(str::to_string),
        actor: "tester".to_string(),
    })
}

/// A tool context for `principal` on a connection naming `connection`.
pub(crate) fn context(test: &TestApp, principal: Arc<Principal>, connection: Option<&[&str]>) -> ToolContext {
    let connection = connection.map(|names| names.iter().map(|name| name.to_string()).collect());
    ToolContext::new(Arc::clone(&test.app), principal, connection)
}
```

- [ ] **Step 4: The tool context**

Create `src/mcp/context.rs`:

```rust
//! What one tool call acts as and on: the principal, the connection's
//! catalogs, and one view of the catalog map.

use std::sync::Arc;

use axum::http::request::Parts;

use crate::app::{App, Catalogs};
use crate::auth::Principal;
use crate::catalog::CatalogState;
use crate::error::OkfError;
use crate::selection::{connection_catalogs, resolve, Selection, Sources};

/// One tool call's context.
pub struct ToolContext {
    app: Arc<App>,
    principal: Arc<Principal>,
    connection: Option<Vec<String>>,
    // Taken once per call, so the names `select` checks are the states the
    // tool then reads, even if the poll loop swaps the map meanwhile.
    catalogs: Arc<Catalogs>,
}

impl ToolContext {
    /// A context for `principal` on a connection naming `connection`.
    pub fn new(app: Arc<App>, principal: Arc<Principal>, connection: Option<Vec<String>>) -> Self {
        let catalogs = app.catalogs();
        Self {
            app,
            principal,
            connection,
            catalogs,
        }
    }

    /// The context of the HTTP request rmcp handed the tool.
    ///
    /// # Errors
    /// [`OkfError::Forbidden`] if the request carries no principal, which
    /// only a router without the auth layer would allow.
    pub fn from_parts(app: &Arc<App>, parts: &Parts) -> Result<Self, OkfError> {
        let principal = parts
            .extensions
            .get::<Arc<Principal>>()
            .cloned()
            .ok_or_else(|| OkfError::Forbidden {
                reason: "the request carries no API key".to_string(),
            })?;
        Ok(Self::new(Arc::clone(app), principal, connection_catalogs(parts)))
    }

    /// The shared state.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The calling key.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// The catalog map this call sees.
    pub fn catalogs(&self) -> &Catalogs {
        &self.catalogs
    }

    /// Resolves the call's catalogs (design "Catalog selection").
    ///
    /// # Arguments
    /// * `argument` - the call's `catalogs` (or `catalog` as one name).
    ///
    /// # Errors
    /// As [`resolve`].
    pub fn select(&self, argument: Option<&[String]>) -> Result<Selection, OkfError> {
        let sources = Sources {
            argument,
            connection: self.connection.as_deref(),
            server_default: self.app.config().default_catalog.as_deref(),
        };
        resolve(&self.principal, sources, |name| self.catalogs.contains_key(name))
    }

    /// The states of the selected catalogs, in order.
    ///
    /// # Errors
    /// As [`ToolContext::select`].
    pub fn states(&self, argument: Option<&[String]>) -> Result<Vec<Arc<CatalogState>>, OkfError> {
        let selection = self.select(argument)?;
        Ok(selection
            .catalogs()
            .iter()
            .map(|name| {
                let state = self.catalogs.get(name).expect("select checked this map");
                Arc::clone(state)
            })
            .collect())
    }

    /// The read set of a tool with a single `catalog` argument: that catalog,
    /// else the selected catalogs (Decision 1).
    ///
    /// # Errors
    /// As [`ToolContext::select`].
    pub fn read_set(&self, catalog: Option<&str>) -> Result<Vec<Arc<CatalogState>>, OkfError> {
        let argument = catalog.map(|name| vec![name.to_string()]);
        self.states(argument.as_deref())
    }

    /// The one catalog a report runs on: `catalog`, else the first selected.
    ///
    /// # Errors
    /// As [`ToolContext::select`].
    pub fn one(&self, catalog: Option<&str>) -> Result<Arc<CatalogState>, OkfError> {
        // `states` never returns an empty list.
        Ok(self.read_set(catalog)?.swap_remove(0))
    }
}
```

- [ ] **Step 5: Write the failing `list_catalogs` tests**

Create `src/mcp/catalogs.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_reachable_catalogs_and_the_connection_selection() {
        let test = app_with(
            &[("pg", &[("a.md", "# A\n"), ("b.md", "# B\n")]), ("work", &[]), ("home", &[])],
            Config::default(),
        )
        .await;
        let key = principal(&["pg", "work"], None);

        let listed = list(&context(&test, key, Some(&["work", "pg"])));

        let names: Vec<&str> = listed.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["pg", "work"]);
        assert_eq!(listed.catalogs[0].files, 2);
        assert!(listed.catalogs[0].last_change.is_some());
        assert_eq!(listed.selection, ["work", "pg"]);
        assert_eq!(listed.write_target.as_deref(), Some("work"));
        assert_eq!(listed.selection_error, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_any_source_the_selection_says_why() {
        let test = app_with(&[("pg", &[])], Config::default()).await;

        let listed = list(&context(&test, principal(&[], None), None));

        assert!(listed.selection.is_empty());
        assert_eq!(listed.write_target, None);
        assert!(listed.selection_error.unwrap().contains("no catalog selected"));
    }

    #[test]
    fn the_server_lists_the_tool() {
        let names: Vec<String> = OkfServer::tools()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();

        assert!(names.contains(&"list_catalogs".to_string()));
    }
}
```

- [ ] **Step 6: Implement `catalogs.rs`**

Above the tests:

```rust
//! `list_catalogs`: the catalogs a key can reach and this connection's
//! selection.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Serialize;

use rmcp::Json;

use crate::clock::format_timestamp;
use crate::mcp::context::ToolContext;
use crate::mcp::{OkfServer, ToolResult};

/// One reachable catalog.
// `JsonSchema` gives the tool its output schema; doc comments become the
// fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CatalogInfo {
    /// The catalog's name.
    pub name: String,
    /// Its description.
    pub description: String,
    /// How many files it holds.
    pub files: usize,
    /// Its newest change (`2026-11-02T21:00:00Z`), or null when empty.
    pub last_change: Option<String>,
}

/// `list_catalogs`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CatalogList {
    /// Every catalog this key can reach, by name.
    pub catalogs: Vec<CatalogInfo>,
    /// The catalogs a call without a `catalog` argument reads, in order.
    pub selection: Vec<String>,
    /// Where writes go by default: the first of `selection`.
    pub write_target: Option<String>,
    /// Why there is no selection, when there is none.
    pub selection_error: Option<String>,
}

/// Lists the catalogs `ctx`'s key reaches and the connection's selection.
///
/// # Arguments
/// * `ctx` - the call's context.
///
/// # Returns
/// The list; a selection that fails is reported in `selection_error`
/// rather than failing the call, since this is the tool to diagnose it with.
pub(crate) fn list(ctx: &ToolContext) -> CatalogList {
    let catalogs = ctx
        .catalogs()
        .iter()
        .filter(|(name, _)| ctx.principal().reaches(name))
        .map(|(name, state)| {
            let loaded = state.current();
            CatalogInfo {
                name: name.clone(),
                description: state.description().to_string(),
                files: loaded.files(),
                last_change: loaded.last_change().map(format_timestamp),
            }
        })
        .collect();
    let (selection, write_target, selection_error) = match ctx.select(None) {
        Ok(selection) => (
            selection.catalogs().to_vec(),
            Some(selection.write_target().to_string()),
            None,
        ),
        Err(error) => (Vec::new(), None, Some(error.to_string())),
    };
    CatalogList {
        catalogs,
        selection,
        write_target,
        selection_error,
    }
}

// `#[tool_router]` turns the `#[tool]` methods of this block into a router
// named `catalogs_router`, which `OkfServer::tools` joins with the others.
#[tool_router(router = catalogs_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `list_catalogs` tool.
    #[tool(
        description = "List the catalogs this API key can reach (name, description, file \
                       count, last change) and this connection's selection: the catalogs \
                       a call without a `catalog` argument reads, and the default write target."
    )]
    async fn list_catalogs(&self, Extension(parts): Extension<Parts>) -> ToolResult<CatalogList> {
        let ctx = self.context(&parts)?;
        Ok(Json(list(&ctx)))
    }
}
```

- [ ] **Step 7: The handler**

Create `src/mcp/mod.rs`:

```rust
//! The MCP server (design "Tools"): the rmcp handler and the read tools,
//! one module per group. Each `#[tool]` method builds a
//! [`context::ToolContext`] from the HTTP request rmcp hands it and calls a
//! plain function, which the module's tests call directly.

mod catalogs;
// `pub` (not `pub(crate)`): the context's helpers are used by later tool
// modules, and a public item is never flagged as dead code meanwhile.
pub mod context;
#[cfg(test)]
pub(crate) mod testing;

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool_handler, Json, ServerHandler};

use crate::app::App;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;

/// What the server tells a client it is for.
pub const INSTRUCTIONS: &str = "Open Knowledge Format catalogs: concepts (markdown with YAML \
    frontmatter) about systems, projects, people and decisions. Start with `search`, then \
    `get_concept`. Paths look like /infra/time-server.md (the leading slash and .md are \
    optional). Without a `catalog` argument, tools use this connection's catalogs \
    (`list_catalogs` shows them).";

/// The MCP handler: one per session (rmcp's factory makes it).
pub struct OkfServer {
    app: Arc<App>,
    tool_router: ToolRouter<OkfServer>,
}

impl OkfServer {
    /// A handler over the shared state.
    ///
    /// # Arguments
    /// * `app` - the shared state.
    ///
    /// # Returns
    /// The handler.
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            tool_router: Self::tools(),
        }
    }

    /// Every tool: each module's router, joined.
    pub fn tools() -> ToolRouter<Self> {
        Self::catalogs_router()
    }
}

// `#[tool_handler]` writes `ServerHandler`'s `list_tools` and `call_tool`
// over the stored router; `get_info` is ours.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for OkfServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

/// What every tool returns: a structured result, or an `isError` result.
pub(crate) type ToolResult<T> = Result<Json<T>, CallToolResult>;

impl OkfServer {
    /// The context of the request rmcp handed a tool. The error is already a
    /// tool result, so a tool writes `let ctx = self.context(&parts)?;`.
    pub(crate) fn context(&self, parts: &Parts) -> Result<ToolContext, CallToolResult> {
        ToolContext::from_parts(&self.app, parts).map_err(|error| tool_error(&error))
    }
}

/// An error as a tool result: `isError: true`, its JSON in the content.
/// Internal errors are logged here, with their details.
pub(crate) fn tool_error(error: &OkfError) -> CallToolResult {
    if error.is_internal() {
        tracing::error!(%error, "tool failed");
    }
    CallToolResult::structured_error(error.to_json())
}

/// A tool function's outcome as a tool result.
pub(crate) fn reply<T>(result: Result<T, OkfError>) -> ToolResult<T> {
    result.map(Json).map_err(|error| tool_error(&error))
}

/// Runs CPU work (an analysis, a search) on a blocking thread, so the async
/// threads stay free; a panic in it is re-raised here.
pub async fn blocking<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}
```

Add `pub mod mcp;` to `src/lib.rs` and the crate doc.

- [ ] **Step 8: Run the tests**

Run: `cargo test --lib -- mcp:: error`
Expected: 5 passed.

- [ ] **Step 9: Update the design**

In "Tools", after the first paragraph, add: "Each result's `structuredContent` is the JSON, and its one text block holds the same JSON (rmcp's `Json<T>`), rather than a hand-written summary. Errors are results with `isError: true` and a body `{"error": <variant>, "message": <text>}` plus the `path`, `catalog` or `problems` they concern; a database error says only that the log has the details. Without a `catalog` argument, `get_concept`, `read_document` and `history` look through the selected catalogs in order and answer from the first that has the path, naming it; the reports (`open_questions`, `oldest`, `sprawl`, `links_report`, `lint`) run on the first selected catalog." In the `list_catalogs` row, make the result: "Catalogs the key can reach (name, description, file count, last change); this connection's selection and default write target, or why there is none".

- [ ] **Step 10: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-05-mcp.md`. Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/error.rs src/mcp/ src/lib.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: MCP handler, tool context, error results and list_catalogs (M4.05)"
```

---

### Task 6: `http` and `okf-mcp serve`

The router (design "Process and routes"): `/healthz` without a key; `/mcp` (rmcp's service, with the configured `allowed_hosts`, the session idle timeout, and a cancellation token) behind the key layer; request tracing on everything and a timeout on everything but `/mcp`. `okf-mcp serve` loads the app, starts the poll loop, listens, and on SIGINT or SIGTERM cancels one token that ends rmcp's sessions, the poll loop and the server. The end-to-end test harness (`tests/support/server.rs`) starts all of it on a free port over a throwaway Postgres, with real rmcp clients.

**Files:**
- Modify: `Cargo.toml` (`tower-http`; tokio `net`, `signal`; dev `rmcp` client, `reqwest`)
- Create: `src/http/mod.rs`
- Modify: `src/lib.rs` (`pub mod http;`)
- Modify: `src/cli.rs` (`Serve(ServeArgs)`; `run` takes `Arc<dyn Clock>`)
- Modify: `src/main.rs`
- Create: `tests/support/server.rs`; modify `tests/support/mod.rs`
- Create: `tests/server.rs`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Process and routes")
- Create: `changelog/<ts>-m4-task-06-http-serve.md`

- [ ] **Step 1: Dependencies**

`[dependencies]`:

```toml
# HTTP middleware: request tracing and timeouts.
tower-http = { version = "0.6.11", features = ["trace", "timeout"] }
```

and add `"net"` (the listener) and `"signal"` (SIGINT, SIGTERM) to tokio's features. `[dev-dependencies]`:

```toml
# The end-to-end tests talk to the server as a real MCP client would.
rmcp = { version = "3.8.0", features = ["client", "transport-streamable-http-client-reqwest"] }
# Plain HTTP requests in tests (401s, /healthz, the archive routes);
# `json` for `Response::json`.
reqwest = { version = "0.13.5", default-features = false, features = ["json"] }
```

- [ ] **Step 2: The router and `serve`**

Create `src/http/mod.rs`:

```rust
//! The HTTP server (design "Process and routes"): `/healthz`, `/mcp`, and
//! the archive routes (`archives`, Task 11), with request tracing, timeouts
//! and the key layer.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::app::App;
use crate::auth::require_key;
use crate::error::OkfError;
use crate::mcp::OkfServer;

/// How long a request on any route but `/mcp` may take. `/mcp`'s `GET` is a
/// long-lived event stream, which rmcp's keep-alive and the session idle
/// timeout bound instead.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// An [`OkfError`] as an HTTP response: a status and the error's JSON.
// A newtype (a one-field tuple struct) so this crate can implement axum's
// `IntoResponse` for it.
#[derive(Debug)]
pub struct AppError(pub OkfError);

// `From` lets `?` turn an `OkfError` into an `AppError` in handlers.
impl From<OkfError> for AppError {
    fn from(error: OkfError) -> Self {
        Self(error)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            OkfError::NotFound { .. } | OkfError::CatalogNotSelected { .. } => StatusCode::NOT_FOUND,
            OkfError::Forbidden { .. } => StatusCode::FORBIDDEN,
            OkfError::Refused { .. } => StatusCode::CONFLICT,
            OkfError::InvalidPath { .. }
            | OkfError::FrontmatterParse { .. }
            | OkfError::Shape { .. }
            | OkfError::ImportRefused { .. } => StatusCode::BAD_REQUEST,
            OkfError::Io { .. }
            | OkfError::Database { .. }
            | OkfError::Migrate { .. }
            | OkfError::Config { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if self.0.is_internal() {
            tracing::error!(error = %self.0, "request failed");
        }
        (status, Json(self.0.to_json())).into_response()
    }
}

/// `/healthz`'s body.
#[derive(Debug, Serialize)]
struct Health {
    /// `"ok"` or `"unreachable"`.
    database: &'static str,
    /// How many catalogs are loaded (no names: the route needs no key).
    catalogs: usize,
}

/// `GET /healthz`: 200 while the database answers, else 503.
async fn healthz(State(app): State<Arc<App>>) -> Response {
    let reachable = sqlx::query("SELECT 1").execute(app.pool()).await.is_ok();
    let health = Health {
        database: if reachable { "ok" } else { "unreachable" },
        catalogs: app.catalogs().len(),
    };
    let status = if reachable {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(health)).into_response()
}

/// rmcp's streamable HTTP service over the app: one [`OkfServer`] per
/// session, the configured `Host` names, sessions closed after
/// `session_idle_timeout`, and every session ended when `shutdown` is.
fn mcp_service(
    app: &Arc<App>,
    shutdown: &CancellationToken,
) -> StreamableHttpService<OkfServer, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(app.config().session_idle_timeout);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(app.config().allowed_hosts.iter().cloned())
        .with_cancellation_token(shutdown.child_token());
    let app = Arc::clone(app);
    // The factory runs once per session; `move` gives it its own `Arc`.
    StreamableHttpService::new(
        move || Ok(OkfServer::new(Arc::clone(&app))),
        Arc::new(sessions),
        config,
    )
}

/// The whole router.
///
/// # Arguments
/// * `app` - the shared state.
/// * `shutdown` - cancelling it ends every MCP session.
///
/// # Returns
/// The router, ready for [`axum::serve`].
pub fn router(app: Arc<App>, shutdown: &CancellationToken) -> Router {
    let keys = Arc::clone(app.keys());
    // `route_layer` applies to the routes added before it, so every keyed
    // route goes above it (Task 11 merges the archive routes here).
    let keyed = Router::new()
        .route_service("/mcp", mcp_service(&app, shutdown))
        .route_layer(middleware::from_fn_with_state(keys, require_key));
    let health = Router::new()
        .route("/healthz", get(healthz))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT));
    Router::new()
        .merge(health)
        .merge(keyed)
        .layer(TraceLayer::new_for_http())
        // Hands `app` to every handler that asks for `State<Arc<App>>`.
        .with_state(app)
}

/// Serves until `shutdown` is cancelled, running the poll loop beside it.
///
/// # Arguments
/// * `app` - the shared state.
/// * `listener` - a bound socket.
/// * `shutdown` - cancelled to stop.
///
/// # Errors
/// The listener's I/O error.
pub async fn serve(
    app: Arc<App>,
    listener: TcpListener,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let router = router(Arc::clone(&app), &shutdown);
    let poller = tokio::spawn(Arc::clone(&app).poll_until(shutdown.clone()));
    let stop = shutdown.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { stop.cancelled().await })
        .await?;
    // The loop returns once `shutdown` is cancelled; a panic in it is logged.
    if let Err(error) = poller.await {
        tracing::error!(%error, "the poll loop panicked");
    }
    Ok(())
}
```

Add `pub mod http;` to `src/lib.rs` and the crate doc.

- [ ] **Step 3: `serve` on the command line**

In `src/cli.rs`:

1. Replace the `Serve` variant with `Serve(ServeArgs)` and add:

```rust
/// `okf-mcp serve`'s options.
#[derive(Debug, Default, Args)]
pub struct ServeArgs {
    /// The address to listen on, overriding the config's `listen`.
    #[arg(long, env = "OKF_MCP_LISTEN")]
    pub listen: Option<std::net::SocketAddr>,
}
```

2. Change `run`'s signature to `pub async fn run(cli: Cli, clock: Arc<dyn Clock>) -> anyhow::Result<()>` (the server keeps the clock for as long as it runs). Code that passed `clock` on as `&dyn Clock` now passes `clock.as_ref()`; `clock.now()` works unchanged through the `Arc`. In the tests, replace each `&FixedClock(..)` argument with `Arc::new(FixedClock(..))`.
3. Replace `cli.command.unwrap_or(Command::Serve)` with `cli.command.unwrap_or_else(|| Command::Serve(ServeArgs::default()))`, and the `Command::Serve` arm with:

```rust
        Command::Serve(args) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            // `mut`: the flag may override the file.
            let mut config = config;
            if let Some(listen) = args.listen {
                config.listen = listen;
            }
            let listen = config.listen;
            let app = App::load(pool, config, clock)
                .await
                .context("load the catalogs")?;
            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("listen on {listen}"))?;
            tracing::info!(%listen, "serving");
            let shutdown = CancellationToken::new();
            tokio::spawn(cancel_on_signal(shutdown.clone()));
            http::serve(app, listener, shutdown)
                .await
                .context("serve")?;
        }
```

4. Add:

```rust
/// Cancels `shutdown` on SIGINT (Ctrl-C) or SIGTERM (what Kubernetes sends).
async fn cancel_on_signal(shutdown: CancellationToken) {
    let interrupt = tokio::signal::ctrl_c();
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                // Wait forever: Ctrl-C still works.
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
    shutdown.cancel();
}
```

(`tokio::signal::unix` exists only on Unix, the only platform this server runs on.)

5. Remove the test asserting "milestone 4" and add:

```rust
    #[test]
    fn serve_is_the_default_and_takes_listen() {
        let default = Cli::try_parse_from(["okf-mcp"]).unwrap();
        let flagged = Cli::try_parse_from(["okf-mcp", "serve", "--listen", "127.0.0.1:9000"]).unwrap();

        assert!(default.command.is_none());
        assert!(matches!(
            flagged.command,
            Some(Command::Serve(ServeArgs { listen: Some(addr) })) if addr.port() == 9000
        ));
    }
```

6. Update the module doc ("`serve`, the default, runs the MCP server").

In `src/main.rs`, pass `std::sync::Arc::new(SystemClock)` to `run`, and default the log filter to `"warn,okf_mcp=info"` (the server logs `serving`, catalog loads and shutdown at `info`).

- [ ] **Step 4: The end-to-end harness**

Create `tests/support/server.rs`:

```rust
//! An okf-mcp server for end-to-end tests: a throwaway Postgres, catalogs
//! imported from literal files, the real router on a free port, and rmcp
//! clients that talk to it over HTTP.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use okf_mcp::app::App;
use okf_mcp::archive::Files;
use okf_mcp::clock::SystemClock;
use okf_mcp::config::Config;
use okf_mcp::okf::workspace::StoredFile;
use okf_mcp::store::catalogs;
use okf_mcp::store::import::{import, ImportOptions};
use okf_mcp::store::keys::{self, Access, Grants, NewKey};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use axum::http::{HeaderName, HeaderValue};
use rmcp::{RoleClient, ServiceExt};
use tokio_util::sync::CancellationToken;

use super::pg::TestDb;

/// A connected MCP client.
pub type Client = RunningService<RoleClient, ()>;

/// Literal files, as an import takes them.
pub fn files(entries: &[(&str, &str)]) -> Files {
    entries
        .iter()
        .map(|(path, text)| {
            let file = StoredFile {
                content: text.as_bytes().to_vec(),
                executable: false,
            };
            (path.to_string(), file)
        })
        .collect()
}

/// A running server. Dropping it stops the server, then the database.
pub struct TestServer {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    /// The server's state, to `poll` after changing the database.
    pub app: Arc<App>,
    shutdown: CancellationToken,
    /// The database, for changes made "by the subcommands".
    pub db: TestDb,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl TestServer {
    /// Creates and imports each catalog, then starts the server.
    pub async fn start(catalogs_and_files: Vec<(&str, Files)>, config: Config) -> Self {
        let db = TestDb::start().await;
        okf_mcp::db::migrate(&db.pool).await.expect("migrations");
        for (name, files) in catalogs_and_files {
            catalogs::create(&db.pool, name, "", Utc::now())
                .await
                .expect("create catalog");
            let options = ImportOptions {
                replace: false,
                actor: "tester",
                now: Utc::now(),
            };
            import(&db.pool, name, files, &options)
                .await
                .expect("import");
        }
        let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
            .await
            .expect("app loads");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let shutdown = CancellationToken::new();
        tokio::spawn(okf_mcp::http::serve(Arc::clone(&app), listener, shutdown.clone()));
        Self {
            base,
            app,
            shutdown,
            db,
        }
    }

    /// The MCP endpoint.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base)
    }

    /// Creates a key (`catalogs` empty: every catalog) and reloads the cache.
    pub async fn key(&self, id: &str, access: Access, catalogs: &[&str], default: Option<&str>) -> String {
        let names: Vec<String> = catalogs.iter().map(|name| name.to_string()).collect();
        let grants = if names.is_empty() {
            Grants::All
        } else {
            Grants::Catalogs(&names)
        };
        let new = NewKey {
            id,
            access,
            grants,
            default_catalog: default,
            actor: id,
        };
        let key = keys::create(&self.db.pool, &new, Utc::now())
            .await
            .expect("create key");
        self.app.poll().await.expect("poll");
        key
    }

    /// An MCP client with `key`, on a connection naming `catalogs` (the
    /// `X-OKF-Catalogs` header), if any.
    pub async fn client(&self, key: &str, catalogs: Option<&str>) -> Result<Client, String> {
        let mut headers = HashMap::new();
        if let Some(list) = catalogs {
            headers.insert(
                HeaderName::from_static("x-okf-catalogs"),
                HeaderValue::from_str(list).expect("header value"),
            );
        }
        let config = StreamableHttpClientTransportConfig::with_uri(self.mcp_url())
            .auth_header(key)
            .custom_headers(headers);
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|error| error.to_string())
    }
}

/// Calls a tool; `arguments` must be a JSON object.
pub async fn call(client: &Client, tool: &'static str, arguments: serde_json::Value) -> CallToolResult {
    let arguments = arguments.as_object().expect("arguments object").clone();
    client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .expect("tool call")
}

/// A tool's structured result, asserting it is not an error.
pub fn structured(result: &CallToolResult) -> &serde_json::Value {
    assert_ne!(result.is_error, Some(true), "tool failed: {result:?}");
    result.structured_content.as_ref().expect("structured content")
}
```

Add `pub mod server;` to `tests/support/mod.rs`.

- [ ] **Step 5: Write the end-to-end tests**

Create `tests/server.rs`:

```rust
//! End-to-end tests of the HTTP server (design "Testing"): keys, hosts,
//! `/healthz` and catalog selection, over real HTTP with rmcp's client.

mod support;

use okf_mcp::config::Config;
use okf_mcp::store::keys::{self, Access};
use support::server::{call, files, structured, Client, TestServer};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

async fn post_initialize(server: &TestServer, key: Option<&str>, host: Option<&str>) -> reqwest::StatusCode {
    let mut request = reqwest::Client::new()
        .post(server.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    if let Some(host) = host {
        request = request.header("host", host);
    }
    request.send().await.unwrap().status()
}

/// The connection's selection, as `list_catalogs` reports it.
async fn selection(client: Client) -> serde_json::Value {
    let result = call(&client, "list_catalogs", serde_json::json!({})).await;
    structured(&result)["selection"].clone()
}

fn pg() -> Vec<(&'static str, okf_mcp::archive::Files)> {
    vec![("pg", files(&[("a.md", "---\ntype: Note\n---\n# A\n")]))]
}

#[tokio::test(flavor = "multi_thread")]
async fn healthz_answers_without_a_key() {
    let server = TestServer::start(pg(), Config::default()).await;

    let response = reqwest::get(format!("{}/healthz", server.base)).await.unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"database": "ok", "catalogs": 1}));
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_refuses_a_missing_or_unknown_key() {
    let server = TestServer::start(pg(), Config::default()).await;
    let key = server.key("reader", Access::Read, &[], None).await;

    assert_eq!(post_initialize(&server, None, None).await, 401);
    assert_eq!(post_initialize(&server, Some("not-a-key"), None).await, 401);
    assert_eq!(post_initialize(&server, Some(&key), None).await, 200);
    assert!(server.client("not-a-key", None).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_allowed_hosts_is_refused() {
    let server = TestServer::start(pg(), Config::default()).await;
    let key = server.key("reader", Access::Read, &[], None).await;

    assert_eq!(post_initialize(&server, Some(&key), Some("evil.example")).await, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_key_stops_working_after_a_poll() {
    let server = TestServer::start(pg(), Config::default()).await;
    let key = server.key("reader", Access::Read, &[], Some("pg")).await;
    let client = server.client(&key, None).await.unwrap();
    structured(&call(&client, "list_catalogs", serde_json::json!({})).await);

    keys::revoke(&server.db.pool, "reader", chrono::Utc::now()).await.unwrap();
    server.app.poll().await.unwrap();

    let arguments = serde_json::json!({}).as_object().unwrap().clone();
    // Bounded: if the client retried a 401 instead of failing, the test
    // must fail rather than hang.
    let call = client
        .call_tool(rmcp::model::CallToolRequestParams::new("list_catalogs").with_arguments(arguments));
    let after = tokio::time::timeout(std::time::Duration::from_secs(10), call)
        .await
        .expect("the call answers within 10 s");
    assert!(after.is_err(), "a revoked key still worked: {after:?}");
    // The server's side, independent of how the client reports it.
    assert_eq!(post_initialize(&server, Some(&key), None).await, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_takes_the_header_then_the_key_default_then_the_server_default() {
    let catalogs = vec![
        ("pg", files(&[])),
        ("work", files(&[])),
        ("home", files(&[])),
    ];
    let config = Config {
        default_catalog: Some("home".to_string()),
        ..Config::default()
    };
    let server = TestServer::start(catalogs, config).await;
    let with_default = server.key("a", Access::Read, &[], Some("work")).await;
    let without_default = server.key("b", Access::Read, &[], None).await;
    let only_pg = server.key("c", Access::Read, &["pg"], Some("pg")).await;

    assert_eq!(selection(server.client(&with_default, Some("pg,work")).await.unwrap()).await, serde_json::json!(["pg", "work"]));
    assert_eq!(selection(server.client(&with_default, None).await.unwrap()).await, serde_json::json!(["work"]));
    assert_eq!(selection(server.client(&without_default, None).await.unwrap()).await, serde_json::json!(["home"]));
    let client = server.client(&only_pg, None).await.unwrap();
    let listed = call(&client, "list_catalogs", serde_json::json!({})).await;
    assert_eq!(structured(&listed)["catalogs"].as_array().unwrap().len(), 1);
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --test server`
Expected: 5 passed. Then the whole suite: `cargo test`.

- [ ] **Step 7: Run it by hand**

With a test Postgres (`docker run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=postgres postgres:17-alpine`): `DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5433/postgres cargo run -- catalog create pg`, `... cargo run -- key create me --access read --all --default pg`, `... cargo run -- serve --listen 127.0.0.1:8080` in the background, then `curl -s localhost:8080/healthz` (expect `{"database":"ok","catalogs":1}`) and `curl -s -o /dev/null -w '%{http_code}' -X POST localhost:8080/mcp` (expect `401`). Stop it with Ctrl-C (or `kill -TERM <pid>` for the PID you started) and check it exits within a second. Stop the container you started by its id.

- [ ] **Step 8: Update the design**

In "Process and routes": `/healthz` reports `{"database": "ok"|"unreachable", "catalogs": <count>}` (no names: it needs no key), 200 or 503; the clock offset joins it in M7. Add: "`tower-http` traces every request; every route but `/mcp` has a 120-second timeout (`/mcp`'s `GET` is a long-lived event stream). SIGINT or SIGTERM cancels one token that ends rmcp's sessions, the poll loop and the listener." In the `serve` row: "Runs migrations, loads snapshots, starts the poll loop, serves HTTP (`--listen` overrides `listen`)".

- [ ] **Step 9: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-06-http-serve.md`, including what Step 7 printed. Run every gate. Commit:

```bash
git add Cargo.toml Cargo.lock src/http/ src/cli.rs src/main.rs src/lib.rs tests/support/ tests/server.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: HTTP router, /healthz and okf-mcp serve (M4.06)"
```

---

### Task 7: `search` across catalogs, and the `tags` wording (#34)

`search` runs M2's `okf::search::search` on each selected catalog (its `catalogs` argument, else the connection's) and merges the hits: best rank first, then catalog order, then path, at most `limit` (design "Read tools"; the cross-catalog merge deferred from M2). Its descriptions say what #34 asked: the `tags` filter matches whole tags only, while free text in `query` matches tags by substring. This task closes #34.

**Files:**
- Modify: `src/okf/search.rs` (`search_catalogs`, `CatalogHit`, `JsonSchema` on `SearchHit`)
- Create: `src/mcp/search.rs`
- Modify: `src/mcp/mod.rs` (`mod search;`, `+ Self::search_router()`)
- Create: `changelog/<ts>-m4-task-07-search.md`

- [ ] **Step 1: Write the failing merge test**

In the tests module of `src/okf/search.rs`:

```rust
    #[test]
    fn search_catalogs_merges_by_rank_then_catalog_order_then_path() {
        // "time" is in work's title (tier 0) but only in pg's body (tier 3).
        let pg = Snapshot::build(&MemWorkspace::from_files([
            ("b.md", "---\ntype: Note\ntitle: Beta\n---\nAbout time.\n"),
            ("a.md", "---\ntype: Note\ntitle: Alpha\n---\nAbout time.\n"),
        ]))
        .unwrap();
        let work = Snapshot::build(&MemWorkspace::from_files([(
            "z.md",
            "---\ntype: Note\ntitle: Time\n---\nText.\n",
        )]))
        .unwrap();
        let mut query = SearchQuery::new("time");

        let all = search_catalogs(&[("pg", &pg), ("work", &work)], &query);
        query.limit = 2;
        let two = search_catalogs(&[("pg", &pg), ("work", &work)], &query);

        let order = |hits: &[CatalogHit]| -> Vec<(String, String)> {
            hits.iter()
                .map(|hit| (hit.catalog.clone(), hit.hit.path.clone()))
                .collect()
        };
        assert_eq!(
            order(&all),
            [
                ("work".to_string(), "/z.md".to_string()),
                ("pg".to_string(), "/a.md".to_string()),
                ("pg".to_string(), "/b.md".to_string()),
            ]
        );
        assert_eq!(order(&two).len(), 2);
        let json = serde_json::to_value(&all[0]).unwrap();
        assert_eq!(json["catalog"], "work");
        assert_eq!(json["path"], "/z.md");
    }
```

Run: `cargo test --lib -- search_catalogs`
Expected: compile error (`search_catalogs` not found).

- [ ] **Step 2: Implement the merge**

In `src/okf/search.rs`, add `JsonSchema` to `SearchHit`'s derives (`#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]`; the skipped `rank` needs no schema), and after `search`:

```rust
/// One hit of a search across catalogs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct CatalogHit {
    /// The catalog holding the concept.
    pub catalog: String,
    /// The hit; `flatten` writes its fields beside `catalog`.
    #[serde(flatten)]
    pub hit: SearchHit,
}

/// Searches several catalogs and merges the hits: best [`SearchRank`] first,
/// then catalog order, then path.
///
/// # Arguments
/// * `catalogs` - `(name, snapshot)` pairs, in the selection's order.
/// * `query` - text, filters and limit.
///
/// # Returns
/// At most `query.limit` hits.
pub fn search_catalogs(catalogs: &[(&str, &Snapshot)], query: &SearchQuery) -> Vec<CatalogHit> {
    // Each search already runs across its documents with rayon and returns
    // at most `limit` hits, so the merge sorts at most catalogs x limit.
    let mut ranked: Vec<(usize, SearchHit)> = catalogs
        .iter()
        .enumerate()
        .flat_map(|(order, (_, snapshot))| {
            search(snapshot, query).into_iter().map(move |hit| (order, hit))
        })
        .collect();
    ranked.sort_by(|(left_order, left), (right_order, right)| {
        left.rank
            .cmp(&right.rank)
            .then(left_order.cmp(right_order))
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked.truncate(query.limit);
    ranked
        .into_iter()
        .map(|(order, hit)| CatalogHit {
            catalog: catalogs[order].0.to_string(),
            hit,
        })
        .collect()
}
```

Run: `cargo test --lib -- search`
Expected: every search test passes, the new one included.

- [ ] **Step 3: Write the failing tool tests**

Create `src/mcp/search.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    const TAGGED: &str = "---\ntype: Service\ntitle: Router\ntags: [infrastructure]\n---\nRoutes.\n";

    fn params(query: &str, tags: &[&str]) -> SearchParams {
        SearchParams {
            query: query.to_string(),
            catalogs: None,
            concept_type: None,
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            status: None,
            limit: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tags_match_whole_tags_while_free_text_matches_substrings() {
        let test = app_with(&[("pg", &[("router.md", TAGGED)])], Config::default()).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let by_tag = run(&ctx, params("", &["infra"])).await.unwrap();
        let by_whole_tag = run(&ctx, params("", &["infrastructure"])).await.unwrap();
        let by_text = run(&ctx, params("infra", &[])).await.unwrap();

        assert!(by_tag.hits.is_empty());
        assert_eq!(by_whole_tag.hits.len(), 1);
        assert_eq!(by_text.hits.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_spans_the_selected_catalogs_and_names_each_hit_s_catalog() {
        let test = app_with(
            &[("pg", &[("router.md", TAGGED)]), ("work", &[("switch.md", TAGGED)])],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], None), Some(&["work", "pg"]));

        let found = run(&ctx, params("routes", &[])).await.unwrap();

        let catalogs: Vec<&str> = found.hits.iter().map(|hit| hit.catalog.as_str()).collect();
        assert_eq!(catalogs, ["work", "pg"]);
    }

    #[test]
    fn the_schema_says_tags_match_whole_tags_only() {
        let tools = OkfServer::search_router().list_all();
        let tool = tools.iter().find(|tool| tool.name == "search").unwrap();

        let description = tool.description.as_deref().unwrap_or_default();
        let schema = serde_json::to_string(&tool.input_schema).unwrap();

        assert!(description.contains("whole tags"), "{description}");
        assert!(schema.contains("whole tags"), "{schema}");
        assert!(schema.contains("substring"), "{schema}");
    }
}
```

- [ ] **Step 4: Implement the tool**

Above the tests:

```rust
//! `search`: concepts across the selected catalogs.

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::Loaded;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{blocking, reply, OkfServer, ToolResult};
use crate::okf::search::{search_catalogs, CatalogHit, SearchQuery};
use crate::okf::snapshot::Snapshot;

/// The design's default `limit`.
const DEFAULT_LIMIT: usize = 20;

/// `search`'s arguments. Doc comments are the parameters' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Free text. Every whitespace-separated term must occur
    /// (case-insensitive substring) in a concept's title or aliases, tags,
    /// description or body; free text matches tags by substring. Empty
    /// matches every concept that passes the filters.
    pub query: String,
    /// The catalogs to search (default: this connection's catalogs).
    pub catalogs: Option<Vec<String>>,
    /// Only concepts of this `type` (case-insensitive).
    #[serde(rename = "type")]
    pub concept_type: Option<String>,
    /// Only concepts carrying every one of these tags. These match whole
    /// tags only (case-insensitive): `infra` does not match
    /// `infrastructure`, while the same text in `query` would, by substring.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Only concepts with this `status` (case-insensitive; a missing status
    /// counts as `stable`).
    pub status: Option<String>,
    /// At most this many hits (default 20).
    pub limit: Option<usize>,
}

/// `search`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SearchResult {
    /// Best first: concepts holding the whole query in a better field
    /// first, deprecated concepts last, then catalog order, then path.
    pub hits: Vec<CatalogHit>,
}

/// Runs a search.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the arguments.
///
/// # Returns
/// The merged hits.
///
/// # Errors
/// A selection error ([`ToolContext::states`]).
pub(crate) async fn run(ctx: &ToolContext, params: SearchParams) -> Result<SearchResult, OkfError> {
    let states = ctx.states(params.catalogs.as_deref())?;
    let query = SearchQuery {
        text: params.query,
        concept_type: params.concept_type,
        tags: params.tags,
        status: params.status,
        limit: params.limit.unwrap_or(DEFAULT_LIMIT),
    };
    // Owned handles, so the blocking thread can keep them.
    let loaded: Vec<(String, Arc<Loaded>)> = states
        .iter()
        .map(|state| (state.name().to_string(), state.current()))
        .collect();
    let hits = blocking(move || {
        let catalogs: Vec<(&str, &Snapshot)> = loaded
            .iter()
            .map(|(name, loaded)| (name.as_str(), loaded.snapshot()))
            .collect();
        search_catalogs(&catalogs, &query)
    })
    .await;
    Ok(SearchResult { hits })
}

#[tool_router(router = search_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `search` tool.
    #[tool(
        description = "Search concepts across this connection's catalogs (or `catalogs`). \
                       Every term of `query` must occur, as a case-insensitive substring, in \
                       a concept's title or aliases, tags, description or body; free text \
                       matches tags by substring. The `tags` filter matches whole tags only. \
                       Returns catalog, path, title, description and a snippet, best first."
    )]
    async fn search(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SearchParams>,
    ) -> ToolResult<SearchResult> {
        let ctx = self.context(&parts)?;
        reply(run(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, add `mod search;` and make `tools()` return `Self::catalogs_router() + Self::search_router()`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- mcp::search search_catalogs`
Expected: 4 passed.

- [ ] **Step 6: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-07-search.md` (closes #34). Run every gate. Commit:

```bash
git add src/okf/search.rs src/mcp/ changelog/
git commit -m "feat: search across catalogs; say tags match whole tags only (M4.07, #34)"
```

---

### Task 8: `get_concept` and `read_document`

`get_concept` returns a concept's frontmatter as JSON and its body; with `links`, its resolved outbound links (target, title, description, broken) and inbound backlinks; with `raw`, the file verbatim. `read_document` returns the raw text of an `index.md`, `log.md` or the root `AGENTS.md`. Without `catalog`, both look through the selected catalogs in order (Decision 1). `Doc` gains `body`.

**Files:**
- Modify: `src/okf/doc.rs` (`Doc::body`)
- Create: `src/mcp/documents.rs`
- Modify: `src/mcp/mod.rs` (`mod documents;`, `+ Self::documents_router()`)
- Create: `changelog/<ts>-m4-task-08-documents.md`

- [ ] **Step 1: `Doc::body`, test first**

In `src/okf/doc.rs`'s tests:

```rust
    #[test]
    fn body_is_the_text_after_the_frontmatter() {
        let with = Doc::from_text("a.md", "---\ntype: Note\n---\n# A\r\nText\n");
        let without = Doc::from_text("b.md", "# B\n");

        assert_eq!(with.body(), "# A\r\nText\n");
        assert_eq!(without.body(), "# B\n");
    }
```

and after `render`:

```rust
    /// The text after the frontmatter (the whole text without one), with
    /// its line endings as written.
    pub fn body(&self) -> String {
        // `from_text` keeps the closing `---` as the tail's first line, so a
        // document with frontmatter skips it (`usize::from(true)` is 1).
        self.tail
            .iter()
            .skip(usize::from(self.has_frontmatter))
            .map(String::as_str)
            .collect()
    }
```

Run: `cargo test --lib -- body_is`
Expected: 1 passed.

- [ ] **Step 2: Write the failing tool tests**

Create `src/mcp/documents.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: The hub.\n---\n\
                       See [leaf](leaf.md) and [gone](gone.md).\n";
    const LEAF: &str = "---\ntype: Note\ntitle: Leaf\ndescription: A leaf.\n---\nBack to [hub](hub.md).\n";

    fn concept_params(path: &str) -> GetConceptParams {
        GetConceptParams {
            path: path.to_string(),
            catalog: None,
            links: false,
            raw: false,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_returns_frontmatter_body_links_and_raw() {
        let test = app_with(
            &[("pg", &[("hub.md", HUB), ("leaf.md", LEAF), ("index.md", "# Index\n")])],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        let mut params = concept_params("hub");
        params.links = true;
        params.raw = true;

        let concept = get_concept(&ctx, params).unwrap();

        assert_eq!(concept.catalog, "pg");
        assert_eq!(concept.path, "/hub.md");
        assert_eq!(concept.frontmatter["title"], "Hub");
        assert!(concept.body.starts_with("See [leaf]"));
        assert_eq!(concept.raw.as_deref(), Some(HUB));
        let links = concept.links.unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!((links[0].target.as_str(), links[0].broken), ("/leaf.md", false));
        assert_eq!(links[0].description.as_deref(), Some("A leaf."));
        assert_eq!((links[1].target.as_str(), links[1].broken), ("/gone.md", true));
        let backlinks = concept.backlinks.unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].path, "/leaf.md");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_looks_through_the_read_set_in_order() {
        let test = app_with(
            &[("pg", &[("only-pg.md", LEAF)]), ("work", &[("hub.md", HUB)])],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], None), Some(&["pg", "work"]));

        let found = get_concept(&ctx, concept_params("/hub.md")).unwrap();
        let missing = get_concept(&ctx, concept_params("nowhere")).unwrap_err();
        let mut pinned = concept_params("hub");
        pinned.catalog = Some("pg".to_string());
        let not_in_pg = get_concept(&ctx, pinned).unwrap_err();

        assert_eq!(found.catalog, "work");
        assert!(found.links.is_none() && found.raw.is_none());
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(not_in_pg, OkfError::NotFound { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_document_reads_indexes_logs_and_agents_only() {
        let test = app_with(
            &[(
                "pg",
                &[
                    ("index.md", "# Index\n"),
                    ("infra/index.md", "# Infra\n"),
                    ("log.md", "# Log\n"),
                    ("AGENTS.md", "# Agents\n"),
                    ("hub.md", HUB),
                ],
            )],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        let read = |path: Option<&str>| {
            read_document(
                &ctx,
                ReadDocumentParams {
                    path: path.map(str::to_string),
                    catalog: None,
                },
            )
        };

        assert_eq!(read(None).unwrap().text, "# Index\n");
        assert_eq!(read(Some("/infra/index.md")).unwrap().text, "# Infra\n");
        assert_eq!(read(Some("log.md")).unwrap().text, "# Log\n");
        assert_eq!(read(Some("/AGENTS.md")).unwrap().text, "# Agents\n");
        assert!(matches!(read(Some("/hub.md")), Err(OkfError::InvalidPath { .. })));
        assert!(matches!(read(Some("/../index.md")), Err(OkfError::InvalidPath { .. })));
        assert!(matches!(read(Some("/nope/index.md")), Err(OkfError::NotFound { .. })));
    }
}
```

- [ ] **Step 3: Implement**

Above the tests:

```rust
//! `get_concept` and `read_document`: one document, from the first selected
//! catalog that has it (Decision 1).

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer, ToolResult};
use crate::okf::paths::{basename, escapes_root, normpath};
use crate::okf::snapshot::{Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;

/// `get_concept`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct GetConceptParams {
    /// The concept: `/infra/time-server.md`, `infra/time-server` or
    /// `/infra/time-server`.
    pub path: String,
    /// The catalog (default: the first selected catalog that has it).
    pub catalog: Option<String>,
    /// Also return its resolved outbound links and its backlinks.
    #[serde(default)]
    pub links: bool,
    /// Also return the file verbatim.
    #[serde(default)]
    pub raw: bool,
}

/// A link out of the concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OutboundLink {
    /// 1-based line of the link.
    pub line: usize,
    /// `/`-prefixed target path.
    pub target: String,
    /// The text after `#`, if any.
    pub fragment: Option<String>,
    /// The target's title, if it is a document.
    pub title: Option<String>,
    /// The target's description, if it has one.
    pub description: Option<String>,
    /// The target does not exist.
    pub broken: bool,
}

/// A link into the concept from another document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InboundLink {
    /// `/`-prefixed path of the linking document.
    pub path: String,
    /// 1-based line of the link in it.
    pub line: usize,
    /// The linking document's title.
    pub title: Option<String>,
}

/// `get_concept`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Concept {
    /// The catalog it came from.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// Its title.
    pub title: Option<String>,
    /// The frontmatter as JSON, keys in file order; null when absent or invalid.
    pub frontmatter: serde_json::Value,
    /// Why the frontmatter does not parse, when it does not.
    pub frontmatter_error: Option<String>,
    /// The text after the frontmatter.
    pub body: String,
    /// The file verbatim (with `raw`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Outbound links, in-page links left out (with `links`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<OutboundLink>>,
    /// Backlinks (with `links`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlinks: Option<Vec<InboundLink>>,
}

/// `read_document`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReadDocumentParams {
    /// An `index.md` (any folder's), `log.md` or `AGENTS.md`; default
    /// `/index.md`. Use `get_concept` for concepts.
    pub path: Option<String>,
    /// The catalog (default: the first selected catalog that has it).
    pub catalog: Option<String>,
}

/// `read_document`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Document {
    /// The catalog it came from.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// The file verbatim.
    pub text: String,
}

/// A frontmatter string value.
fn text_field(doc: &SnapshotDoc, key: &str) -> Option<String> {
    let data = doc.frontmatter().data()?;
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The frontmatter as JSON, and the parse error if any.
fn frontmatter_json(doc: &SnapshotDoc) -> (serde_json::Value, Option<String>) {
    match doc.frontmatter() {
        Frontmatter::Parsed(data) => {
            let object = data
                .iter()
                .map(|(key, value)| (key.clone(), value.to_json()))
                .collect();
            (serde_json::Value::Object(object), None)
        }
        Frontmatter::Absent => (serde_json::Value::Null, None),
        Frontmatter::Invalid(error) => (serde_json::Value::Null, Some(error.to_string())),
    }
}

/// Builds the result for one resolved concept.
fn concept(catalog: &str, snapshot: &Snapshot, doc: &SnapshotDoc, params: &GetConceptParams) -> Concept {
    let (frontmatter, frontmatter_error) = frontmatter_json(doc);
    let links = params.links.then(|| {
        doc.links()
            .iter()
            .filter(|link| !link.in_page)
            .map(|link| {
                let target = snapshot.get(&link.target);
                OutboundLink {
                    line: link.line,
                    target: format!("/{}", link.target),
                    fragment: link.fragment.clone(),
                    title: target.and_then(|t| t.title().map(str::to_string)),
                    description: target.and_then(|t| text_field(t, "description")),
                    broken: !link.target_exists,
                }
            })
            .collect()
    });
    let backlinks = params.links.then(|| {
        snapshot
            .backlinks(doc.path())
            .iter()
            .map(|backlink| InboundLink {
                path: format!("/{}", backlink.source),
                line: backlink.line,
                title: snapshot
                    .get(&backlink.source)
                    .and_then(|source| source.title().map(str::to_string)),
            })
            .collect()
    });
    Concept {
        catalog: catalog.to_string(),
        path: format!("/{}", doc.path()),
        title: doc.title().map(str::to_string),
        frontmatter,
        frontmatter_error,
        body: doc.doc().body(),
        raw: params.raw.then(|| doc.raw()),
        links,
        backlinks,
    }
}

/// Finds a concept in the read set.
///
/// # Errors
/// A selection error; [`OkfError::NotFound`] if no selected catalog has it;
/// [`OkfError::InvalidPath`] if the first catalog holding the path holds a
/// document that is not a concept there.
pub(crate) fn get_concept(ctx: &ToolContext, params: GetConceptParams) -> Result<Concept, OkfError> {
    let mut refused = None;
    for state in ctx.read_set(params.catalog.as_deref())? {
        let loaded = state.current();
        let snapshot = loaded.snapshot();
        match snapshot.resolve_concept(&params.path) {
            Ok(path) => {
                let doc = snapshot.get(&path).expect("resolve_concept found it");
                return Ok(concept(state.name(), snapshot, doc, &params));
            }
            Err(OkfError::NotFound { .. }) => {}
            // Keep looking, but report this if nothing better turns up.
            Err(error) => {
                refused.get_or_insert(error);
            }
        }
    }
    Err(refused.unwrap_or(OkfError::NotFound { path: params.path }))
}

/// The documents `read_document` serves.
fn readable(path: &str) -> bool {
    matches!(basename(path), "index.md" | "log.md") || path == "AGENTS.md"
}

/// Reads an index, the log or `AGENTS.md` from the read set.
///
/// # Errors
/// A selection error; [`OkfError::InvalidPath`] for any other path or one
/// outside the catalog; [`OkfError::NotFound`] if no selected catalog has it.
pub(crate) fn read_document(ctx: &ToolContext, params: ReadDocumentParams) -> Result<Document, OkfError> {
    let asked = params.path.unwrap_or_else(|| "/index.md".to_string());
    let path = normpath(asked.trim_start_matches('/'));
    if escapes_root(&path) || !readable(&path) {
        return Err(OkfError::InvalidPath {
            path: asked,
            reason: "read_document reads index.md, log.md and AGENTS.md; use get_concept for concepts"
                .to_string(),
        });
    }
    for state in ctx.read_set(params.catalog.as_deref())? {
        if let Some(doc) = state.current().snapshot().get(&path) {
            return Ok(Document {
                catalog: state.name().to_string(),
                path: format!("/{path}"),
                text: doc.raw(),
            });
        }
    }
    Err(OkfError::NotFound { path: asked })
}

#[tool_router(router = documents_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `get_concept` tool.
    #[tool(
        description = "Get a concept: its frontmatter as JSON and its body. With `links`, its \
                       resolved outbound links (target, title, description, broken) and its \
                       backlinks; with `raw`, the file verbatim. Without `catalog`, the first \
                       selected catalog that has it answers."
    )]
    async fn get_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<GetConceptParams>,
    ) -> ToolResult<Concept> {
        let ctx = self.context(&parts)?;
        reply(get_concept(&ctx, params))
    }

    /// The `read_document` tool.
    #[tool(
        description = "Read the raw text of an index.md (default /index.md), log.md or \
                       AGENTS.md. Without `catalog`, the first selected catalog that has it \
                       answers."
    )]
    async fn read_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReadDocumentParams>,
    ) -> ToolResult<Document> {
        let ctx = self.context(&parts)?;
        reply(read_document(&ctx, params))
    }
}
```

Inside a method, a bare `get_concept(..)` is the module's free function: Rust reaches methods only through `self.` or `Self::`, so the shared names do not clash.

In `src/mcp/mod.rs`, add `mod documents;` and `+ Self::documents_router()` to `tools()`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib -- mcp::documents body_is`
Expected: 4 passed.

- [ ] **Step 5: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-08-documents.md`. Run every gate. Commit:

```bash
git add src/okf/doc.rs src/mcp/ changelog/
git commit -m "feat: get_concept and read_document (M4.08)"
```

---

### Task 9: `history`

The revisions of one document, newest first: revision, operation, actor, message and time (design "Read tools"; the server's `git log`). A document deleted since still has a history. Without `catalog`, the first selected catalog with any revision of the path answers (Decision 1). Paths are spelled as for `get_concept`; `log.md`, indexes and other files work too.

**Files:**
- Create: `src/store/history.rs`; modify `src/store/mod.rs` (`pub mod history;`)
- Create: `src/mcp/history.rs`
- Modify: `src/mcp/mod.rs` (`mod history;`, `+ Self::history_router()`)
- Create: `changelog/<ts>-m4-task-09-history.md`

- [ ] **Step 1: Write the failing store test**

Create `src/store/history.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::db::test_db;
    use crate::okf::workspace::StoredFile;
    use crate::store::catalogs;
    use crate::store::import::{import, ImportOptions};
    use chrono::Utc;

    fn files(entries: &[(&str, &str)]) -> Files {
        entries
            .iter()
            .map(|(path, text)| {
                let file = StoredFile {
                    content: text.as_bytes().to_vec(),
                    executable: false,
                };
                (path.to_string(), file)
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revisions_come_newest_first_and_outlive_a_delete() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", Utc::now()).await.unwrap();
        for (version, replace) in [("one", false), ("two", true)] {
            let options = ImportOptions {
                replace,
                actor: "tester",
                now: Utc::now(),
            };
            let text = format!("# {version}\n");
            import(&db.pool, "pg", files(&[("a.md", &text), ("b.md", "# B\n")]), &options)
                .await
                .unwrap();
        }
        let options = ImportOptions {
            replace: true,
            actor: "tester",
            now: Utc::now(),
        };
        import(&db.pool, "pg", files(&[("b.md", "# B\n")]), &options).await.unwrap();

        let all = revisions(&db.pool, catalog.id, "a.md", 20).await.unwrap();
        let one = revisions(&db.pool, catalog.id, "a.md", 1).await.unwrap();
        let none = revisions(&db.pool, catalog.id, "c.md", 20).await.unwrap();

        let numbers: Vec<i32> = all.iter().map(|row| row.revision).collect();
        assert_eq!(numbers, [3, 2, 1]);
        // Import writes `import` for every added or changed file (M3,
        // `src/store/import.rs`) and `delete` for a file it drops.
        assert_eq!(all[0].op, "delete");
        assert_eq!(all[1].op, "import");
        assert_eq!(all[0].message, "import");
        assert_eq!(all[2].actor, "tester");
        assert_eq!(one.len(), 1);
        assert!(none.is_empty());
    }
}
```

- [ ] **Step 2: Implement the query**

Above the tests:

```rust
//! A document's history: its rows in `revisions`, newest first.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// One revision of a document (its content left out).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RevisionRow {
    /// The revision number, 1 for the first.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// Who made it.
    pub actor: String,
    /// The publish message (`import` for an import).
    pub message: String,
    /// When (server clock).
    pub created_at: DateTime<Utc>,
}

/// The newest `limit` revisions of one path.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `path` - catalog-relative path, as stored.
/// * `limit` - at most this many rows.
///
/// # Returns
/// The rows, newest first; empty if the path never existed.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revisions(
    pool: &PgPool,
    catalog_id: i64,
    path: &str,
    limit: i64,
) -> Result<Vec<RevisionRow>, OkfError> {
    // The primary key (catalog_id, path, revision), read backwards, serves
    // both the filter and the order.
    let rows = sqlx::query_as::<_, RevisionRow>(
        "SELECT revision, op, actor, message, created_at FROM revisions \
         WHERE catalog_id = $1 AND path = $2 ORDER BY revision DESC LIMIT $3",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
```

Add `pub mod history;` to `src/store/mod.rs`.

Run: `cargo test --lib -- store::history`
Expected: 1 passed.

- [ ] **Step 3: Write the failing tool test**

Create `src/mcp/history.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    fn params(path: &str) -> HistoryParams {
        HistoryParams {
            path: path.to_string(),
            catalog: None,
            limit: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn history_finds_the_path_in_the_read_set_and_spells_paths_like_get_concept() {
        let test = app_with(
            &[("pg", &[("a.md", "# A\n")]), ("work", &[("infra/b.md", "# B\n"), ("log.md", "# Log\n")])],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], None), Some(&["pg", "work"]));

        let found = history(&ctx, params("infra/b")).await.unwrap();
        let log = history(&ctx, params("/log.md")).await.unwrap();
        let missing = history(&ctx, params("nowhere")).await.unwrap_err();
        let zero = history(&ctx, HistoryParams { limit: Some(0), ..params("infra/b") }).await.unwrap();

        assert_eq!((found.catalog.as_str(), found.path.as_str()), ("work", "/infra/b.md"));
        assert_eq!(found.revisions.len(), 1);
        assert_eq!(found.revisions[0].op, "import");
        assert_eq!(log.path, "/log.md");
        assert_eq!(zero.revisions.len(), 1);
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }
}
```

- [ ] **Step 4: Implement the tool**

Above the tests:

```rust
//! `history`: a document's revisions, from the first selected catalog that
//! has any (Decision 1).

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer, ToolResult};
use crate::okf::paths::{escapes_root, normpath};
use crate::store::history::revisions;

/// The design's default `limit`.
const DEFAULT_LIMIT: i64 = 20;

/// `history`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct HistoryParams {
    /// The document: `/infra/time-server.md`, `infra/time-server`, `/log.md`.
    pub path: String,
    /// The catalog (default: the first selected catalog with any revision of it).
    pub catalog: Option<String>,
    /// At most this many revisions (default 20; 0 counts as 1).
    pub limit: Option<u32>,
}

/// One revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Revision {
    /// The revision number, 1 for the first.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// Who made it.
    pub actor: String,
    /// The publish message.
    pub message: String,
    /// When (`2026-11-02T21:00:00Z`).
    pub at: String,
}

/// `history`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct History {
    /// The catalog it came from.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// Newest first.
    pub revisions: Vec<Revision>,
}

/// The stored paths a reference may mean: as written, then with `.md`.
fn candidates(reference: &str) -> Vec<String> {
    let bare = reference.trim_start_matches('/');
    [normpath(bare), normpath(&format!("{bare}.md"))]
        .into_iter()
        .filter(|path| !escapes_root(path))
        .collect()
}

/// A document's revisions.
///
/// # Errors
/// A selection error; [`OkfError::NotFound`] if no selected catalog has any
/// revision of the path; [`OkfError::Database`].
pub(crate) async fn history(ctx: &ToolContext, params: HistoryParams) -> Result<History, OkfError> {
    // At least 1: with 0, every path would look like it has no history.
    let limit = params.limit.map_or(DEFAULT_LIMIT, |n| i64::from(n.max(1)));
    let paths = candidates(&params.path);
    for state in ctx.read_set(params.catalog.as_deref())? {
        for path in &paths {
            let rows = revisions(ctx.app().pool(), state.id(), path, limit).await?;
            if rows.is_empty() {
                continue;
            }
            let revisions = rows
                .into_iter()
                .map(|row| Revision {
                    revision: row.revision,
                    op: row.op,
                    actor: row.actor,
                    message: row.message,
                    at: format_timestamp(row.created_at),
                })
                .collect();
            return Ok(History {
                catalog: state.name().to_string(),
                path: format!("/{path}"),
                revisions,
            });
        }
    }
    Err(OkfError::NotFound { path: params.path })
}

#[tool_router(router = history_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `history` tool.
    #[tool(
        description = "A document's revisions, newest first: revision, op (create, update, \
                       delete, import), actor, message and time. Deleted documents keep their \
                       history. Without `catalog`, the first selected catalog with any \
                       revision of the path answers."
    )]
    async fn history(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<HistoryParams>,
    ) -> ToolResult<History> {
        let ctx = self.context(&parts)?;
        reply(history(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, add `mod history;` and `+ Self::history_router()`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib -- history`
Expected: 2 passed.

- [ ] **Step 6: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-09-history.md`. Run every gate. Commit:

```bash
git add src/store/ src/mcp/ changelog/
git commit -m "feat: history of a document from its revisions (M4.09)"
```

---

### Task 10: the reports: `open_questions`, `oldest`, `sprawl`, `links_report`, `lint`

Five tools over M2's analyses, each on one catalog (`catalog`, else the first selected; Decision 1), each on a blocking thread. `oldest`, `sprawl`, `links_report` and `lint` return `okf.py`'s JSON (`lint` on the published catalog; `pending` arrives with changesets in M5), with the catalog's name added and lists wrapped in an object. `open_questions` numbers each concept's queued questions from 1, reusing `oldest`'s collection of them. The M2 report types gain `JsonSchema`.

**Files:**
- Modify: `src/okf/analysis/mod.rs`, `lint.rs`, `links.rs`, `oldest.rs`, `sprawl.rs` (derive `schemars::JsonSchema` on every `Serialize` report type)
- Create: `src/mcp/reports.rs`
- Modify: `src/mcp/mod.rs` (`mod reports;`, `+ Self::reports_router()`)
- Create: `changelog/<ts>-m4-task-10-reports.md`

- [ ] **Step 1: Schemas for the report types**

Add `schemars::JsonSchema` to the derives of `Finding` (`analysis/mod.rs`), `LintReport` (`lint.rs`), `LinkIssue`, `IndexDrift`, `MentionHit`, `UnlinkedMention`, `LinksSummary`, `LinksReport` (`links.rs`), `OldestRow` (`oldest.rs`; its skipped `fresh` needs none), `SprawlSection`, `VolatileItem`, `SprawlReport` (`sprawl.rs`). Nothing else changes; the parity tests still compare the same JSON.

- [ ] **Step 2: Write the failing tests**

Create `src/mcp/reports.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    const QUEUED: &str = "---\ntype: Note\ntitle: Q\nopen_questions:\n  - Is it up?\n  - Who owns it?\n---\nBody.\n";
    const PLAIN: &str = "---\ntype: Note\ntitle: P\n---\nSee [q](q.md) and [gone](gone.md).\n";

    async fn ctx_for(catalog: Option<&str>) -> (crate::mcp::testing::TestApp, Option<String>) {
        let test = app_with(
            &[("pg", &[("q.md", QUEUED), ("p.md", PLAIN)]), ("work", &[("w.md", PLAIN)])],
            Config::default(),
        )
        .await;
        (test, catalog.map(str::to_string))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_questions_are_numbered_from_one_per_concept() {
        let (test, _) = ctx_for(None).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let found = open_questions(&ctx, CatalogParam { catalog: None }).await.unwrap();

        assert_eq!(found.catalog, "pg");
        assert_eq!(found.concepts.len(), 1);
        assert_eq!(found.concepts[0].path, "/q.md");
        let numbered: Vec<(usize, &str)> = found.concepts[0]
            .questions
            .iter()
            .map(|q| (q.number, q.text.as_str()))
            .collect();
        assert_eq!(numbered, [(1, "Is it up?"), (2, "Who owns it?")]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_run_on_the_first_selected_catalog_unless_one_is_named() {
        let (test, _) = ctx_for(None).await;
        let ctx = context(&test, principal(&[], None), Some(&["work", "pg"]));

        let default = links(&ctx, LinksParams { catalog: None, mentions: None }).await.unwrap();
        let named = links(&ctx, LinksParams { catalog: Some("pg".to_string()), mentions: Some(false) })
            .await
            .unwrap();

        assert_eq!(default.catalog, "work");
        assert_eq!(named.catalog, "pg");
        let json = serde_json::to_value(&named).unwrap();
        assert_eq!(json["catalog"], "pg");
        assert_eq!(json["summary"]["broken"], 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oldest_sprawl_and_lint_answer_with_okf_py_s_shapes() {
        let (test, _) = ctx_for(None).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let rows = oldest_report(&ctx, OldestParams { catalog: None, n: Some(1), open_questions_only: false })
            .await
            .unwrap();
        let reports = sprawl_report(&ctx, SprawlParams { catalog: None, path: Some("q".to_string()), n: None })
            .await
            .unwrap();
        let findings = lint_report(&ctx, CatalogParam { catalog: None }).await.unwrap();

        assert_eq!(rows.rows.len(), 1);
        assert_eq!(reports.reports.len(), 1);
        assert_eq!(reports.reports[0].path, "/q.md");
        let json = serde_json::to_value(&findings).unwrap();
        assert_eq!(json["catalog"], "pg");
        assert!(json["errors"].is_array() && json["warnings"].is_array());
    }

    #[test]
    fn every_report_tool_is_listed_with_an_object_output_schema() {
        let tools = OkfServer::reports_router().list_all();
        // rmcp sorts `list_all` (its tests/test_tool_routers.rs asserts it);
        // sorting here keeps the test independent of that.
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();

        assert_eq!(names, ["links_report", "lint", "oldest", "open_questions", "sprawl"]);
        for tool in &tools {
            let schema = tool.output_schema.as_ref().expect("output schema");
            assert_eq!(schema.get("type"), Some(&serde_json::json!("object")), "{}", tool.name);
        }
    }
}
```

- [ ] **Step 3: Implement**

Above the tests:

```rust
//! The reports: `open_questions`, `oldest`, `sprawl`, `links_report` and
//! `lint`, each on one catalog (Decision 1) and on a blocking thread.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{blocking, reply, OkfServer, ToolResult};
use crate::okf::analysis::links::{links_report, LinksReport};
use crate::okf::analysis::lint::LintReport;
use crate::okf::analysis::oldest::{oldest, OldestQuery, OldestRow};
use crate::okf::analysis::sprawl::{sprawl, SprawlQuery, SprawlReport};

/// `okf.py`'s default `-n` for `oldest` and `sprawl`.
const DEFAULT_N: usize = 5;

/// A report's only argument.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct CatalogParam {
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
}

/// One queued question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Question {
    /// Its 1-based position (what `unquestion` takes).
    pub number: usize,
    /// The question.
    pub text: String,
}

/// A concept's queued questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ConceptQuestions {
    /// `/`-prefixed path.
    pub path: String,
    /// Its title.
    pub title: Option<String>,
    /// In file order.
    pub questions: Vec<Question>,
}

/// `open_questions`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OpenQuestions {
    /// The catalog.
    pub catalog: String,
    /// Concepts with queued questions, stalest first (as `oldest` orders them).
    pub concepts: Vec<ConceptQuestions>,
}

/// `oldest`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct OldestParams {
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
    /// How many concepts (default 5; ignored with `open_questions_only`).
    pub n: Option<usize>,
    /// Only concepts with queued `open_questions`, all of them.
    #[serde(default)]
    pub open_questions_only: bool,
}

/// `oldest`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Oldest {
    /// The catalog.
    pub catalog: String,
    /// `okf.py oldest --json`'s rows, stalest first.
    pub rows: Vec<OldestRow>,
}

/// `sprawl`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SprawlParams {
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
    /// One concept to score (default: the worst `n`).
    pub path: Option<String>,
    /// How many concepts (default 5).
    pub n: Option<usize>,
}

/// `sprawl`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Sprawl {
    /// The catalog.
    pub catalog: String,
    /// `okf.py sprawl --json`'s reports, worst first.
    pub reports: Vec<SprawlReport>,
}

/// `links_report`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct LinksParams {
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
    /// Also look for unlinked mentions of concept names (default true; the
    /// slowest part).
    pub mentions: Option<bool>,
}

/// `links_report`'s result: `okf.py links --json` plus the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Links {
    /// The catalog.
    pub catalog: String,
    /// The report; `flatten` writes its fields beside `catalog`.
    #[serde(flatten)]
    pub report: LinksReport,
}

/// `lint`'s result: `okf.py lint --json` plus the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Lint {
    /// The catalog.
    pub catalog: String,
    /// The findings; `flatten` writes `errors` and `warnings` beside `catalog`.
    #[serde(flatten)]
    pub report: LintReport,
}

/// Queued questions, numbered.
///
/// # Errors
/// A selection error.
pub(crate) async fn open_questions(ctx: &ToolContext, params: CatalogParam) -> Result<OpenQuestions, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let now = ctx.app().clock().now();
    let query = OldestQuery {
        n: 0,
        open_questions_only: true,
    };
    let rows = blocking(move || oldest(loaded.snapshot(), now, query)).await;
    let concepts = rows
        .into_iter()
        .map(|row| ConceptQuestions {
            path: row.path,
            title: row.title,
            questions: row
                .open_questions
                .into_iter()
                .enumerate()
                .map(|(index, text)| Question {
                    number: index + 1,
                    text,
                })
                .collect(),
        })
        .collect();
    Ok(OpenQuestions {
        catalog: state.name().to_string(),
        concepts,
    })
}

/// `okf.py oldest`.
///
/// # Errors
/// A selection error.
pub(crate) async fn oldest_report(ctx: &ToolContext, params: OldestParams) -> Result<Oldest, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let now = ctx.app().clock().now();
    let query = OldestQuery {
        n: params.n.unwrap_or(DEFAULT_N),
        open_questions_only: params.open_questions_only,
    };
    let rows = blocking(move || oldest(loaded.snapshot(), now, query)).await;
    Ok(Oldest {
        catalog: state.name().to_string(),
        rows,
    })
}

/// `okf.py sprawl`.
///
/// # Errors
/// A selection error; [`OkfError::NotFound`] for an unknown `path`.
pub(crate) async fn sprawl_report(ctx: &ToolContext, params: SprawlParams) -> Result<Sprawl, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let n = params.n.unwrap_or(DEFAULT_N);
    let path = params.path;
    let reports = blocking(move || {
        let query = SprawlQuery {
            concept: path.as_deref(),
            n,
        };
        sprawl(loaded.snapshot(), query)
    })
    .await?;
    Ok(Sprawl {
        catalog: state.name().to_string(),
        reports,
    })
}

/// `okf.py links`.
///
/// # Errors
/// A selection error.
pub(crate) async fn links(ctx: &ToolContext, params: LinksParams) -> Result<Links, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let mentions = params.mentions.unwrap_or(true);
    let report = blocking(move || links_report(loaded.snapshot(), mentions)).await;
    Ok(Links {
        catalog: state.name().to_string(),
        report,
    })
}

/// `okf.py lint`, on the published catalog with its commit times.
///
/// # Errors
/// A selection error.
pub(crate) async fn lint_report(ctx: &ToolContext, params: CatalogParam) -> Result<Lint, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let now = ctx.app().clock().now();
    let report = blocking(move || loaded.lint(now)).await;
    Ok(Lint {
        catalog: state.name().to_string(),
        report,
    })
}

#[tool_router(router = reports_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `open_questions` tool.
    #[tool(description = "Every queued open_questions entry in a catalog, grouped by concept \
                          and numbered from 1 (the number `unquestion` takes).")]
    async fn open_questions(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CatalogParam>,
    ) -> ToolResult<OpenQuestions> {
        let ctx = self.context(&parts)?;
        reply(open_questions(&ctx, params).await)
    }

    /// The `oldest` tool.
    #[tool(description = "The stalest concepts (okf.py oldest --json): last stamp, by whom, \
                          age in days, stale flag and queued questions.")]
    async fn oldest(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<OldestParams>,
    ) -> ToolResult<Oldest> {
        let ctx = self.context(&parts)?;
        reply(oldest_report(&ctx, params).await)
    }

    /// The `sprawl` tool.
    #[tool(description = "Sprawl scores (okf.py sprawl --json): which concepts have grown too \
                          long or volatile, with sections and split candidates.")]
    async fn sprawl(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SprawlParams>,
    ) -> ToolResult<Sprawl> {
        let ctx = self.context(&parts)?;
        reply(sprawl_report(&ctx, params).await)
    }

    /// The `links_report` tool.
    #[tool(description = "The link report (okf.py links --json): broken links and anchors, \
                          relative links, orphans, dead ends, index drift and (unless \
                          `mentions` is false) unlinked mentions.")]
    async fn links_report(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<LinksParams>,
    ) -> ToolResult<Links> {
        let ctx = self.context(&parts)?;
        reply(links(&ctx, params).await)
    }

    /// The `lint` tool.
    #[tool(description = "Lint the published catalog (okf.py lint --json): errors and warnings \
                          with file, line, rule and message.")]
    async fn lint(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CatalogParam>,
    ) -> ToolResult<Lint> {
        let ctx = self.context(&parts)?;
        reply(lint_report(&ctx, params).await)
    }
}
```

In `src/mcp/mod.rs`, add `mod reports;` and `+ Self::reports_router()`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib -- mcp::reports`
Expected: 4 passed. Then live parity, which must still pass 6/6 (the derives change no JSON).

- [ ] **Step 5: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-10-reports.md`. Run every gate. Commit:

```bash
git add src/okf/analysis/ src/mcp/ changelog/
git commit -m "feat: open_questions, oldest, sprawl, links_report and lint tools (M4.10)"
```

---

### Task 11: the archive routes: export and import over HTTP

`GET /catalogs/{name}/export` returns the catalog as a `.tar.gz` (M3's `store::export`) to a key with `read` on it. `POST /catalogs/{name}/import[?replace=true]` imports an uploaded `.tar.gz` (M3's `archive::read_tarball` and `store::import`) for a key with `admin` on it, as the key's actor, caps the body at `max_import_bytes` (413 beyond it), and swaps the new snapshot in before answering, so the next read sees it. A catalog that does not exist or that the key does not reach is 404; a reachable catalog the key's level does not allow is 403; a refused archive is 400 with every problem; importing into a non-empty catalog without `replace` is 409. Creating and deleting catalogs stays subcommand-only (design).

> **Corrected (issue #65), 2026-11-02.** Task 1 left the import route out of
> `Config::max_import_bytes`'s doc comment. When the route lands, add to it: "The import
> route caps the request body at this size plus a little slack for gzip (`body_limit`)."
> and list `src/config.rs` under this task's files.

**Files:**
- Create: `src/http/archives.rs`
- Modify: `src/http/mod.rs` (`mod archives;`, and `.merge(archives::routes(&app))` above `route_layer`)
- Modify: `tests/server.rs`
- Create: `changelog/<ts>-m4-task-11-archive-routes.md`

- [ ] **Step 1: Write the failing tests**

Append to `tests/server.rs`:

```rust
async fn get_export(server: &TestServer, catalog: &str, key: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}/catalogs/{catalog}/export", server.base))
        .bearer_auth(key)
        .send()
        .await
        .unwrap()
}

async fn post_import(server: &TestServer, catalog: &str, key: &str, query: &str, body: Vec<u8>) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/catalogs/{catalog}/import{query}", server.base))
        .bearer_auth(key)
        .header("content-type", "application/gzip")
        .body(body)
        .send()
        .await
        .unwrap()
}

fn two_catalogs() -> Vec<(&'static str, okf_mcp::archive::Files)> {
    vec![
        ("pg", files(&[("a.md", "---\ntype: Note\ntitle: A\n---\n# A\n"), ("index.md", "# Index\n")])),
        ("work", files(&[])),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn export_gives_a_reachable_catalog_as_a_tarball() {
    let server = TestServer::start(two_catalogs(), Config::default()).await;
    let reader = server.key("reader", Access::Read, &["pg"], None).await;

    let response = get_export(&server, "pg", &reader).await;
    let elsewhere = get_export(&server, "work", &reader).await.status();
    let unknown = get_export(&server, "nope", &reader).await.status();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/gzip");
    let bytes = response.bytes().await.unwrap();
    let exported = okf_mcp::archive::read_tarball(&bytes[..], 1 << 20).unwrap();
    assert_eq!(exported.keys().collect::<Vec<_>>(), ["a.md", "index.md"]);
    assert_eq!(elsewhere, 404);
    assert_eq!(unknown, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_needs_admin_refuses_a_full_catalog_without_replace_and_is_read_at_once() {
    let server = TestServer::start(two_catalogs(), Config::default()).await;
    let reader = server.key("reader", Access::Read, &[], None).await;
    let admin = server.key("admin", Access::Admin, &[], Some("work")).await;
    let tarball = get_export(&server, "pg", &admin).await.bytes().await.unwrap().to_vec();

    let as_reader = post_import(&server, "work", &reader, "", tarball.clone()).await.status();
    let first = post_import(&server, "work", &admin, "", tarball.clone()).await;
    assert_eq!(first.status(), 200);
    let report: serde_json::Value = first.json().await.unwrap();
    let again = post_import(&server, "work", &admin, "", tarball.clone()).await;
    assert_eq!(again.status(), 409);
    let conflict: serde_json::Value = again.json().await.unwrap();
    assert!(conflict["message"].as_str().unwrap().contains("?replace=true"));
    let replaced: serde_json::Value = post_import(&server, "work", &admin, "?replace=true", tarball)
        .await
        .json()
        .await
        .unwrap();
    let garbage = post_import(&server, "work", &admin, "", b"not a tarball".to_vec()).await;

    assert_eq!(as_reader, 403);
    assert_eq!(report["added"], 2);
    assert_eq!(replaced["unchanged"], 2);
    assert_eq!(garbage.status(), 400);
    let refusal: serde_json::Value = garbage.json().await.unwrap();
    assert_eq!(refusal["error"], "ImportRefused");
    // No poll in between: the route swapped the snapshot in itself.
    let client = server.client(&admin, None).await.unwrap();
    let concept = call(&client, "get_concept", serde_json::json!({"path": "a"})).await;
    assert_eq!(structured(&concept)["catalog"], "work");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_import_larger_than_max_import_bytes_is_refused() {
    let config = Config {
        max_import_bytes: bytesize::ByteSize::b(1024),
        ..Config::default()
    };
    let server = TestServer::start(two_catalogs(), config).await;
    let admin = server.key("admin", Access::Admin, &[], None).await;

    // Over the limit plus its 64 KiB of gzip slack.
    let status = post_import(&server, "work", &admin, "", vec![0u8; 128 * 1024]).await.status();

    assert_eq!(status, 413);
}
```

Run: `cargo test --test server`
Expected: the three new tests fail (404 for every archive route).

- [ ] **Step 2: Implement the routes**

Create `src/http/archives.rs`:

```rust
//! The archive routes (design "Process and routes"): a catalog's export as a
//! `.tar.gz`, and an import from one.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use tower_http::timeout::TimeoutLayer;

use crate::app::App;
use crate::archive::read_tarball;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::{AppError, REQUEST_TIMEOUT};
use crate::mcp::blocking;
use crate::store::export::export;
use crate::store::import::{import, ImportOptions, ImportReport};
use crate::store::keys::Access;

/// The two routes, for the keyed part of the router.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// A router over `Arc<App>` state.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    let limit = body_limit(app.config().max_import_bytes.as_u64());
    Router::new()
        .route("/catalogs/{name}/export", get(export_catalog))
        .route(
            "/catalogs/{name}/import",
            post(import_catalog).layer(DefaultBodyLimit::max(limit)),
        )
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT))
}

/// The request-body cap for an import whose decompressed tar stream may be
/// `max_import_bytes`. gzip makes incompressible input slightly larger (5
/// bytes per stored 64 KiB block, plus an 18-byte header and trailer), so the
/// cap adds 0.1% and 64 KiB: a tarball the CLI would accept is never refused
/// here with 413. `read_tarball` still enforces the exact limit on the
/// decompressed stream.
///
/// # Arguments
/// * `max_import_bytes` - the configured limit.
///
/// # Returns
/// The body cap, saturating at `usize::MAX`.
fn body_limit(max_import_bytes: u64) -> usize {
    let slack = max_import_bytes / 1000 + 64 * 1024;
    // A `u64` that does not fit a `usize` is larger than any body anyway.
    usize::try_from(max_import_bytes.saturating_add(slack)).unwrap_or(usize::MAX)
}

/// `GET /catalogs/{name}/export`: the published documents as a `.tar.gz`.
// Each argument is an extractor: axum fills it from the request (the
// state, the auth layer's principal, the `{name}` path segment).
async fn export_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    principal.require(Access::Read, &name)?;
    let bytes = export(app.pool(), &name, Vec::new()).await?;
    // `name` is a catalog that exists, so it is `[a-z0-9-]+` and safe in a header.
    let disposition = format!("attachment; filename=\"{name}.tar.gz\"");
    let headers = [
        (header::CONTENT_TYPE, "application/gzip".to_string()),
        (header::CONTENT_DISPOSITION, disposition),
    ];
    Ok((headers, bytes).into_response())
}

/// `?replace=true` on an import.
#[derive(Debug, Default, Deserialize)]
struct ImportQuery {
    /// Replace a non-empty catalog's documents.
    #[serde(default)]
    replace: bool,
}

/// `POST /catalogs/{name}/import`: imports the uploaded `.tar.gz`.
async fn import_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
    Query(query): Query<ImportQuery>,
    // The body extractor must come last: it consumes the request.
    body: Bytes,
) -> Result<Json<ImportReport>, AppError> {
    principal.require(Access::Admin, &name)?;
    let max_bytes = app.config().max_import_bytes.as_u64();
    // Decompressing and checking every entry is CPU work.
    let files = blocking(move || read_tarball(body.as_ref(), max_bytes)).await?;
    let options = ImportOptions {
        replace: query.replace,
        actor: &principal.actor,
        now: app.clock().now(),
    };
    let report = import(app.pool(), &name, files, &options)
        .await
        // `store::import`'s only refusal is a non-empty catalog without
        // `replace`; its hint names the CLI flag, so say what HTTP needs.
        .map_err(|error| match error {
            OkfError::Refused { .. } => OkfError::Refused {
                reason: format!(
                    "catalog {name:?} is not empty; add ?replace=true to replace its contents"
                ),
            },
            other => other,
        })?;
    // Swap the new snapshot in now, not at the next poll. The import has
    // committed, so a failed refresh must not turn it into an error (a
    // retry would then get 409): log it and leave it to the poll loop.
    if let Some(state) = app.catalog(&name) {
        if let Err(error) = state.refresh(app.pool()).await {
            tracing::error!(catalog = %name, %error, "refresh after import failed; the poll loop retries");
        }
    }
    tracing::info!(catalog = %name, key = %principal.key_id, "imported");
    Ok(Json(report))
}
```

Add a unit test at the end of `src/http/archives.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_limit_leaves_room_for_gzip_overhead() {
        assert_eq!(body_limit(0), 64 * 1024);
        assert_eq!(body_limit(64 << 20), (64 << 20) + (64 << 20) / 1000 + 64 * 1024);
        assert_eq!(body_limit(u64::MAX), usize::MAX);
    }
}
```

In `src/http/mod.rs`, add `mod archives;` and put `.merge(archives::routes(&app))` after `.route_service("/mcp", ...)` and before `.route_layer(...)`, replacing the comment that said Task 11 would.

- [ ] **Step 3: Run the tests**

Run: `cargo test --test server` and `cargo test --lib -- body_limit`
Expected: 8 passed, and 1.

- [ ] **Step 4: Changelog, gates, commit**

Write `changelog/<ts>-m4-task-11-archive-routes.md` (Notes: export is built in memory, as the CLI does). Run every gate. Commit:

```bash
git add src/http/ tests/server.rs changelog/
git commit -m "feat: export and import routes (M4.11)"
```

---

### Task 12: Smoke test, README, design update and milestone changelog

Runs the server on the real catalog over HTTP, records the timings, documents how to run it, and closes the milestone in the design.

**Files:**
- Modify: `tests/smoke.rs` (add `server_on_a_real_catalog`)
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`
- Create: `changelog/<ts>-m4-read-only-server.md` (milestone) and `changelog/<ts>-m4-task-12-wrap-up.md`

- [ ] **Step 1: The smoke test**

Add to `tests/smoke.rs` (and to its module doc: "`server_on_a_real_catalog` (M4) serves the imported checkout over HTTP and times each read tool and an export through rmcp's client"):

```rust
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn server_on_a_real_catalog() {
    let root = std::env::var("OKF_PARITY_CATALOG").expect("OKF_PARITY_CATALOG");
    let files = okf_mcp::archive::read_directory(std::path::Path::new(&root), 64 << 20).unwrap();
    let count = files.len();
    let started = Instant::now();
    let server = support::server::TestServer::start(vec![("pg", files)], okf_mcp::config::Config::default()).await;
    eprintln!("import {count} files and start the server: {:?}", started.elapsed());
    let key = server.key("smoke", okf_mcp::store::keys::Access::Read, &[], Some("pg")).await;
    let client = server.client(&key, None).await.unwrap();
    let started = Instant::now();
    let found = support::server::call(&client, "search", serde_json::json!({"query": "time server"})).await;
    eprintln!("search: {:?}", started.elapsed());
    // The best hit's path, so `get_concept` times a real concept.
    let first = support::server::structured(&found)["hits"][0]["path"]
        .as_str()
        .expect("the search finds a concept")
        .to_string();

    for (tool, arguments) in [
        ("list_catalogs", serde_json::json!({})),
        ("get_concept", serde_json::json!({"path": first, "links": true})),
        ("read_document", serde_json::json!({})),
        ("history", serde_json::json!({"path": "/log.md"})),
        ("open_questions", serde_json::json!({})),
        ("oldest", serde_json::json!({})),
        ("sprawl", serde_json::json!({})),
        ("links_report", serde_json::json!({})),
        ("links_report", serde_json::json!({})),
        ("lint", serde_json::json!({})),
    ] {
        let started = Instant::now();
        let result = support::server::call(&client, tool, arguments).await;
        eprintln!("{tool}: {:?} (error: {})", started.elapsed(), result.is_error == Some(true));
    }
    let started = Instant::now();
    let export = reqwest::Client::new()
        .get(format!("{}/catalogs/pg/export", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    eprintln!("export route: {} bytes in {:?}", export.len(), started.elapsed());
}
```

`links_report` runs twice: the second call reuses the snapshot's cached mention matcher (M3, #33).

Run: `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`
Expected: three tests pass; paste the timings into the milestone changelog.

- [ ] **Step 2: README**

Rewrite the status line (M1 to M4 delivered) and add "Running the server": `okf-mcp serve` (config file, `DATABASE_URL`, `--listen`), creating a catalog and a key, importing (`git -C <checkout> archive --format=tar.gz HEAD | okf-mcp import pg -`), the `mcp.json` entry from the design (`Authorization: Bearer`, `X-OKF-Catalogs`), the routes (`/healthz`, `/mcp`, export and import with `curl` examples using `$OKF_MCP_KEY`), and the tool list. Keep the testing section's Docker note.

- [ ] **Step 3: The design**

- Milestones table: "M4. Read-only server (delivered)" with the plan link and PR range.
- Status line at the top: M1 to M4 delivered.
- Add "What M4 shipped differently", listing the plan's deviations (key lookup by hash; the text block is the JSON; lists wrapped in objects; `open_questions` from `oldest`; export built in memory; the import body cap's gzip slack; the catalog list polled each tick; `poll` passes serialized; 404 for unreachable catalogs; no timeout on `/mcp`) and every dated `Corrected` note from this plan.
- In "Carried into M3 and later", mark #34 done in M4 (PR number), and the cross-catalog search merge.
- Add under "Session lifecycle", or as a new "Open questions for M5" paragraph: "MCP `2026-07-28` removes sessions; rmcp 3.8 serves a client on that version statelessly, with no `Mcp-Session-Id`. 'One changeset per (MCP session, catalog)' needs a different identity for such clients (for example per key and a client-chosen name); M5 decides (M4 plan, Decision 2). Whatever the identity, M5 binds it to the key that created it. In M4 each request is authorized by its own key, but a session's streams are not bound to the key that opened it: another valid key that learned an `Mcp-Session-Id` (a random UUID) could attach to the session's `GET /mcp` stream, or resume it with `Last-Event-ID`, and receive messages meant for the first key. M5 records the creating key with the session and refuses any other, before changesets depend on sessions."

- [ ] **Step 4: The milestone changelog**

`changelog/<ts>-m4-read-only-server.md`: what M4 delivers, the decisions, the deviations, every `Corrected` note, the smoke timings, test counts against M3's (279 unit, 6 golden parity, 1 roundtrip, 12 doc tests), and the follow-ups for M5 (changeset identity without sessions; the open M5 questions M3 left).

- [ ] **Step 5: Gates, commit**

Run every gate, plus the release smoke test. Commit:

```bash
git add tests/smoke.rs README.md docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "docs: M4 smoke test, README, design update and changelog (M4.12)"
```

---

## Self-review (2026-11-02)

- **Design coverage (M4 row).** axum: T6. rmcp: T5, T6. auth: T2 (401), T11 (403). `selection`: T3, wired in T5. Read tools needing no changeset: `list_catalogs` (T5), `search` (T7), `get_concept` and `read_document` (T8), `history` (T9), `open_questions`, `oldest`, `sprawl`, `links_report`, `lint` (T10). `pending_changes` and `stranded_sessions` need changesets and go to M5 (Scope). Import and export routes: T11. `/healthz`: T6. The poll loop ("Noticing changes made outside the server"): T4. Configuration fields: T1. End-to-end tests (missing, wrong and read-only keys; catalog precedence; export): T6, T11; the full write cycle is M5's. #34: T7.
- **Placeholders.** None; every step that changes code shows it, and every fact a test pins was checked against M1 to M3's code (the closing `---` is the tail's first line in `Doc`; import writes `import` revisions; sprawl paths are `/`-prefixed).
- **Names across tasks.** `Principal`, `Reach`, `KeyCache`, `Keys::{new, load, reload, find}`, `bearer`, `require_key` (T2) are used by T3, T4, T5, T6, T11. `Selection::{catalogs, write_target}`, `Sources`, `resolve`, `connection_catalogs` (T3) by T5. `App::{load, pool, config, clock, keys, catalogs, catalog, poll, poll_until}` (passes serialized by `polling`) and `Catalogs` (T4) by T5, T6, T11. `CatalogState::description`, `Loaded::{files, last_change}` (T4) by T5. `ToolContext::{new, from_parts, app, principal, catalogs, select, states, read_set, one}`, `OkfServer::{new, tools, context}`, `ToolResult`, `reply`, `tool_error`, `blocking` (T5) by T7 to T11. `OkfError::{Forbidden, kind, is_internal, to_json}` (T2, T5) by T5, T6. `AppError`, `REQUEST_TIMEOUT`, `router`, `serve` (T6) by T11 and the tests. `TestServer::{start, key, client, mcp_url}`, `call`, `structured`, `files`, `Client` (T6) by T11, T12.
