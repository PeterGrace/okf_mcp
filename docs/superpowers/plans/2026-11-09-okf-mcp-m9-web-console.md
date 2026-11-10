# OKF MCP Server, Milestone 9 (Web console): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** A browser console for `okf-mcp`, plus the JSON API it runs on. The console covers API key management, catalog health dashboards, catalog administration with `.tar.gz` import and export, maintenance findings and runs, changeset housekeeping, and read-only browsing and search. Everything the console does is also a documented `/api/v1` call.

**Architecture:**
- **One binary, one port.** `/api/v1/*` is nested beside the existing keyed router and applies the existing `require_key` bearer middleware itself, inside its header layer (corrected in issue #216). `/ui/*` serves a static single-page app (vanilla ES modules, Pico CSS) without a key.
- **Embedded assets.** `build.rs` embeds every file under `src/http/ui/` at build time, so adding a page needs no Rust edit.
- **Thin handlers.** API handlers build the same `ToolContext` the MCP tools build and call the existing `pub(crate)` tool functions, or the `store::*` functions the CLI calls. Behaviour cannot drift from MCP or the CLI.
- **Rendering stays in Rust.** Markdown is rendered to escaped HTML with `pulldown-cmark`, and diffs and reports are computed in Rust. The JS only fetches, displays, filters and sorts.
- **Browser tests.** End-to-end tests drive headless Chromium through `rustwright` from `cargo test`.

**Tech Stack:** Rust 2021 (toolchain 1.99.1), axum 0.8.9, everything M1 to M8 use, plus `pulldown-cmark` 0.13.6 (Task 4) and `rustwright` 0.3.0 (dev-dependency, Task 1). The front end is Pico CSS 2.1.1 (vendored), vanilla JS ES modules and Google Fonts. There is no npm and no JS build step.

Design: `docs/superpowers/specs/2026-11-09-okf-webui-design.md` (the "spec" below).

---

## Roadmap position

Milestone 9, the first milestone after the maintenance slices. M8 closed with epic #159, and `main` is at `62e88cd` (v0.11.2). M9 adds a second user-facing surface beside MCP and changes nothing in the MCP tool API.

## Scope

In scope (one task per spec delivery phase, plus a release task):

| Task | Spec phase |
|---|---|
| 1 | Foundation: `/api/v1` skeleton, `GET /whoami`, asset serving and security headers, app shell, login, theme, access-matrix and browser harnesses |
| 2 | API keys |
| 3 | Health dashboards |
| 4 | Browse and search (markdown renderer, tree, concept, links, history, revision diff, search) |
| 5 | Catalogs and import/export |
| 6 | Findings and runs |
| 7 | Changesets |
| 8 | Release v0.12.0 and wrap-up (README, operations guide, milestone changelog, smoke). Rollout waits for pgrace's go-ahead. |

Deliberately deferred:

| Item | Where it goes |
|---|---|
| Concept editing in the console (body, frontmatter, stamp, review, questions, create, move, split, delete, publish) | A later milestone (spec "Non-goals"). |
| OIDC / SSO or any identity beyond API keys | Not planned (spec "Decisions"). |
| Live updates (SSE or websockets) for findings and runs | Not planned. Pages reload on navigation and through a Refresh button. |
| A JS unit-test runner | Not planned. Behaviour is covered by the rustwright journeys (spec "Testing"). |

## Decisions taken while planning (2026-11-09)

1. **One task per spec phase,** seven feature tasks plus a release task (pgrace). Some PRs are large; each is still one reviewable feature.
2. **pgrace authorized merging this plan's PR** after the review is addressed.
3. **The epic ends with a release task:** prepare v0.12.0 (pgrace). Tagging, publishing the image and rolling out wait for pgrace's go-ahead at each outward step, as in M7 and M8.
4. **Assets are embedded by `build.rs`,** not listed by hand. `build.rs` walks `src/http/ui/` and writes `$OUT_DIR/ui_assets.rs`, a `const ASSETS: &[(&str, &[u8])]` of `include_bytes!`. A new page is just a new file. `cargo:rerun-if-changed=src/http/ui` scans the directory recursively.
5. **Pages are discovered, not registered.** `GET /ui/manifest.json` lists the embedded `js/pages/*.js`. The router loads `js/pages/<name>.js` on demand and hides nav links whose page is not in the build. Parallel tasks therefore never edit a shared page registry, and an unbuilt page never produces a 404 in the browser console.
6. **Every API area has its own file from Task 1.** Task 1 creates `src/http/api/{keys,health,browse,search,catalogs,maintenance,changesets}.rs` as stubs returning `Router::new()`, already merged in `api::routes`. A feature task fills in its own file only.
7. **Access is tested per area.** Each area's test module calls the shared `api::testing::assert_matrix` with one row per endpoint. A row lists the expected status for no key, read, write, admin, and an admin key that does not reach the catalog. See Deviation 1.
8. **All API responses carry `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`.** The spec required `no-store` only on `POST /keys`. Applying it to the whole API is simpler and keeps every key-bearing or private response out of caches.
9. **`/` redirects to `/ui/`.** Today `/` is a 404. The redirect makes the console the server's front page.
10. **The browser tests skip without Chromium.** Discovery is rustwright's own (`RUSTWRIGHT_CHROMIUM`, `CHROME`, `CHROMIUM`, the Playwright cache, `PATH`). A test prints a skip line and returns when `chromium().executable_path()` is `None`, the way the Docker-backed tests need Docker.
11. **Script errors and CSP violations fail the browser tests.** `main.js` turns `error`, `unhandledrejection` (except `ApiError`, which already toasted) and `securitypolicyviolation` events into `console.error`. The harness fails a journey on any `"error"` console record, because rustwright 0.3.0 has no page-error event (Verified facts).
12. **Admin mutations poll at once.** After any key or catalog change, the handler calls `app.poll().await`, so the change takes effect for the next request without waiting for `poll_interval`. The same is done for the snapshot after an import, which the archive handler already refreshes.

## Simplifications and deviations relative to the design

1. **Access-matrix coverage is a convention, not generated from the router.** axum cannot enumerate a router's routes. Each area test has a matrix row per endpoint, and the PR checklist plus review enforce "a new endpoint has a row". The spec ("Testing") says the route table is the test's input; Task 8 updates the spec to match.
2. **`tabs.js` is dropped.** Concept tabs use the same `segments` component as the report switcher. The spec's front-end file list is updated in Task 8.
3. **There is no CI test job.** The repo's CI builds images only, and the gates run locally. The spec's "Run with `just test-ui` and in CI" becomes "with `just test-ui` and in the local gates". Task 8 updates the spec.
4. **Import options become query parameters on the existing route.** Today `ImportQuery` has only `replace`. Task 5 adds `only_machinery` and `expect_generation` to it, and so to both the old and the `/api/v1` path. The CLI already has them.
5. **Downloads are checked by URL in browser tests.** rustwright 0.3.0 cannot save a download (Verified facts). The export journey asserts the button's request URL and fetches it with `reqwest`, and the API test checks the bytes.
6. **Per-document browse routes put the sub-resource before the path** (Task 4): `/catalogs/{catalog}/links/{*path}`, `/history/{*path}`, `/revisions/{revision}/{*path}`, not the spec's `/concepts/{*path}/links` and so on. An axum wildcard must be the last segment.
7. **Rendered markdown** (Task 4):
   - Heading ids are `section-<slug>`, with the slug itself in `data-slug`. A bare slug could take a shell element's id (`main`, `app`, `drawer`).
   - Table column alignment is dropped: pulldown-cmark writes it as a `style` attribute, which the CSP blocks.
   - Images become links (http/https) or alt text: the CSP blocks outside images, and the console serves no catalog files.
   - The concept endpoint always returns the whole outline. Links come only from `/links/{*path}`.
   - Search without `catalogs` searches every catalog the key reaches.
8. **Key administration stays within the caller's reach** (Task 2). A catalog-scoped admin lists and manages only keys whose catalogs it all reaches; any other key is 404. Granting or creating needs reach of each named catalog, and `all: true` needs an all-catalogs caller (403 otherwise). Key changes have no audit actor, because `store::keys` records none, as with the CLI. The DTO field is `default_catalog`, not `default`. A bad `access` is 400 via `InvalidPath { path: "access" }`.
9. **Creating a catalog needs an all-catalogs admin** (Task 5; 403 otherwise). A scoped key would not reach the catalog it created. `POST /catalogs` answers 201. An invalid name is 409 (the store refuses it like a taken name, as the CLI does).
10. **The archive routes are shared builders** (Task 5). `archives::export_route()` and `import_route(app)` are mounted at both the old paths and `/api/v1`. The `Refused` rewrite becomes a text replacement, so a generation mismatch keeps its own "export it again" message.
11. **Findings** (Task 6):
    - The store gains a disposition filter (`list_findings_with`, `count_findings`), so paging counts matching rows. The MCP tool schema is unchanged.
    - A new `GET /catalogs/{catalog}/maintenance/counts` feeds the nav badge.
    - The disposition body is `{disposition, rationale, idempotency_key}` (the tool's names; the spec said `note`).
    - Task 6 edits `main.js`, to refresh the badge after sign-in and on a catalog switch.
12. **Changeset diffs are owner-or-admin** (Task 7), as `ToolContext::changeset` enforces. The spec's "read" holds only in that a read key owns none.
13. **Reports** (Task 3): `?n=` defaults to 20 (the tools' default is 5). An unknown report name is 404. The `oldest` tile is the stalest concept's newest stamp (`generated`, `verified` or `reviewed`), which is what `oldest` measures.

## Verified facts the code relies on

`REG` stands for `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`. Each fact was checked in the crate source and, where marked, by compiling and running code in a scratch project.

**axum 0.8.9 / tower-http 0.6.11**
- A wildcard `/{*path}` captures without a leading slash. `/ui/{*path}` does not match `/ui` or `/ui/`, so register those separately (REG/axum-0.8.9/src/docs/routing/route.md:44-56).
- `Router::nest(path, router)` panics at the root and keeps the inner router's layers scoped to it (src/routing/mod.rs:209-211, docs/routing/nest.md:92-114).
- `route_layer` covers only the routes added before it, and runs only when a route matched, so an unknown path is 404, not 401 (docs/routing/route_layer.md:1-14). The existing `router()` relies on this (`src/http/mod.rs:198-231`).
- `Redirect::to` answers 303 and `Redirect::permanent` 308 (src/response/redirect.rs:37, :54).
- `[(HeaderName, &str); N]` implements `IntoResponseParts` (REG/axum-core-0.5.6/src/response/into_response_parts.rs:141), and `&'static [u8]` and `Bytes` implement `IntoResponse` (into_response.rs:202).
- Extractor rejection statuses (src/extract/rejection.rs):
  - `Json`: 422 for a data error (:10-19), 400 for a syntax error (:22-29), 415 without the JSON content type (:33-39).
  - `Query`: 400 (:84-90).
  - `Path`: 400 on a parse error (extract/path/mod.rs:438-445).
- `axum::middleware::map_response(f)` with `async fn(Response) -> Response` sets headers on every response of a router (REG/axum-0.8.9/src/middleware/map_response.rs:99; compiled). It needs no tower-http feature, unlike `SetResponseHeaderLayer`, which is behind `set-header` (REG/tower-http-0.6.11/src/lib.rs:213).

**pulldown-cmark 0.13.6** (Task 4; compiled and run)
- Options: `ENABLE_TABLES`, `ENABLE_STRIKETHROUGH`, `ENABLE_TASKLISTS`, `ENABLE_HEADING_ATTRIBUTES` (src/lib.rs:651-687).
- `Parser::new_ext(&str, Options)` (src/parse.rs:267).
- `html::push_html` writes `Event::Html` and `Event::InlineHtml` raw (src/html.rs:122) and escapes `Event::Text` (html.rs:101). Mapping both to `Text` therefore escapes raw HTML.
- `Tag::Link` and `Tag::Image` are `{ link_type, dest_url: CowStr, title: CowStr, id: CowStr }` (lib.rs:266-283). `Tag::Heading { level, id: Option<CowStr>, classes, attrs }` (lib.rs:164-170).
- `push_html` does not filter URL schemes (html.rs:336-364), so `javascript:` must be blocked by the renderer.
- Default features pull in `getopts`, so use `default-features = false, features = ["html"]`.

**rustwright 0.3.0** (dev-dependency; a real headless Chromium journey ran)
- **Synchronous API.** Each browser builds its own tokio runtime and `block_on`s on the caller (REG/rustwright-core-0.3.0/src/lib.rs:25691-25707, :40771-40777).
- **Panics inside a tokio runtime** ("Cannot start a runtime from within a runtime"; verified). Drive it from `tokio::task::spawn_blocking` inside a `#[tokio::test(flavor = "multi_thread")]`, so the server keeps worker threads.
- **Launching:**
  - `chromium().launch(LaunchOptions::default().headless(true)) -> Result<Browser>` (rustwright lib.rs:23, :38, :128-187).
  - `chromium().executable_path() -> Option<String>` previews discovery (lib.rs:33).
  - Discovery order (core :49684-49730, :49953-49975): explicit path, then `RUSTWRIGHT_CHROMIUM` / `CHROME` / `CHROMIUM`, then the Playwright cache, then `PATH`.
- **Browser:** `new_page() -> Result<Page>` (:231), `close()` (:313).
- **Page methods:**
  - Navigation: `goto(&str, GotoOptions)` (:772), `reload(GotoOptions)` (:927).
  - Input: `click(sel, ActionOptions)` (:981), `fill(sel, value, ActionOptions)` (:1073), both auto-waiting (verified).
  - Reading: `is_visible(sel) -> Result<bool>` (:1335), `text_content(sel, ActionOptions) -> Result<Option<String>>` (:1394; returns `None` at once for a missing element, without waiting), `get_attribute(sel, name)` (:1330).
  - Scripting: `evaluate(expr, Option<&Value>, ActionOptions) -> Result<serde_json::Value>` (:1405). Use the function form `"(arg) => ..."`, which runs under a CSP without `unsafe-eval`.
  - Console: `arm_console_capture()` (:710; call before `goto`), `console_records(include_previous, clear) -> Result<ConsoleRecords { records: Vec<ConsoleRecord { message_type: String, text: String, .. }>, .. }>` (:670, :470-490).
- **Not available:** `wait_for_selector`, locators, `set_input_files`, saving downloads, and a page-error event. The harness polls instead of waiting. A file input is filled by an `evaluate` that builds a `DataTransfer` (verified). A download is checked by URL (Deviation 5).
- **Dependencies:** rustwright-core is used with `default-features = false` (no pyo3). It adds reqwest 0.12 beside the project's 0.13 and resolves to the project's rustls 0.23.46. `cargo check --tests` with the project's dependencies passes with no warnings.

**This repository** (at `62e88cd`)
- The tool modules are private: `src/mcp/mod.rs:6-19` declares `mod catalogs; mod changesets; mod documents; mod history; mod maintenance; mod reports; mod restructure; mod search; mod writes;`, and `src/mcp/maintenance/mod.rs:3-4` declares `mod reports; mod work;`. Their functions are `pub(crate)`, and their parameter and result structs are `pub`. Task 1 makes the modules `pub(crate)`.
- Params structs derive `Deserialize` (not `Serialize`). Results derive `Serialize`. The maintenance params are `#[serde(deny_unknown_fields)]` and their `catalog: String` is required.
- `pub struct AppError(pub OkfError)` (`src/http/mod.rs:61`) has `From<OkfError>` and `IntoResponse`, and returns `(status, Json(error.to_json()))`. Statuses (79-101):

  | Status | Variants |
  |---|---|
  | 404 | NotFound, CatalogNotSelected, UnknownChangeset |
  | 403 | Forbidden |
  | 409 | Refused, Conflict, StillLinked |
  | 422 | LintFailed |
  | 503 | Busy |
  | 400 | InvalidPath, FrontmatterParse, SecretDetected, Shape, EditMismatch, ImportRefused |
  | 500 | Io, Database, Migrate, Config |

- `require_key` (`src/auth.rs:266`) inserts an `Arc<Principal>` extension, so handlers take `Extension(principal): Extension<Arc<Principal>>`. A missing or unknown key gets 401 with `WWW-Authenticate: Bearer` and the text body `missing or unknown API key\n`.
- `Principal::require(access, catalog)` (`src/auth.rs:75`) answers `CatalogNotSelected` (404) for an unreached catalog, then `Forbidden` (403) below `access`. `Access` is `Read < Write < Admin` and has `as_str()` but no serde derive.
- `ToolContext::new(app: Arc<App>, principal: Arc<Principal>, connection: Option<Vec<String>>)` (`src/mcp/context.rs:71`). Every tool's params carry `catalog`, which wins over the connection.
- The key cache and catalog list refresh only in `App::poll()` (`src/app.rs:249`), when `catalogs::admin_generation` moves. `App` exposes `pool()` (137), `catalogs() -> Arc<Catalogs>` (209), `catalog(&str)` (220) and `poll()` (249).
- Test support:
  - `src/mcp/testing.rs` (`#[cfg(test)]`): `TestApp { app }`, `app_with(&[(catalog, &[(path, text)])], Config) -> TestApp` (42), `principal_as(key_id, access, reach, default)` (108).
  - `tests/support/server.rs`: `TestServer::start(Vec<(&str, Files)>, Config)` (97), `.base` (`http://127.0.0.1:<port>`), `.key(id, Access, catalogs, default) -> String` (177; polls), and `files(&[(&str, &str)])` (48). It starts the real `okf_mcp::http::serve`, so `/ui` and `/api/v1` come with it.
- `store::keys::create_with_id(pool, &NewKey { name, access, maintenance_report, grants: Grants::{All | Catalogs(&[String])}, default_catalog, actor }, now) -> CreatedKey { id, key }` (`src/store/keys.rs:166`). It refuses a grant to a catalog that does not exist.
- `build.rs` exists and only emits `cargo:rerun-if-changed=migrations`.

## Conventions for every task

The M4 to M8 plans' "Conventions for every task" apply unchanged:
- no `.unwrap()` in library code, and `thiserror` errors;
- a doc comment on every public and `pub(crate)` item, with Arguments, Returns and Errors where they apply;
- comments that explain Rust-specific points to a Python expert;
- no emoji, and no unicode that imitates emoji, in code, UI text or docs;
- never a secret-shaped string;
- outward steps wait for pgrace's go-ahead.

**Nothing site-specific in this repository** (it is public). Docs, fixtures, tests and changelogs say `example.com` and use synthetic catalog names (`main`, `other`), never a real host, catalog, person or private repository.

**API handler shape** (every area):
- Signature: `async fn name(State(app): State<Arc<App>>, Extension(principal): Extension<Arc<Principal>>, ...) -> Result<Json<T>, AppError>`.
- A handler builds its context with `api::context(&app, principal)` and passes the path's catalog as the tool's `catalog` parameter.
- Access checks are the tool's own. Where a handler calls a `store::` function directly, it calls `principal.require(level, catalog)` first, or `require_admin(&principal)` for catalog-less admin routes (Task 1).
- Response DTOs that wrap store rows live in the area file and derive `Debug, Clone, PartialEq, Serialize`.
- **Every `/catalogs/...` route names its catalog segment `{catalog}`,** for consistency (axum 0.8.9 accepts different names at one position, checked by compiling, but one name keeps the areas readable). Concept and document paths are a trailing `{*path}`. A wildcard must be the last segment, so the sub-resource goes before the path (`/catalogs/{catalog}/history/{*path}`), never after it.

**Front-end shape** (every page):
- `src/http/ui/js/pages/<name>.js` exports `async function render(main, rest)`. `main` is this navigation's own fresh container inside `<main>` (the router's `mount`); `rest` holds the hash segments after the page name. A page writes only into `main`. After any `await`, before touching anything outside `main` (the drawer, the nav badge, `location`), it checks `main.isConnected` and returns if false: the user has navigated away and the router has detached that container. Never look `#main` up globally.
- DOM only through `h()` and `textContent`. Raw HTML only for server-rendered `body_html`, and only in Task 4.
- Every `api` call is awaited inside `try`/`catch` (or with `.catch(() => {})` when the default toast is enough), so no `ApiError` becomes an unhandled rejection.
- Element ids and `data-test` attributes used by the browser journeys are listed in the task.

**Gates** (all must pass before every push; paste the counts into the PR):
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test -- --test-threads=6` (needs Docker for testcontainers; the `ui_e2e` journeys need Chromium, otherwise they print a skip line)
- `just test-ui` (Task 1 adds it) with Chromium available. A PR touching `src/http/ui/` must show its journeys running, not skipped.
- `OKF_PARITY_CATALOG=<a local catalog checkout> cargo test --test parity -- --ignored` and the same for `--test roundtrip`, when a task touches `src/okf/` or `src/mcp/` behaviour. Only Task 1 does (module visibility).

**Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m9-task-<NN>-<slug>.md`, with Added/Changed/Notes sections.

Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. PR bodies end with the Claude Code attribution line.

Apart from Tasks 1 and 4, which were built and run in a scratch copy of the repository while drafting (see "Self-review"), the code in this plan has not been compiled; even those may need adjusting on the real tree. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## Interfaces shared across tasks

Task 1 defines these, and Tasks 2 to 7 use them.

```rust
// src/http/api/mod.rs
pub(crate) fn routes(app: &App) -> Router<Arc<App>>;            // nested at /api/v1
pub(crate) fn context(app: &Arc<App>, principal: Arc<Principal>) -> ToolContext;
pub(crate) fn require_admin(principal: &Principal) -> Result<(), OkfError>; // Forbidden below Admin
// One stub per area, each `pub(crate) fn routes() -> Router<Arc<App>>`,
// except catalogs::routes(app: &App) (it needs the import body limit).

// src/http/api/identity.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WhoAmI {
    pub key_id: String, pub name: Option<String>, pub actor: String,
    pub access: &'static str, pub maintenance_report: bool,
    pub all_catalogs: bool, pub catalogs: Vec<String>, pub default_catalog: Option<String>,
}

// src/http/api/testing.rs (#[cfg(test)])
pub(crate) const MAIN: &[(&str, &str)];                          // the synthetic `main` catalog
pub(crate) async fn app() -> TestApp;                            // catalogs `main` (MAIN) and `other`
pub(crate) struct Keys { pub read: String, pub write: String, pub admin: String, pub stranger: String }
pub(crate) async fn keys(test: &TestApp) -> Keys;                // reach `main`; stranger: admin on `other` only
pub(crate) async fn send(test: &TestApp, method: &str, uri: &str, key: Option<&str>,
                         body: Option<serde_json::Value>) -> (StatusCode, serde_json::Value);
pub(crate) struct Row { pub method: &'static str, pub uri: String, pub body: Option<serde_json::Value>,
                        pub read: u16, pub write: u16, pub admin: u16, pub stranger: u16 }
pub(crate) async fn assert_matrix(test: &TestApp, rows: &[Row]);  // also asserts 401 with no key
pub(crate) async fn assert_matrix_with(test: &TestApp, keys: &Keys, rows: &[Row]); // with caller-made keys
// A key's Principal (its id, for owning test state): test.app.keys().find(secret)
```

```js
// src/http/ui/js/core/dom.js
export function h(tag, attrs = {}, ...children)   // element builder; never innerHTML
export function clear(el)                         // empties and returns el
export const $ = (selector, root = document) => root.querySelector(selector)
export function pill(text, kind)                  // <span class="pill pill-<kind>">; kind: ok|warn|error|info|muted
// src/http/ui/js/core/api.js
export class ApiError extends Error { status; body }
export const api = { get(path, opts), post(path, body, opts), put(path, body, opts), patch(path, body, opts), del(path, opts) }
//   path is relative to /api/v1; opts: { raw, quiet, key }; a Blob body is sent as application/gzip
export const enc = encodeURIComponent
// src/http/ui/js/core/store.js
export const store = { whoami, key, catalog, theme, can(level) }   // can('read'|'write'|'admin')
// src/http/ui/js/core/toast.js     export function toast(message, kind = 'info')
// src/http/ui/js/core/drawer.js    export function openDrawer(title, subtitle, ...content) -> body element; export function closeDrawer()
// src/http/ui/js/core/format.js    export function ago(iso); export function when(iso); export function count(n, noun)
// src/http/ui/js/core/router.js    export function go(hash); export function route(); export function start()
//                                  export async function mount(render, rest)   // fresh per-navigation container
//                                  export function browseHash(catalog, path = '', section = null)
// src/http/ui/js/core/shell.js     export async function refreshIdentity()   // re-reads whoami; rebuilds the switcher and data-need gating
// src/http/ui/js/components/list-table.js
export function listTable(columns, rows, { onRow, empty }) -> table   // table.redraw(rows?), table.setFilter(fn|null)
//   columns: [{ key, label, render?(row), sort?(row), class? }]
// src/http/ui/js/components/segments.js
export function segments(options, value, onChange) -> element          // options: [{ value, label }]
// src/http/ui/js/components/confirm.js
export function confirmDialog({ title, message, action, danger, typed }) -> Promise<boolean>
```

```rust
// tests/ui_e2e/harness.rs
pub struct Ui { pub base: String, pub page: rustwright::Page, /* browser kept alive */ }
pub async fn drive<F: FnOnce(&Ui) + Send + 'static>(base: String, journey: F); // skips without Chromium
impl Ui {
    pub fn open(&self, route: &str);                 // goto {base}/ui/#{route}
    pub fn login(&self, key: &str);                  // login form, then waits for #app
    pub fn wait_visible(&self, selector: &str);      // polls up to 10 s, panics with the selector
    pub fn wait_hidden(&self, selector: &str);
    pub fn wait_text(&self, selector: &str, contains: &str) -> String;
    pub fn text(&self, selector: &str) -> String;    // "" when absent
    pub fn click(&self, selector: &str);
    pub fn fill(&self, selector: &str, value: &str);
    pub fn eval(&self, function: &str, arg: serde_json::Value) -> serde_json::Value;
    pub fn assert_no_console_errors(&self);
}
// tests/ui_e2e/main.rs: `#[path = "../support/mod.rs"] mod support; mod harness;` plus one `mod <page>;` per task
```

## File map

| File | Tasks |
|---|---|
| `Cargo.toml`, `Cargo.lock` | 1 (`rustwright` dev-dep), 4 (`pulldown-cmark`), 8 (version 0.12.0) |
| `build.rs` | 1 |
| `justfile` | 1 (`test-ui`) |
| `src/mcp/mod.rs`, `src/mcp/maintenance/mod.rs` (module visibility) | 1 |
| `src/http/mod.rs` (`router()` wiring) | 1 |
| `src/http/ui.rs` (new) | 1 |
| `src/http/api/mod.rs`, `identity.rs`, `testing.rs` (new) | 1 |
| `src/http/api/keys.rs` | 1 (stub), 2 |
| `src/http/api/health.rs` | 1 (stub), 3 |
| `src/http/api/browse.rs`, `src/http/api/search.rs`, `src/render.rs` (new), `src/lib.rs` (`pub mod render;`) | 1 (stubs), 4 |
| `src/mcp/changesets.rs` (`unified_diff` becomes `pub(crate)`) | 4 |
| `src/http/api/catalogs.rs`, `src/http/archives.rs` (import options, shared route builders) | 1 (stub), 5 |
| `src/http/api/maintenance.rs` | 1 (stub), 6 |
| `src/store/maintenance/{mod,runs,reports,report_tests}.rs`, `src/mcp/maintenance/reports.rs` (disposition filter, counts) | 6 |
| `src/http/ui/js/main.js` | 1, 6 (findings badge refresh) |
| `src/http/api/changesets.rs` | 1 (stub), 7 |
| `src/http/ui/index.html`, `styles.css`, `vendor/pico.min.css`, `js/main.js`, `js/core/*.js`, `js/components/{list-table,segments,confirm}.js` | 1. Later tasks may append CSS to `styles.css` (keep both sides). |
| `src/http/ui/js/pages/keys.js` | 2 |
| `src/http/ui/js/pages/health.js` | 3 |
| `src/http/ui/js/pages/browse.js`, `search.js`, `js/components/tree.js` | 4 |
| `src/http/ui/js/pages/catalogs.js`, `transfer.js`, `js/components/upload.js` | 5 |
| `src/http/ui/js/pages/findings.js`, `runs.js` | 6 |
| `src/http/ui/js/pages/changesets.js` | 7 |
| `tests/ui_e2e/main.rs` (one `mod` line per task: keep both sides), `harness.rs`, `login.rs` | 1, then 2 to 7 add `keys.rs`, `health.rs`, `browse.rs`, `catalogs.rs`, `maintenance.rs`, `changesets.rs` |
| `README.md`, `docs/operations.md` (new section "Web console") | 1 (section and route list), 2 to 7 (one subsection each: keep both sides), 8 |
| spec `docs/superpowers/specs/2026-11-09-okf-webui-design.md` | 8 (deviations and "What M9 shipped differently") |
| `changelog/` | every task |

## Task dependencies

```
T1 (foundation) ──┬─> T2 (API keys) ─────────────┐
                  ├─> T3 (health) ───────────────┤
                  ├─> T4 (browse & search) ──────┤
                  ├─> T5 (catalogs & transfer) ──┼─> T8 (release v0.12.0, wrap-up)
                  ├─> T6 (findings & runs) ──────┤
                  └─> T7 (changesets) ───────────┘
```

- **T1 first, alone.** Every other task builds on its router, harnesses and JS core.
- **T2 to T7 can run in parallel after T1.** Spec priority (and the order to start them in if fewer run at once): T2, T3, T4, T5, T6, T7. They touch disjoint files except:
  - `tests/ui_e2e/main.rs` (one `mod` line each);
  - `styles.css` (appended rules);
  - `README.md` and `docs/operations.md` (one subsection each);
  - `Cargo.toml` (T4 only).

  For all of these, keep both sides.
- **T3 links to T4's Browse page.** Until T4 lands, Health's rows link to `#/browse/<catalog>/<path>`, which the router shows as "not available yet". This is expected and needs no stub.
- **T8 waits for all.**

---

### Task 1: Foundation: API skeleton, assets, shell, login, harnesses

**Files:**
- Modify: `Cargo.toml` (dev-dependency `rustwright = "0.3.0"`)
- Modify: `build.rs`
- Modify: `justfile`
- Modify: `src/mcp/mod.rs:6-19`, `src/mcp/maintenance/mod.rs:3-4` (module visibility)
- Modify: `src/http/mod.rs` (`router()`, module declarations)
- Create: `src/http/ui.rs`
- Create: `src/http/api/{mod,identity,testing,keys,health,browse,search,catalogs,maintenance,changesets}.rs`
- Create: `src/http/ui/index.html`, `src/http/ui/styles.css`, `src/http/ui/vendor/pico.min.css`
- Create: `src/http/ui/js/main.js`, `js/core/{dom,api,store,toast,drawer,format,router,shell}.js`, `js/components/{list-table,segments,confirm}.js`
- Create: `tests/ui_e2e/main.rs`, `tests/ui_e2e/harness.rs`, `tests/ui_e2e/login.rs`
- Modify: `README.md`, `docs/operations.md` (section "Web console")
- Create: `changelog/<ts>-m9-task-01-foundation.md`

- [ ] **Step 1: Dependencies, build script and module visibility**

`Cargo.toml`, under `[dev-dependencies]`:

```toml
# Browser end-to-end tests of the web console (tests/ui_e2e): a native Rust
# Chromium driver (no Node). Its API is blocking; call it from spawn_blocking.
rustwright = "0.3.0"
```

`build.rs` (whole file):

```rust
//! Build-time work:
//! - rebuilds the crate when a migration is added or edited (`sqlx::migrate!`
//!   embeds `migrations/` at compile time, and cargo would not notice a new
//!   file);
//! - embeds the web console (`src/http/ui/**`) as `$OUT_DIR/ui_assets.rs`, a
//!   table of `include_bytes!` that `src/http/ui.rs` pastes in with `include!`.

use std::path::Path;
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=migrations");
    // A directory here makes cargo rescan it recursively for changes.
    println!("cargo:rerun-if-changed=src/http/ui");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let root = Path::new(&manifest).join("src/http/ui");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    // Sorted, so the generated file (and the binary) is reproducible.
    files.sort();
    let mut out = String::from(
        "/// Every file under `src/http/ui`, by its path below it (generated by build.rs).\n\
         const ASSETS: &[(&str, &[u8])] = &[\n",
    );
    for (relative, absolute) in &files {
        out.push_str(&format!("    ({relative:?}, include_bytes!({absolute:?})),\n"));
    }
    out.push_str("];\n");
    let dest = Path::new(&env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("ui_assets.rs");
    fs::write(dest, out).expect("write ui_assets.rs");
}

/// Appends `(path below root with '/' separators, absolute path)` for every
/// file under `dir`, recursively.
fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("read src/http/ui") {
        let path = entry.expect("read a directory entry").path();
        if path.is_dir() {
            collect(root, &path, files);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("a file below src/http/ui")
            .to_string_lossy()
            .replace('\\', "/");
        files.push((relative, path.to_string_lossy().into_owned()));
    }
}
```

`src/mcp/mod.rs:6-19`: change each `mod catalogs;`, `mod changesets;`, `mod documents;`, `mod history;`, `mod maintenance;`, `mod reports;`, `mod search;` to `pub(crate) mod ...;`. Leave `restructure` and `writes` private, since the console does not write concepts. In `src/mcp/maintenance/mod.rs:3-4`, make `reports` and `work` `pub(crate) mod`. A `pub(crate) mod` is visible anywhere in this crate but not to other crates, like a module-level `_private` convention that the compiler enforces.

- [ ] **Step 2: Write the failing API tests (identity and the harness itself)**

Create `src/http/api/testing.rs`:

```rust
//! Test helpers for the `/api/v1` routes: a two-catalog app, real keys at
//! every access level, a request helper over the whole router (auth
//! included), and the access-matrix assertion every area uses.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::config::Config;
use crate::http::router;
use crate::mcp::testing::{app_with, TestApp};
use crate::store::keys::{self as key_rows, Access, Grants, NewKey};

/// The synthetic `main` catalog every area's tests read.
pub(crate) const MAIN: &[(&str, &str)] = &[
    ("index.md", "# Main\n\n- [Alpha](systems/alpha.md): the first system\n"),
    ("log.md", "# Log\n"),
    ("AGENTS.md", "# Agents\n"),
    (
        "systems/alpha.md",
        "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha runs [Beta](beta.md).\n\n## Config\n\nNothing yet.\n",
    ),
    ("systems/beta.md", "---\ntype: system\ntitle: Beta\n---\n# Beta\n\nBeta.\n"),
    ("systems/index.md", "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n"),
];

/// An app holding `main` (from [`MAIN`]) and a one-file `other` catalog.
pub(crate) async fn app() -> TestApp {
    app_with(&[("main", MAIN), ("other", &[("index.md", "# Other\n")])], Config::default()).await
}

/// Secrets of live keys at each level. `read`, `write` and `admin` reach
/// `main` only; `stranger` is an admin key that reaches only `other`.
pub(crate) struct Keys {
    /// A read key on `main`.
    pub read: String,
    /// A write key on `main`.
    pub write: String,
    /// An admin key on `main`.
    pub admin: String,
    /// An admin key on `other` only.
    pub stranger: String,
}

/// Creates [`Keys`] in the test database and polls, so the server knows them.
pub(crate) async fn keys(test: &TestApp) -> Keys {
    let make = |name: &'static str, access: Access, catalog: &'static str| {
        let pool = test.app.pool().clone();
        async move {
            let grants = [catalog.to_owned()];
            let new = NewKey {
                name,
                access,
                maintenance_report: false,
                grants: Grants::Catalogs(&grants),
                default_catalog: Some(catalog),
                actor: name,
            };
            key_rows::create_with_id(&pool, &new, Utc::now())
                .await
                .expect("create a test key")
                .key
        }
    };
    let keys = Keys {
        read: make("reader", Access::Read, "main").await,
        write: make("writer", Access::Write, "main").await,
        admin: make("admin", Access::Admin, "main").await,
        stranger: make("stranger", Access::Admin, "other").await,
    };
    test.app.poll().await.expect("poll the new keys in");
    keys
}

/// Sends one request through the whole router (auth included).
///
/// # Returns
/// The status and the body as JSON (`Null` when it is not JSON, such as
/// the 401's text body or an export).
pub(crate) async fn send(
    test: &TestApp,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let request = match body {
        Some(json) => request
            .header("content-type", "application/json")
            .body(Body::from(json.to_string())),
        None => request.body(Body::empty()),
    }
    .expect("a valid request");
    let response = router(Arc::clone(&test.app), &CancellationToken::new())
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

/// One endpoint's expected status per caller.
pub(crate) struct Row {
    /// HTTP method.
    pub method: &'static str,
    /// Full URI, `/api/v1/...`.
    pub uri: String,
    /// JSON body, if the endpoint takes one.
    pub body: Option<serde_json::Value>,
    /// Expected status for the read key.
    pub read: u16,
    /// ... the write key.
    pub write: u16,
    /// ... the admin key on `main`.
    pub admin: u16,
    /// ... the admin key that does not reach `main`.
    pub stranger: u16,
}

/// Asserts every row with fresh [`Keys`]. See [`assert_matrix_with`].
pub(crate) async fn assert_matrix(test: &TestApp, rows: &[Row]) {
    let keys = keys(test).await;
    assert_matrix_with(test, &keys, rows).await;
}

/// Asserts every row with `keys`: 401 without a key, then each caller's
/// status. Use it when the rows need state owned by those keys (a
/// changeset opened by the write key, say), since one name and actor
/// cannot be created twice. Callers run in the order read, write,
/// stranger, admin, so a mutating row that succeeds only for admin runs
/// it last.
pub(crate) async fn assert_matrix_with(test: &TestApp, keys: &Keys, rows: &[Row]) {
    for row in rows {
        let (status, _) = send(test, row.method, &row.uri, None, row.body.clone()).await;
        assert_eq!(status.as_u16(), 401, "{} {} without a key", row.method, row.uri);
        for (who, key, want) in [
            ("read", &keys.read, row.read),
            ("write", &keys.write, row.write),
            ("stranger", &keys.stranger, row.stranger),
            ("admin", &keys.admin, row.admin),
        ] {
            let (status, body) = send(test, row.method, &row.uri, Some(key), row.body.clone()).await;
            assert_eq!(status.as_u16(), want, "{} {} as {who}: {body}", row.method, row.uri);
        }
    }
}
```

Create `src/http/api/identity.rs` with its tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    #[tokio::test]
    async fn whoami_describes_the_calling_key() {
        let test = app().await;
        let keys = keys(&test).await;
        let (status, body) = send(&test, "GET", "/api/v1/whoami", Some(&keys.read), None).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["name"], "reader");
        assert_eq!(body["actor"], "reader");
        assert_eq!(body["access"], "read");
        assert_eq!(body["all_catalogs"], false);
        assert_eq!(body["catalogs"], serde_json::json!(["main"]));
        assert_eq!(body["default_catalog"], "main");
        assert!(body.get("sha256").is_none(), "never a hash or secret");
    }

    #[tokio::test]
    async fn whoami_needs_any_key() {
        let test = app().await;
        assert_matrix(
            &test,
            &[Row { method: "GET", uri: "/api/v1/whoami".into(), body: None,
                    read: 200, write: 200, admin: 200, stranger: 200 }],
        )
        .await;
    }

    #[tokio::test]
    async fn api_responses_are_not_cached_or_sniffed() {
        let test = app().await;
        let keys = keys(&test).await;
        let valid = format!("Bearer {}", keys.read);
        // A good key, no key, an unknown key, and an unknown path: the
        // refusals carry the headers too, since `require_key` runs inside them.
        for (uri, authorization, want) in [
            ("/api/v1/whoami", Some(valid.as_str()), 200),
            ("/api/v1/whoami", None, 401),
            ("/api/v1/whoami", Some("Bearer okf_not-a-real-key"), 401),
            ("/api/v1/nothing-here", None, 404),
        ] {
            let mut request = axum::http::Request::builder().uri(uri);
            if let Some(value) = authorization {
                request = request.header("authorization", value);
            }
            let request = request.body(axum::body::Body::empty()).expect("request");
            let response = crate::http::router(
                std::sync::Arc::clone(&test.app),
                &tokio_util::sync::CancellationToken::new(),
            )
            .oneshot(request)
            .await
            .expect("response");
            let headers = response.headers();
            let case = format!("{uri} with {authorization:?}");
            assert_eq!(response.status().as_u16(), want, "{case}");
            assert_eq!(headers["cache-control"], "no-store", "{case}");
            assert_eq!(headers["x-content-type-options"], "nosniff", "{case}");
            if want == 401 {
                assert_eq!(headers["www-authenticate"], "Bearer", "{case}");
            }
        }
    }

    #[tokio::test]
    async fn an_unknown_api_path_is_404_not_401() {
        let test = app().await;
        let (status, _) = send(&test, "GET", "/api/v1/nothing-here", None, None).await;
        assert_eq!(status.as_u16(), 404);
    }
}
```

> **Corrected (issue #216, 2026-11-10).** The module has no `use super::*;`: nothing in it uses the parent's items, so the import was an unused-import warning. `use tower::ServiceExt;` (for `.oneshot`) is written in.

Create `src/http/ui.rs`'s tests (bottom of the file from Step 4):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::testing::app;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn get(path: &str) -> Response {
        let test = app().await;
        crate::http::router(std::sync::Arc::clone(&test.app),
            &tokio_util::sync::CancellationToken::new())
            .oneshot(Request::builder().uri(path).body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn the_shell_is_served_without_a_key_with_security_headers() {
        let response = get("/ui/").await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert_eq!(headers["content-security-policy"], CONTENT_SECURITY_POLICY);
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["referrer-policy"], "no-referrer");
    }

    #[tokio::test]
    async fn assets_have_their_content_type_and_unknown_ones_are_404() {
        assert_eq!(get("/ui/js/main.js").await.headers()["content-type"],
                   "text/javascript; charset=utf-8");
        assert_eq!(get("/ui/styles.css").await.headers()["content-type"], "text/css; charset=utf-8");
        assert_eq!(get("/ui/nope.js").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn root_and_bare_ui_redirect_to_the_shell() {
        let root = get("/").await;
        assert_eq!(root.status(), StatusCode::SEE_OTHER);
        assert_eq!(root.headers()["location"], "/ui/");
        let bare = get("/ui").await;
        assert_eq!(bare.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(bare.headers()["location"], "/ui/");
    }

    #[test]
    fn the_manifest_lists_embedded_pages_only() {
        let pages = page_names();
        assert!(pages.iter().all(|name| !name.contains('/') && !name.ends_with(".js")));
        // Task 1 embeds no page; later tasks add theirs (keys, health, ...).
        assert_eq!(pages.len(), ASSETS.iter().filter(|(p, _)| p.starts_with("js/pages/")).count());
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type("a/b.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("x.svg"), "image/svg+xml");
        assert_eq!(content_type("README"), "application/octet-stream");
    }
}
```

- [ ] **Step 3: Run the tests to make sure they fail**

Run: `cargo test --lib http:: 2>&1 | tail -20`
Expected: it does not compile, because `src/http/api` and `src/http/ui.rs` are not declared and `routes`, `WhoAmI`, `ASSETS`, `page_names` and `content_type` don't exist yet.

- [ ] **Step 4: Implement the API skeleton, identity and asset serving**

`src/http/api/mod.rs`:

```rust
//! The JSON REST API under `/api/v1` (web console design, "API"). Handlers
//! are thin adapters: they build the same [`ToolContext`] the MCP tools build
//! and call the same functions, or the `store` functions the CLI calls, so an
//! endpoint behaves exactly like its tool or command.

use std::sync::Arc;

use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::Response;
use axum::Router;

use crate::app::App;
use crate::auth::{require_key, Principal};
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::store::keys::Access;

mod browse;
mod catalogs;
mod changesets;
mod health;
mod identity;
mod keys;
mod maintenance;
mod search;
#[cfg(test)]
pub(crate) mod testing;

/// Every API route, for nesting at `/api/v1`, behind `require_key`.
///
/// # Arguments
/// * `app` - the application (its key cache for `require_key`; the import
///   route's body limit comes from its config).
///
/// # Returns
/// A router whose responses all carry `Cache-Control: no-store` and
/// `nosniff`, the 401 for a missing or unknown key included.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    Router::new()
        .merge(identity::routes())
        .merge(keys::routes())
        .merge(health::routes())
        .merge(browse::routes())
        .merge(search::routes())
        .merge(catalogs::routes(app))
        .merge(maintenance::routes())
        .merge(changesets::routes())
        // `route_layer` covers the routes merged above and runs only when one
        // matched, so an unknown API path stays 404 rather than 401.
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(app.keys()),
            require_key,
        ))
        // Its own fallback, so an unknown API path is answered here (404,
        // without a key check) and gets the headers below; without one, the
        // outer router's fallback would answer it.
        .fallback(|| async { StatusCode::NOT_FOUND })
        // A plain `layer` wraps everything above, `require_key` and the
        // fallback included, so the 401 and the 404 get these headers too.
        .layer(middleware::map_response(api_headers))
}

/// Adds the headers every API response carries: never cached (keys and
/// catalog content are private), never content-sniffed.
async fn api_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// The tool context for a request, as the MCP layer builds it, with no
/// connection catalogs: each handler passes its catalog as the tool's
/// `catalog` parameter, which wins over every other selection.
///
/// # Arguments
/// * `app` - the application.
/// * `principal` - the calling key, from `require_key`.
// Its first callers arrive with Tasks 3 to 7; until then the compiler would
// warn that it is unused. Whichever of them merges first removes this attribute.
#[allow(dead_code)]
pub(crate) fn context(app: &Arc<App>, principal: Arc<Principal>) -> ToolContext {
    ToolContext::new(Arc::clone(app), principal, None)
}

/// Refuses a key below `admin`, for the admin routes that name no catalog
/// (keys, catalog creation).
///
/// # Errors
/// [`OkfError::Forbidden`] for a read or write key.
// Its first callers arrive with Tasks 2 and 5; until then the compiler would
// warn that it is unused. Whichever of them merges first removes this attribute.
#[allow(dead_code)]
pub(crate) fn require_admin(principal: &Principal) -> Result<(), OkfError> {
    if principal.access >= Access::Admin {
        Ok(())
    } else {
        Err(OkfError::Forbidden { reason: "this needs an admin key".into() })
    }
}
```

> Check `OkfError::Forbidden`'s field type when implementing (`reason: String` per the error list). Adjust `.into()` if it differs.

> **Corrected (issue #216, 2026-11-10).** `OkfError::Forbidden { reason: String }` is as written. `context` carries the same `#[allow(dead_code)]` as `require_admin`: nothing in Task 1 calls it, and clippy's `-D warnings` refuses the dead-code warning. The first of Tasks 3 to 7 to call it removes the attribute (Task 2 calls only `require_admin`, so it removes that one if it merges before Task 5).

Each area stub (`keys.rs`, `health.rs`, `browse.rs`, `search.rs`, `maintenance.rs`, `changesets.rs`). Here is the content for `keys.rs`; each other file says its own area and spec section:

```rust
//! `/api/v1/keys` (web console design, "API": Keys). Filled in by M9 Task 2.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
}
```

`catalogs.rs` is the same, except that its signature is `pub(crate) fn routes(_app: &App) -> Router<Arc<App>>` (Task 5 uses `app` for the import body limit).

`src/http/api/identity.rs` (above its tests):

```rust
//! `GET /api/v1/whoami`: the calling key, so the console can greet it and
//! hide what it cannot do. Never includes the key or its hash.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;

use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::http::AppError;
use crate::store::keys as key_rows;

/// The calling key, as the console sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WhoAmI {
    /// The key's immutable id.
    pub key_id: String,
    /// Its grouping name (`None` if the row vanished meanwhile).
    pub name: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// `read`, `write` or `admin`.
    pub access: &'static str,
    /// Whether it may submit maintenance reports.
    pub maintenance_report: bool,
    /// Whether it reaches every catalog.
    pub all_catalogs: bool,
    /// The loaded catalogs it reaches, sorted.
    pub catalogs: Vec<String>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new().route("/whoami", get(whoami))
}

/// `GET /whoami`.
///
/// # Errors
/// 500 if the key list cannot be read.
async fn whoami(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<WhoAmI>, AppError> {
    let name = key_rows::list(app.pool())
        .await?
        .into_iter()
        .find(|record| record.id == principal.key_id)
        .map(|record| record.name);
    // `Catalogs` is a BTreeMap, so its keys come out sorted.
    let catalogs = app
        .catalogs()
        .keys()
        .filter(|catalog| principal.reaches(catalog))
        .cloned()
        .collect();
    Ok(Json(WhoAmI {
        key_id: principal.key_id.clone(),
        name,
        actor: principal.actor.clone(),
        access: principal.access.as_str(),
        maintenance_report: principal.maintenance_report,
        all_catalogs: matches!(principal.reach, Reach::All),
        catalogs,
        default_catalog: principal.default_catalog.clone(),
    }))
}
```

`src/http/ui.rs` (above its tests):

```rust
//! The web console's static files under `/ui/` (web console design, "Front
//! end"), embedded at build time by `build.rs`, plus the security headers
//! and the page manifest the client router reads.

use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};

// Pastes build.rs's generated `const ASSETS: &[(&str, &[u8])]` here, as if
// written in this file (like a C `#include`).
include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// The console's Content-Security-Policy: scripts and styles only from this
/// origin (Google Fonts' stylesheet and fonts excepted), no framing.
pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; \
    img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
    form-action 'self'";

/// The console's routes. They need no key: the files hold no catalog data.
pub(crate) fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(|| async { Redirect::to("/ui/") }))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(|| async { asset("index.html") }))
        .route("/ui/manifest.json", get(manifest))
        .route("/ui/{*path}", get(|Path(path): Path<String>| async move { asset(&path) }))
        .layer(middleware::map_response(security_headers))
}

/// The pages this build embeds (`js/pages/<name>.js`), by name.
fn page_names() -> Vec<&'static str> {
    ASSETS
        .iter()
        .filter_map(|(path, _)| path.strip_prefix("js/pages/")?.strip_suffix(".js"))
        .collect()
}

/// `GET /ui/manifest.json`: `{"pages": [...]}`.
async fn manifest() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pages": page_names() }))
}

/// The media type for an embedded file, by extension.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// One embedded file, revalidated on every load (`no-cache`), or 404.
fn asset(path: &str) -> Response {
    match ASSETS.iter().find(|(name, _)| *name == path) {
        Some((name, body)) => (
            [(header::CONTENT_TYPE, content_type(name)), (header::CACHE_CONTROL, "no-cache")],
            *body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Adds the CSP, `nosniff` and `no-referrer` to every console response.
async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}
```

> **Corrected (issue #216, 2026-11-10).** As first planned, the API was nested inside `keyed`, under `keyed`'s `require_key`. That put `require_key` outside `api_headers`, so a 401 from `/api/v1` lacked `no-store` and `nosniff` (review of PR #224). `api::routes(app)` now applies `require_key` itself as a `route_layer`, adds its own 404 fallback, and puts `api_headers` outside both. `router()` nests it beside `keyed` rather than inside it. The signature is unchanged. Later tasks still only merge into `routes()` above the `route_layer`, so every endpoint they add is keyed.

`src/http/mod.rs`: add `pub(crate) mod api;` and `mod ui;` beside `mod archives;`. In `router()`, merge the console beside `health` and nest the API beside `keyed` (it applies `require_key` itself):

```rust
    let keyed = Router::new()
        .merge(mcp)
        .merge(archives::routes(&app))
        .route_layer(middleware::from_fn_with_state(keys, require_key));
    let health = Router::new()
        .route("/healthz", get(healthz))
        .layer(timeout_layer());
    Router::new()
        .merge(health)
        .merge(ui::routes())
        // The API applies `require_key` itself, inside its header layer, so
        // even its 401 carries `no-store` and `nosniff`.
        .nest("/api/v1", api::routes(&app))
        .merge(keyed)
        // (TraceLayer and with_state unchanged)
```

`an_unknown_api_path_is_404_not_401` and `whoami_needs_any_key` prove two things: that `route_layer` covers the API routes, and that it leaves unmatched paths as 404. `api_responses_are_not_cached_or_sniffed` checks the headers on a 200, both 401s (no key, an unknown key) and the 404.

- [ ] **Step 5: Create the front end**

Download Pico (MIT; keep its license header):

```bash
mkdir -p src/http/ui/vendor src/http/ui/js/core src/http/ui/js/components src/http/ui/js/pages
curl -fsSL https://cdn.jsdelivr.net/npm/@picocss/pico@2.1.1/css/pico.min.css -o src/http/ui/vendor/pico.min.css
head -c 200 src/http/ui/vendor/pico.min.css   # starts with the "Pico CSS v2.1.1" license comment
```

`src/http/ui/js/pages/` stays empty in this task. Git does not keep empty directories, and `build.rs`'s `read_dir` only reads directories that exist, so nothing is needed.

`src/http/ui/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>okf console</title>
    <link rel="preconnect" href="https://fonts.googleapis.com">
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
    <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500&family=Space+Grotesk:wght@500;700&display=swap">
    <link rel="stylesheet" href="/ui/vendor/pico.min.css">
    <link rel="stylesheet" href="/ui/styles.css">
    <script type="module" src="/ui/js/main.js"></script>
</head>
<body>
    <section id="login" class="login-card" hidden>
        <h1 class="brand">okf</h1>
        <p class="muted">Paste an API key to continue. It stays in this browser until you sign out.</p>
        <form id="login-form">
            <label>API key
                <input type="password" name="key" autocomplete="off" required>
            </label>
            <button type="submit">Sign in</button>
        </form>
        <p id="login-error" class="error" hidden></p>
    </section>

    <div id="app" class="shell" hidden>
        <nav class="sidebar" aria-label="Sections">
            <div class="brand-block">
                <span class="brand">okf</span>
                <span class="brand-sub">Catalog console</span>
            </div>
            <label class="catalog-switch">Catalog
                <select id="catalog-select"></select>
            </label>
            <a class="nav-link" data-nav="health" href="#/health">Health</a>
            <a class="nav-link" data-nav="browse" href="#/browse">Browse</a>
            <a class="nav-link" data-nav="search" href="#/search">Search</a>
            <div class="nav-group">
                <span class="nav-group-label">Maintenance</span>
                <a class="nav-link" data-nav="findings" href="#/findings">Findings <span id="findings-badge" class="nav-badge" hidden></span></a>
                <a class="nav-link" data-nav="runs" href="#/runs">Runs</a>
                <a class="nav-link" data-nav="changesets" href="#/changesets">Changesets</a>
            </div>
            <div class="nav-group" data-need="admin">
                <span class="nav-group-label">Administer</span>
                <a class="nav-link" data-nav="catalogs" href="#/catalogs">Catalogs</a>
                <a class="nav-link" data-nav="transfer" href="#/transfer">Import &amp; export</a>
                <a class="nav-link" data-nav="keys" href="#/keys">API keys</a>
            </div>
            <div class="sidebar-footer">
                <span id="whoami" class="mono small"></span>
                <div class="footer-actions">
                    <button id="theme-toggle" class="text-btn" type="button">Theme</button>
                    <button id="signout" class="text-btn" type="button">Sign out</button>
                </div>
            </div>
        </nav>
        <main id="main"></main>
    </div>

    <div id="drawer-scrim" class="drawer-scrim" hidden></div>
    <aside id="drawer" class="drawer" aria-label="Details" hidden></aside>
    <div id="toasts" aria-live="polite"></div>
</body>
</html>
```

`src/http/ui/styles.css`:

```css
/* okf console theme, layered on Pico CSS 2 (vendor/pico.min.css).
 * Light and dark are driven by data-theme on <html> (js/main.js).
 * Fonts: Space Grotesk (headings), Inter (body), JetBrains Mono (paths, ids). */

:root {
    --font-body: 'Inter', system-ui, -apple-system, sans-serif;
    --font-display: 'Space Grotesk', 'Inter', system-ui, sans-serif;
    --font-mono: 'JetBrains Mono', ui-monospace, Menlo, monospace;
    --bg: #fafaf9; --surface: #ffffff; --surface-2: #f4f4f3;
    --fg: #18181b; --fg-muted: #71717a;
    --border: #e4e4e7; --border-strong: #d4d4d8;
    --accent: #6d28d9; --accent-fg: #ffffff; --accent-soft: #ede9fe;
    --ok: #047857; --ok-soft: #d1fae5;
    --warn: #b45309; --warn-soft: #fef3c7;
    --error: #b91c1c; --error-soft: #fee2e2;
    --shadow: 0 1px 2px rgba(0,0,0,.04), 0 4px 12px rgba(0,0,0,.06);
    --radius: 10px; --radius-sm: 6px;
}
[data-theme="dark"] {
    --bg: #0f0f11; --surface: #18181b; --surface-2: #1f1f23;
    --fg: #e4e4e7; --fg-muted: #a1a1aa;
    --border: #27272a; --border-strong: #3f3f46;
    --accent: #a78bfa; --accent-fg: #1a1a1c; --accent-soft: #2e1065;
    --ok: #34d399; --ok-soft: #064e3b;
    --warn: #fbbf24; --warn-soft: #451a03;
    --error: #f87171; --error-soft: #450a0a;
    --shadow: 0 1px 2px rgba(0,0,0,.3), 0 4px 16px rgba(0,0,0,.4);
}
/* Map the tokens onto Pico's variables, so its form controls follow the theme. */
:root, [data-theme="dark"] {
    --pico-font-family: var(--font-body);
    --pico-font-size: 14px;
    --pico-background-color: var(--bg);
    --pico-color: var(--fg);
    --pico-muted-color: var(--fg-muted);
    --pico-muted-border-color: var(--border);
    --pico-primary: var(--accent);
    --pico-primary-background: var(--accent);
    --pico-primary-border: var(--accent);
    --pico-primary-hover-background: var(--accent);
    --pico-primary-inverse: var(--accent-fg);
    --pico-form-element-background-color: var(--surface);
    --pico-form-element-border-color: var(--border-strong);
    --pico-border-radius: var(--radius-sm);
    --pico-spacing: .75rem;
}

html, body { margin: 0; background: var(--bg); color: var(--fg); font-family: var(--font-body); }
h1, h2, h3, .brand { font-family: var(--font-display); letter-spacing: -.01em; }
h1 { font-size: 1.5rem; margin: 0 0 .25rem; }
h2 { font-size: 1.15rem; }
code, .mono { font-family: var(--font-mono); font-size: .9em; }
.muted { color: var(--fg-muted); }
.small { font-size: .85em; }
.error { color: var(--error); }
[hidden] { display: none !important; }

/* login */
.login-card { max-width: 400px; margin: 12vh auto 0; padding: 2rem; background: var(--surface);
    border: 1px solid var(--border); border-radius: var(--radius); box-shadow: var(--shadow); }
.login-card .brand { font-size: 2rem; margin: 0; }

/* shell */
.shell { display: grid; grid-template-columns: 232px 1fr; min-height: 100vh; }
.sidebar { position: sticky; top: 0; height: 100vh; display: flex; flex-direction: column; gap: 2px;
    padding: 1.25rem .75rem 1rem; background: var(--surface); border-right: 1px solid var(--border); overflow-y: auto; }
.brand-block { padding: 0 .5rem .75rem; display: flex; flex-direction: column; }
.brand-block .brand { font-size: 1.2rem; font-weight: 700; }
.brand-sub { font-size: .65rem; letter-spacing: .1em; text-transform: uppercase; color: var(--fg-muted); }
.catalog-switch { padding: 0 .5rem .5rem; font-size: .75rem; color: var(--fg-muted); }
.catalog-switch select { margin: .25rem 0 0; padding: .3rem .5rem; font-family: var(--font-mono); font-size: .85rem; }
.nav-group { display: flex; flex-direction: column; gap: 2px; margin-top: 1rem; }
.nav-group-label { padding: 0 .5rem .25rem; font-size: .65rem; letter-spacing: .1em; text-transform: uppercase; color: var(--fg-muted); }
.nav-link { display: flex; justify-content: space-between; align-items: center; padding: .4rem .5rem;
    border-radius: var(--radius-sm); color: var(--fg); text-decoration: none; }
.nav-link:hover { background: var(--surface-2); }
.nav-link.active { background: var(--accent-soft); color: var(--accent); font-weight: 600; }
.nav-badge { background: var(--error); color: #fff; border-radius: 999px; padding: 0 .45rem; font-size: .7rem; font-weight: 700; }
.sidebar-footer { margin-top: auto; padding: .75rem .5rem 0; border-top: 1px solid var(--border); display: flex; flex-direction: column; gap: .4rem; }
.footer-actions { display: flex; gap: .5rem; }
.text-btn { background: none; border: 1px solid var(--border); color: var(--fg-muted); padding: .2rem .6rem; font-size: .8rem; width: auto; margin: 0; }
main { padding: 1.75rem 2.25rem; min-width: 0; }
.page-head { display: flex; justify-content: space-between; align-items: flex-start; gap: 1rem; margin-bottom: 1.25rem; }
.page-head .actions { display: flex; gap: .5rem; }
.page-head button { width: auto; margin: 0; }

/* tiles, segments, tables, pills */
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: .75rem; margin-bottom: 1.25rem; }
.tile { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); padding: .75rem 1rem; }
.tile .n { font-family: var(--font-display); font-size: 1.7rem; font-weight: 700; }
.tile .l { font-size: .75rem; color: var(--fg-muted); }
.segments { display: inline-flex; border: 1px solid var(--border); border-radius: var(--radius-sm); overflow: hidden; margin-bottom: .75rem; }
.segments .seg { background: var(--surface); color: var(--fg-muted); border: 0; border-right: 1px solid var(--border);
    border-radius: 0; padding: .35rem .8rem; font-size: .85rem; width: auto; margin: 0; }
.segments .seg:last-child { border-right: 0; }
.segments .seg.active { background: var(--accent-soft); color: var(--accent); font-weight: 600; }
.list-table { width: 100%; background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius);
    border-collapse: separate; border-spacing: 0; overflow: hidden; }
.list-table th { font-size: .68rem; letter-spacing: .08em; text-transform: uppercase; color: var(--fg-muted);
    font-weight: 500; background: var(--surface); border-bottom: 1px solid var(--border); }
.list-table th.sortable { cursor: pointer; }
.list-table td { border-bottom: 1px solid var(--border); background: var(--surface); }
.list-table tr:last-child td { border-bottom: 0; }
.list-table tr.clickable { cursor: pointer; }
.list-table tr.clickable:hover td { background: var(--surface-2); }
.list-table td.empty { text-align: center; padding: 1.5rem; }
.pill { display: inline-block; font-size: .68rem; font-weight: 700; letter-spacing: .04em; text-transform: uppercase;
    border-radius: 999px; padding: .05rem .5rem; }
.pill-ok { background: var(--ok-soft); color: var(--ok); }
.pill-warn { background: var(--warn-soft); color: var(--warn); }
.pill-error { background: var(--error-soft); color: var(--error); }
.pill-info { background: var(--accent-soft); color: var(--accent); }
.pill-muted { background: var(--surface-2); color: var(--fg-muted); }

/* drawer, dialogs, toasts */
.drawer-scrim { position: fixed; inset: 0; background: rgba(0,0,0,.25); z-index: 20; }
.drawer { position: fixed; top: 0; right: 0; bottom: 0; width: min(720px, 92vw); background: var(--surface);
    border-left: 1px solid var(--border); box-shadow: var(--shadow); z-index: 21; overflow-y: auto; padding: 1.25rem 1.5rem; }
.drawer-head { display: flex; justify-content: space-between; gap: 1rem; align-items: flex-start; margin-bottom: 1rem; }
.drawer-head h2 { margin: 0; font-size: 1rem; }
.drawer-actions { display: flex; flex-wrap: wrap; gap: .5rem; margin: 1rem 0; }
.drawer-actions button { width: auto; margin: 0; }
dialog.confirm article { max-width: 480px; }
button.danger { background: var(--error); border-color: var(--error); color: #fff; }
#toasts { position: fixed; right: 1rem; bottom: 1rem; display: flex; flex-direction: column; gap: .5rem; z-index: 40; }
.toast { background: var(--surface); border: 1px solid var(--border); border-left: 4px solid var(--accent);
    border-radius: var(--radius-sm); padding: .6rem .9rem; box-shadow: var(--shadow); max-width: 420px; }
.toast-error { border-left-color: var(--error); }
.toast-ok { border-left-color: var(--ok); }
pre.diff { background: var(--surface-2); padding: .75rem; border-radius: var(--radius-sm); overflow-x: auto; font-size: .8rem; }

@media (max-width: 760px) {
    .shell { grid-template-columns: 1fr; }
    .sidebar { position: static; height: auto; }
    main { padding: 1rem; }
}
```

`src/http/ui/js/core/dom.js`:

```js
// DOM helpers. Data reaches the page only through h() and textContent, never
// innerHTML, so catalog text can never become markup.

export const $ = (selector, root = document) => root.querySelector(selector);

// h('a', { href: '#/x', class: 'nav', onclick: fn }, 'text', child, [more])
export function h(tag, attrs = {}, ...children) {
    const el = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs ?? {})) {
        if (value === false || value === null || value === undefined) continue;
        if (name.startsWith('on') && typeof value === 'function') el.addEventListener(name.slice(2), value);
        else if (name === 'dataset') Object.assign(el.dataset, value);
        else el.setAttribute(name, value === true ? '' : String(value));
    }
    for (const child of children.flat(Infinity)) {
        if (child === null || child === undefined || child === false) continue;
        el.append(child instanceof Node ? child : String(child));
    }
    return el;
}

export function clear(el) {
    el.replaceChildren();
    return el;
}

// kind: ok | warn | error | info | muted
export function pill(text, kind = 'muted') {
    return h('span', { class: `pill pill-${kind}` }, text);
}
```

`src/http/ui/js/core/store.js`:

```js
// Browser-side state: the signed-in key and preferences, the whoami answer.
// Values live in memory; localStorage only persists them across reloads.
// Storage can be unavailable or throw (private windows, blocked site data),
// and then the console still works for this page's lifetime.

const KEY = 'okf.key';
const CATALOG = 'okf.catalog';
const THEME = 'okf.theme';
const LEVELS = ['read', 'write', 'admin'];
const memory = new Map();

function read(name) {
    if (!memory.has(name)) {
        let stored = null;
        try { stored = localStorage.getItem(name); } catch { /* unavailable: memory only */ }
        memory.set(name, stored);
    }
    return memory.get(name);
}
function write(name, value) {
    memory.set(name, value ?? null);
    try {
        if (value === null || value === undefined) localStorage.removeItem(name);
        else localStorage.setItem(name, value);
    } catch { /* unavailable: the value lives in memory only */ }
}

export const store = {
    whoami: null,
    get key() { return read(KEY); },
    set key(value) { write(KEY, value); },
    get catalog() { return read(CATALOG); },
    set catalog(value) { write(CATALOG, value); },
    get theme() { return read(THEME); },
    set theme(value) { write(THEME, value); },
    can(level) {
        return !!this.whoami && LEVELS.indexOf(this.whoami.access) >= LEVELS.indexOf(level);
    },
};
```

`src/http/ui/js/core/toast.js`:

```js
import { h } from './dom.js';

// kind: info | ok | error. Errors stay longer and are announced as alerts.
export function toast(message, kind = 'info') {
    const el = h('div', { class: `toast toast-${kind}`, role: kind === 'error' ? 'alert' : 'status' }, message);
    document.getElementById('toasts').append(el);
    setTimeout(() => el.remove(), kind === 'error' ? 8000 : 4000);
}
```

`src/http/ui/js/core/api.js`:

```js
// The one way pages talk to the server: /api/v1 with the stored bearer key.
// A non-2xx answer becomes an ApiError (already toasted unless `quiet`);
// a 401 also signs the browser out through the handler main.js installs.
import { store } from './store.js';
import { toast } from './toast.js';

export class ApiError extends Error {
    constructor(status, body) {
        super(body?.message ?? `HTTP ${status}`);
        this.status = status;
        this.body = body;
    }
}

let unauthorized = () => {};
export function setUnauthorizedHandler(handler) { unauthorized = handler; }

async function request(method, path, { body, raw = false, quiet = false, key } = {}) {
    const headers = { Authorization: `Bearer ${key ?? store.key ?? ''}` };
    let payload;
    if (body instanceof Blob) {
        payload = body;
        headers['Content-Type'] = 'application/gzip';
    } else if (body !== undefined) {
        payload = JSON.stringify(body);
        headers['Content-Type'] = 'application/json';
    }
    const response = await fetch(`/api/v1${path}`, { method, headers, body: payload });
    if (response.status === 401) {
        unauthorized();
        throw new ApiError(401, null);
    }
    if (!response.ok) {
        let errorBody = null;
        try { errorBody = await response.json(); } catch { /* not JSON */ }
        const error = new ApiError(response.status, errorBody);
        if (!quiet) toast(response.status === 403 ? 'Your key cannot do this.' : error.message, 'error');
        throw error;
    }
    if (raw) return response;
    return response.status === 204 ? null : response.json();
}

export const api = {
    get: (path, opts) => request('GET', path, opts),
    post: (path, body, opts) => request('POST', path, { ...opts, body }),
    put: (path, body, opts) => request('PUT', path, { ...opts, body }),
    patch: (path, body, opts) => request('PATCH', path, { ...opts, body }),
    del: (path, opts) => request('DELETE', path, opts),
};

export const enc = encodeURIComponent;
```

`src/http/ui/js/core/drawer.js`:

```js
import { h, clear } from './dom.js';

// Opens the right-hand drawer and returns its body element for the caller to fill.
export function openDrawer(title, subtitle, ...content) {
    const drawer = document.getElementById('drawer');
    const body = h('div', { class: 'drawer-body' }, ...content);
    clear(drawer).append(
        h('header', { class: 'drawer-head' },
            h('div', {},
                h('h2', { class: 'mono', id: 'drawer-title' }, title),
                subtitle ? h('p', { class: 'muted small' }, subtitle) : null),
            h('button', { class: 'text-btn', type: 'button', id: 'drawer-close', onclick: closeDrawer }, 'Close')),
        body);
    drawer.hidden = false;
    document.getElementById('drawer-scrim').hidden = false;
    return body;
}

export function closeDrawer() {
    document.getElementById('drawer').hidden = true;
    document.getElementById('drawer-scrim').hidden = true;
}
```

`src/http/ui/js/core/format.js`:

```js
// Display formatting only; the server computes everything else.

export function ago(iso) {
    if (!iso) return '-';
    const seconds = Math.max(0, Math.round((Date.now() - Date.parse(iso)) / 1000));
    const steps = [[60, 's', 1], [3600, 'min', 60], [86400, 'h', 3600], [2592000, 'd', 86400]];
    for (const [limit, unit, size] of steps) if (seconds < limit) return `${Math.floor(seconds / size)} ${unit} ago`;
    return `${Math.floor(seconds / 2592000)} mo ago`;
}

export function when(iso) {
    return iso ? new Date(iso).toLocaleString() : '-';
}

export function count(n, noun) {
    return `${n} ${noun}${n === 1 ? '' : 's'}`;
}
```

`src/http/ui/js/core/router.js`:

```js
// Hash routing: '#/<page>/<rest...>' loads js/pages/<page>.js and calls its
// render(view, rest). /ui/manifest.json says which pages this build has, so
// a nav link to a page not built yet is hidden, never a 404.
//
// Each navigation renders into its own fresh container (`view`), and the
// previous one is detached from #main. A render still awaiting an API answer
// when the user moves on therefore writes into a detached element, never
// into the newer page. Pages check `view.isConnected` after an await before
// touching anything outside `view` (the drawer, the nav badge).
import { h } from './dom.js';

let pages = new Set();
let started = false;
const DEFAULTS = ['health', 'browse', 'keys'];

export function parse(hash) {
    const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);
    return { page: parts.shift() ?? '', rest: parts };
}

// '#/browse/<catalog>/<path segments>[/<section slug>]', each segment encoded
// (parse() decodes each one). src/render.rs (Task 4) writes the same shape
// into rendered links. path may start with '/' or not.
export function browseHash(catalog, path = '', section = null) {
    const parts = [catalog, ...String(path).replace(/^\/+/, '').split('/').filter(Boolean)];
    if (section) parts.push(section);
    return '#/browse/' + parts.map(encodeURIComponent).join('/');
}

export function go(hash) {
    if (location.hash === hash) route();
    else location.hash = hash;
}

// Replaces #main's content with a fresh view and runs render(view, rest) in it.
// Exported for the router journey in tests/ui_e2e/login.rs.
export async function mount(render, rest) {
    const view = h('div', { class: 'view' });
    document.getElementById('main').replaceChildren(view);
    await render(view, rest);
}

export async function route() {
    let { page, rest } = parse(location.hash);
    if (!page) page = DEFAULTS.find(name => pages.has(name)) ?? '';
    document.querySelectorAll('.nav-link').forEach(link => link.classList.toggle('active', link.dataset.nav === page));
    if (!pages.has(page)) {
        await mount(view => view.append(h('h1', {}, page ? 'Not available yet' : 'Welcome'),
            h('p', { class: 'muted' }, page ? `This build has no ${page} page.` : 'No pages are installed in this build yet.')), rest);
        return;
    }
    const placeholder = location.hash;
    const module = await import(`../pages/${page}.js`);
    if (location.hash !== placeholder) return;   // the user moved on while it loaded
    await mount(module.render, rest);
}

export async function start() {
    const manifest = await (await fetch('/ui/manifest.json')).json();
    pages = new Set(manifest.pages);
    document.querySelectorAll('.nav-link').forEach(link => { if (!pages.has(link.dataset.nav)) link.hidden = true; });
    if (!started) {
        addEventListener('hashchange', route);
        started = true;
    }
    await route();
}
```

`src/http/ui/js/components/list-table.js`:

```js
import { h } from '../core/dom.js';

// A table over rows already in memory, sortable by any column with a sort().
// columns: [{ key, label, render?(row) -> Node|string, sort?(row) -> value, class? }]
export function listTable(columns, rows, { onRow, empty = 'Nothing here.' } = {}) {
    let sortKey = null;
    let direction = 1;
    let filter = null;
    const body = h('tbody');
    const head = h('tr', {}, columns.map(column => h('th', {
        scope: 'col',
        class: column.sort ? 'sortable' : null,
        onclick: column.sort ? () => { direction = sortKey === column.key ? -direction : 1; sortKey = column.key; draw(); } : null,
    }, column.label)));
    const table = h('table', { class: 'list-table' }, h('thead', {}, head), body);

    function draw() {
        const view = filter ? rows.filter(filter) : rows.slice();
        if (sortKey) {
            const column = columns.find(c => c.key === sortKey);
            view.sort((a, b) => {
                const x = column.sort(a);
                const y = column.sort(b);
                return (x > y ? 1 : x < y ? -1 : 0) * direction;
            });
        }
        body.replaceChildren(...(view.length
            ? view.map(row => h('tr', { class: onRow ? 'clickable' : null, onclick: onRow ? () => onRow(row) : null },
                columns.map(column => h('td', { class: column.class ?? null },
                    column.render ? column.render(row) : (row[column.key] ?? '-')))))
            : [h('tr', {}, h('td', { colspan: columns.length, class: 'muted empty' }, empty))]));
    }

    table.redraw = newRows => { if (newRows) rows = newRows; draw(); };
    table.setFilter = fn => { filter = fn; draw(); };
    draw();
    return table;
}
```

`src/http/ui/js/components/segments.js`:

```js
import { h } from '../core/dom.js';

// A segmented control. options: [{ value, label }]; onChange(value) on a click.
export function segments(options, value, onChange) {
    const wrap = h('div', { class: 'segments', role: 'tablist' });
    const draw = () => wrap.replaceChildren(...options.map(option => h('button', {
        type: 'button',
        role: 'tab',
        class: option.value === value ? 'seg active' : 'seg',
        'aria-selected': String(option.value === value),
        dataset: { value: option.value },
        onclick: () => { value = option.value; draw(); onChange(value); },
    }, option.label)));
    draw();
    return wrap;
}
```

`src/http/ui/js/components/confirm.js`:

```js
import { h } from '../core/dom.js';

// A modal confirmation; resolves true or false. With `typed`, the action
// stays disabled until the user types that exact text (used for deletes).
export function confirmDialog({ title, message, action = 'Confirm', danger = true, typed = null }) {
    return new Promise(resolve => {
        const input = typed
            ? h('input', { type: 'text', autocomplete: 'off', id: 'confirm-typed', 'aria-label': `Type ${typed} to confirm` })
            : null;
        const ok = h('button', { type: 'button', id: 'confirm-ok', class: danger ? 'danger' : null, disabled: typed ? true : null }, action);
        const cancel = h('button', { type: 'button', id: 'confirm-cancel', class: 'secondary' }, 'Cancel');
        const dialog = h('dialog', { class: 'confirm' },
            h('article', {},
                h('h3', {}, title),
                h('p', {}, message),
                typed ? h('p', { class: 'small muted' }, 'Type ', h('code', {}, typed), ' to confirm.') : null,
                input,
                h('footer', {}, cancel, ok)));
        const done = value => { dialog.close(); dialog.remove(); resolve(value); };
        if (input) input.addEventListener('input', () => { ok.disabled = input.value !== typed; });
        ok.addEventListener('click', () => done(true));
        cancel.addEventListener('click', () => done(false));
        dialog.addEventListener('cancel', event => { event.preventDefault(); done(false); });
        document.body.append(dialog);
        dialog.showModal();
    });
}
```

`src/http/ui/js/core/shell.js`:

```js
// The signed-in identity in the shell: who, which catalogs, which groups.
// Pages that change keys or catalogs call refreshIdentity() afterwards.
import { $, h } from './dom.js';
import { store } from './store.js';
import { api } from './api.js';

export async function refreshIdentity() {
    store.whoami = await api.get('/whoami');
    const me = store.whoami;
    $('#whoami').textContent = `${me.name ?? me.key_id} - ${me.access}`;
    document.querySelectorAll('[data-need]').forEach(el => { el.hidden = !store.can(el.dataset.need); });
    if (!me.catalogs.includes(store.catalog)) {
        store.catalog = me.catalogs.includes(me.default_catalog) ? me.default_catalog : (me.catalogs[0] ?? null);
    }
    const select = $('#catalog-select');
    select.replaceChildren(...me.catalogs.map(name => h('option', { value: name }, name)));
    select.value = store.catalog ?? '';
}
```

`src/http/ui/js/main.js`:

```js
// Boot: theme, sign-in, the shell, the router.
import { $ } from './core/dom.js';
import { store } from './core/store.js';
import { api, ApiError, setUnauthorizedHandler } from './core/api.js';
import { start, route } from './core/router.js';
import { closeDrawer } from './core/drawer.js';
import { refreshIdentity } from './core/shell.js';

// Surface script errors and CSP violations as console errors: the browser
// tests fail on any. An ApiError was already shown as a toast.
addEventListener('error', event => console.error('script error:', event.message));
addEventListener('unhandledrejection', event => {
    if (event.reason instanceof ApiError) { event.preventDefault(); return; }
    console.error('unhandled rejection:', event.reason?.message ?? String(event.reason));
});
document.addEventListener('securitypolicyviolation', event =>
    console.error('CSP violation:', event.violatedDirective, event.blockedURI));

function applyTheme(theme) {
    const chosen = theme ?? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
    document.documentElement.dataset.theme = chosen;
}

function showLogin(message) {
    $('#app').hidden = true;
    $('#login').hidden = false;
    const error = $('#login-error');
    error.textContent = message ?? '';
    error.hidden = !message;
}

async function enter() {
    await refreshIdentity();
    $('#login').hidden = true;
    $('#app').hidden = false;
    await start();
}

// enter(), but a failure never leaves a blank page: a refused key is already
// back on the login card (the 401 handler), and any other failure (the
// server down, a 500) shows the card with the reason. The stored key is
// kept, so a reload retries with it.
async function enterOrRecover() {
    try {
        await enter();
    } catch (error) {
        if (error instanceof ApiError && error.status === 401) return;
        showLogin(`The console could not load: ${error.message}. Reload to retry, or sign in again.`);
    }
}

setUnauthorizedHandler(() => {
    store.key = null;
    store.whoami = null;
    showLogin('The key was refused. Sign in again.');
});

$('#login-form').addEventListener('submit', async event => {
    event.preventDefault();
    const key = String(new FormData(event.target).get('key') ?? '').trim();
    try {
        await api.get('/whoami', { key, quiet: true });
    } catch (error) {
        showLogin(error.status === 401 ? 'That key was not accepted.' : `Sign-in failed: ${error.message}`);
        return;
    }
    store.key = key;
    event.target.reset();
    await enterOrRecover();
});
$('#signout').addEventListener('click', () => {
    store.key = null;
    store.whoami = null;
    history.replaceState(null, '', '/ui/');
    showLogin();
});
$('#theme-toggle').addEventListener('click', () => {
    const next = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
    store.theme = next;
    applyTheme(next);
});
$('#catalog-select').addEventListener('change', event => {
    store.catalog = event.target.value;
    route();
});
$('#drawer-scrim').addEventListener('click', closeDrawer);
addEventListener('keydown', event => { if (event.key === 'Escape') closeDrawer(); });

applyTheme(store.theme);
if (store.key) enterOrRecover();
else showLogin();
```

> **Corrected (issue #216, 2026-11-10).** As first planned, boot ran `enter().catch(() => {})`. A stored key whose first `/whoami` failed with anything but a 401 (a 500, the server unreachable) then left both `#login` and `#app` hidden: a blank page (review of PR #224). `enterOrRecover()` shows the login card with the reason and keeps the stored key, and sign-in uses it too. `enter()` is unchanged, so Task 6's edit (`refreshFindingsBadge();` after `await start();` in `enter()`) still applies as written. The journey `a_failed_start_with_a_stored_key_shows_the_login_card` covers it.

- [ ] **Step 6: Run the API and asset tests**

Run: `cargo test --lib http:: -- --test-threads=6`
Expected: PASS, including `whoami_describes_the_calling_key`, `whoami_needs_any_key`, `an_unknown_api_path_is_404_not_401`, `the_shell_is_served_without_a_key_with_security_headers`, `root_and_bare_ui_redirect_to_the_shell` and the existing `src/http` tests.

- [ ] **Step 7: Write the browser harness and the login journeys (failing first)**

`tests/ui_e2e/main.rs`:

```rust
//! Browser end-to-end tests of the web console (M9), driven by rustwright
//! against the real server. Each journey skips when no Chromium is found
//! (set RUSTWRIGHT_CHROMIUM); `just test-ui` runs them.

#[path = "../support/mod.rs"]
mod support;

mod harness;
mod login;
// Tasks 2 to 7 add: mod keys; mod health; mod browse; mod catalogs; mod maintenance; mod changesets;
```

`tests/ui_e2e/harness.rs`:

```rust
//! Driving headless Chromium from tokio tests. rustwright's API blocks and
//! runs its own runtime, so a journey runs on a blocking thread while the
//! server keeps the test runtime's worker threads.

use std::time::{Duration, Instant};

use rustwright::{chromium, ActionOptions, Browser, GotoOptions, LaunchOptions, Page};

/// How long `wait_*` polls before failing.
const WAIT: Duration = Duration::from_secs(10);

/// One browser page on the console.
pub struct Ui {
    /// The server, `http://127.0.0.1:<port>`.
    pub base: String,
    /// The page.
    pub page: Page,
    // Declared after `page`, so the page is dropped before its browser
    // (Rust drops struct fields in declaration order).
    _browser: Browser,
}

/// Runs `journey` in a fresh headless Chromium, then fails it on any console
/// error (script errors and CSP violations are logged as errors by main.js).
/// Returns without running when no Chromium is installed.
pub async fn drive<F>(base: String, journey: F)
where
    F: FnOnce(&Ui) + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        if chromium().executable_path().is_none() {
            eprintln!("skipping browser journey: no Chromium found (set RUSTWRIGHT_CHROMIUM)");
            return;
        }
        let browser = chromium()
            .launch(LaunchOptions::default().headless(true))
            .expect("launch Chromium");
        let page = browser.new_page().expect("open a page");
        page.arm_console_capture().expect("capture the console");
        let ui = Ui { base, page, _browser: browser };
        journey(&ui);
        ui.assert_no_console_errors();
    })
    .await
    .expect("the journey thread panicked");
}

impl Ui {
    /// Navigates to `#<route>` (route like "/keys"; "" for the shell).
    pub fn open(&self, route: &str) {
        self.page
            .goto(&format!("{}/ui/#{route}", self.base), GotoOptions::default())
            .expect("navigate");
    }

    /// Signs in with `key` from the login card and waits for the shell.
    pub fn login(&self, key: &str) {
        self.open("");
        self.wait_visible("#login");
        self.fill("#login-form input[name=key]", key);
        self.click("#login-form button[type=submit]");
        self.wait_visible("#app");
    }

    fn poll(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}");
    }

    /// Waits until `selector` is visible.
    pub fn wait_visible(&self, selector: &str) {
        self.poll(selector, || self.page.is_visible(selector).unwrap_or(false));
    }

    /// Waits until `selector` is absent or hidden.
    // Its first callers arrive with Tasks 2 to 7 (a closed drawer or dialog);
    // until then the compiler would warn that it is unused.
    #[allow(dead_code)]
    pub fn wait_hidden(&self, selector: &str) {
        self.poll(selector, || !self.page.is_visible(selector).unwrap_or(false));
    }

    /// Waits until `selector`'s text contains `contains`, and returns the text.
    pub fn wait_text(&self, selector: &str, contains: &str) -> String {
        let mut last = String::new();
        self.poll(&format!("{selector} containing {contains:?}"), || {
            last = self.text(selector);
            last.contains(contains)
        });
        last
    }

    /// `selector`'s text content, or "" when it is absent.
    pub fn text(&self, selector: &str) -> String {
        self.page
            .text_content(selector, ActionOptions::default())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Clicks `selector` (auto-waits for it).
    pub fn click(&self, selector: &str) {
        self.page.click(selector, ActionOptions::default()).expect("click");
    }

    /// Fills the input `selector` with `value` (auto-waits for it).
    pub fn fill(&self, selector: &str, value: &str) {
        self.page.fill(selector, value, ActionOptions::default()).expect("fill");
    }

    /// Runs a JS function expression `(arg) => ...` in the page and returns
    /// its JSON result. The function form runs under the console's CSP.
    pub fn eval(&self, function: &str, arg: serde_json::Value) -> serde_json::Value {
        self.page
            .evaluate(function, Some(&arg), ActionOptions::default())
            .expect("evaluate")
    }

    /// Fails on any console record of type "error" since the page opened.
    /// Chromium's own "Failed to load resource" line for an HTTP error
    /// status is ignored: journeys provoke 401, 403 and 409 on purpose, and
    /// the console reports those through toasts, not script errors.
    pub fn assert_no_console_errors(&self) {
        let records = self.page.console_records(true, false).expect("read the console");
        let errors: Vec<&str> = records
            .records
            .iter()
            .filter(|record| record.message_type == "error")
            .filter(|record| !record.text.starts_with("Failed to load resource"))
            .map(|record| record.text.as_str())
            .collect();
        assert!(errors.is_empty(), "console errors: {errors:?}");
    }
}
```

`tests/ui_e2e/login.rs`:

```rust
//! Signing in and out, a refused key, the theme toggle, what a read key
//! sees, and a start that fails with a stored key.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use rustwright::GotoOptions;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

async fn server() -> TestServer {
    TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_signs_in_sees_every_group_and_signs_out() {
    let server = server().await;
    let key = server.key("console-admin", Access::Admin, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "admin");
        assert_eq!(ui.eval("() => document.querySelector('#catalog-select').value", serde_json::json!(null)), "main");
        assert!(ui.page.is_visible("[data-need=admin]").expect("visible"));
        ui.click("#signout");
        ui.wait_visible("#login");
        // Signed out means the key is gone: a reload stays on the login card.
        ui.open("");
        ui.wait_visible("#login");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_key_is_refused_on_the_login_card() {
    let server = server().await;
    drive(server.base.clone(), |ui| {
        ui.open("");
        ui.wait_visible("#login");
        ui.fill("#login-form input[name=key]", "okf_not-a-real-key");
        ui.click("#login-form button[type=submit]");
        ui.wait_text("#login-error", "not accepted");
        assert!(!ui.page.is_visible("#app").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_key_sees_no_administer_group_and_stays_signed_in() {
    let server = server().await;
    let key = server.key("console-reader", Access::Read, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "read");
        assert!(!ui.page.is_visible("[data-need=admin]").expect("visible"));
        ui.open("");
        ui.wait_visible("#app"); // localStorage kept the key across the reload
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_works_when_storage_throws() {
    let server = server().await;
    let key = server.key("console-nostorage", Access::Read, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.open("");
        ui.wait_visible("#login");
        // Every storage call now throws, as with blocked site data.
        ui.eval(
            "() => { for (const m of ['getItem', 'setItem', 'removeItem']) \
             Storage.prototype[m] = () => { throw new DOMException('denied', 'SecurityError'); }; }",
            serde_json::json!(null),
        );
        ui.fill("#login-form input[name=key]", &key);
        ui.click("#login-form button[type=submit]");
        ui.wait_visible("#app");
        ui.wait_text("#whoami", "read");
        ui.click("#theme-toggle"); // a preference write must not throw either
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_page_render_never_lands_in_a_newer_page() {
    let server = server().await;
    let key = server.key("console-router", Access::Read, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        // Two navigations through the real router: the first render is
        // still awaiting when the second runs, then appends late.
        let result = ui.eval(
            "async () => { \
               const router = await import('/ui/js/core/router.js'); \
               let release; \
               const slow = router.mount(async view => { \
                 await new Promise(resolve => { release = resolve; }); \
                 view.append(Object.assign(document.createElement('p'), { id: 'stale' })); }, []); \
               await router.mount(async view => { \
                 view.append(Object.assign(document.createElement('p'), { id: 'fresh' })); }, []); \
               release(); await slow; \
               return [!!document.getElementById('stale'), !!document.getElementById('fresh')]; }",
            serde_json::json!(null),
        );
        assert_eq!(result, serde_json::json!([false, true]));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_theme_toggle_flips_and_persists() {
    let server = server().await;
    let key = server.key("console-theme", Access::Read, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        let theme = |ui: &crate::harness::Ui| {
            ui.eval("() => document.documentElement.dataset.theme", serde_json::json!(null))
        };
        let before = theme(ui);
        ui.click("#theme-toggle");
        let after = theme(ui);
        assert_ne!(before, after);
        ui.open("");
        ui.wait_visible("#app");
        assert_eq!(theme(ui), after);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_start_with_a_stored_key_shows_the_login_card() {
    let server = server().await;
    let key = server
        .key("console-boot", Access::Read, &["main"], Some("main"))
        .await;
    let app = std::sync::Arc::clone(&server.app);
    // The journey runs on a blocking thread; this handle lets it run one
    // async call (closing the pool) on the test's runtime.
    let runtime = tokio::runtime::Handle::current();
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "read");
        // The key stays stored, but the next /whoami fails with a 500: the
        // key cache is in memory, and listing key names needs the database.
        runtime.block_on(app.pool().close());
        ui.page.reload(GotoOptions::default()).expect("reload");
        ui.wait_visible("#login");
        ui.wait_text("#login-error", "could not load");
        assert!(!ui.page.is_visible("#app").expect("visible"));
    })
    .await;
}
```

Run: `cargo test --test ui_e2e` before Steps 4 and 5 exist (or with `src/http/ui/js/main.js` empty).
Expected: FAIL. `an_admin_signs_in_...` times out waiting for `#login`. If no Chromium is installed, each test prints the skip line instead: install Chromium or set `RUSTWRIGHT_CHROMIUM` before going on.

- [ ] **Step 8: Run the journeys to make sure they pass**

`justfile`, after `test:`:

```make
# Browser end-to-end tests of the web console (needs Docker and Chromium;
# RUSTWRIGHT_CHROMIUM names the binary if it is not on PATH).
test-ui:
    cargo test --test ui_e2e -- --test-threads=2
```

Run: `just test-ui`
Expected: 7 passed, with no skip lines (6 as first planned; the seventh, `a_failed_start_with_a_stored_key_shows_the_login_card`, was added in issue #216). (`a_slow_page_render_never_lands_in_a_newer_page` relies on `evaluate` awaiting a returned promise, as Playwright's does; if rustwright 0.3.0 does not, poll a `window` flag the function sets instead.)

> **Corrected (issue #216, 2026-11-10).** Checked: rustwright 0.3.0's `evaluate` awaits a returned promise (it sends `awaitPromise: true`), and the journey passes as written, so no fallback is needed. `wait_hidden` carries `#[allow(dead_code)]` (no Task 1 journey calls it, and the test crate is linted with `-D warnings`); the first task whose journey calls it removes the attribute. A probe journey confirmed that `assert_no_console_errors` fails on both a `console.error` and a CSP violation.

- [ ] **Step 9: Docs and changelog**

- `README.md`: after "Running the server", add a short section "Web console". It says that `/ui/` is the console and `/` redirects there; that you sign in with an API key, which stays in that browser's `localStorage` until you sign out; and that every console action is an `/api/v1` call with the same bearer key. Show one `curl -H "Authorization: Bearer $OKF_MCP_KEY" http://127.0.0.1:8080/api/v1/whoami` example. Point to `docs/operations.md#web-console`.
- `docs/operations.md`: add section "Web console", before "7. Health and time", with:
  - the routes `/ui/*` (no key), `/api/v1/*` (bearer key) and `/` (redirect);
  - the security headers and the CSP string, and that Google Fonts is the only third-party origin;
  - the fact that keys are held in the browser's `localStorage`, so sign out on shared machines;
  - an API reference table that later tasks extend (Task 1 adds `GET /api/v1/whoami`);
  - `just test-ui` and `RUSTWRIGHT_CHROMIUM`.
- `changelog/<ts>-m9-task-01-foundation.md`.

- [ ] **Step 10: Gates and commit**

Run every gate in "Conventions". Then:

```bash
git add Cargo.toml Cargo.lock build.rs justfile src/mcp/mod.rs src/mcp/maintenance/mod.rs \
    src/http/mod.rs src/http/ui.rs src/http/api src/http/ui tests/ui_e2e README.md \
    docs/operations.md changelog/
git commit -m "feat(console): API skeleton, embedded console shell, login and browser tests (M9 task 1)"
```

### Task 2: API keys

Depends on Task 1. Parallel with Tasks 3 to 7.

**Facts this task relies on** (verified at `62e88cd`):
- `store::keys`: `Access` (`src/store/keys.rs:16`, `as_str` 30, `parse(&str) -> Option<Self>` 45, no serde derive); `Grants<'a>::{All, Catalogs(&'a [String])}` (59); `NewKey<'a> { name, access, maintenance_report, grants, default_catalog: Option<&str>, actor: &str }` (68); `CreatedKey { id, key }` (85); `KeyRecord { id, name, sha256, access, maintenance_report, all_catalogs, catalogs, default_catalog, actor, created_at, revoked_at }` (92), not `Serialize`.
- `create_with_id(pool, &NewKey, now) -> Result<CreatedKey>` (166): `Refused` (409) for a bad name (`[A-Za-z0-9._-]+`), no catalogs, an ungranted default or a taken live name+actor; `CatalogNotSelected` (404) for an unknown catalog.
- `list(pool) -> Vec<KeyRecord>` (321), revoked keys included.
- `set_maintenance_report_target(pool, target, actor: Option<&str>, enabled)` (360), `revoke_target(pool, target, actor, now)` (407), `grant_target(pool, target, actor, catalog)` (449), `ungrant_target(pool, target, actor, catalog)` (488) all resolve `target` through `resolve_active` (525): an exact generated id wins, else an unambiguous **active name**. Their `actor` argument only disambiguates a name (and is refused with an id); it is not an audit actor. Grant and ungrant refuse a revoked or `all_catalogs` key with `Refused` (`require_named_grants`, 598). Revoking a revoked key is `Refused`. Each bumps the admin generation, so `App::poll` (`src/app.rs:249`, `Result<(), OkfError>`) reloads the key cache.
- `crate::keys::KEY_PREFIX = "okf_"` (`src/keys.rs:15`).
- `Principal { key_id, access, reach, actor, .. }`, `Reach::{All, Catalogs}` (`src/auth.rs:24-46`), `reaches` (57), `require` (75: unreached -> `CatalogNotSelected`, then `Forbidden`). `App::keys().find(secret) -> Option<Arc<Principal>>` (`src/auth.rs:228`, `src/app.rs:152`).
- `OkfError::InvalidPath { path, reason }` (`src/error.rs:23`) is 400 and is written `"{path}: {reason}"`; `NotFound { path }` (16) is 404; `Forbidden { reason }` (137) 403; `Refused { reason }` (144) 409 (`src/http/mod.rs:79-101`).
- `clock::format_timestamp(DateTime<Utc>) -> String` (`src/clock.rs:56`); chrono has no serde feature in this crate (`Cargo.toml:30`), so DTOs carry formatted strings.
- `App::clock() -> &dyn Clock` (`src/app.rs:147`) with `now()`.
- The CLI defaults a new key's actor to its name (`src/cli.rs:484-503`).

**Security decisions (this task):**
1. Every route needs an admin key (`api::require_admin`, 403 below admin).
2. **An admin manages only keys inside its own reach.** A key is *managed* by the caller when the caller reaches every catalog (`Reach::All`), or when the key is not an all-catalogs key and every catalog it is granted is one the caller reaches. `GET /keys` lists only managed keys. Every `/keys/{id}` route looks the key up among them by exact id and answers 404 (`NotFound`) otherwise, so a catalog-scoped admin can neither see, revoke, re-flag nor re-grant a key that reaches further than it does (a key with every grant removed reaches nothing and is managed by every admin; it can only be given catalogs the caller reaches).
3. **No privilege escalation on create or grant.** Each named catalog (and the default) goes through `principal.require(Access::Admin, catalog)` (404 when unreached). `all: true` needs a `Reach::All` caller (403 otherwise). The access level cannot exceed the caller's, since the caller is already admin.
4. **The path's `{id}` is a key id, never a name.** The handler resolves it among the listed records by exact id and passes the record's id to the store, so `resolve_active`'s name fallback never applies.
5. **Secrets.** `KeyView` is built by destructuring `KeyRecord`, dropping `sha256` explicitly. The secret appears only in `POST /keys`'s response (`CreatedKeyView`), whose `Debug` is hand-written to redact it. The whole API is already `Cache-Control: no-store` (Decision 8).
6. A bad `access` value is 400 `InvalidPath { path: "access", reason }` (the only 400 variant whose fields fit a named field; the body's `path` is `"/access"`).

**Files:**
- Modify: `src/http/api/keys.rs` (replaces Task 1's stub)
- Modify: `src/store/keys.rs` (`*_by_id` key changes; corrected in issue #217)
- Create: `src/http/ui/js/pages/keys.js`
- Modify: `src/http/ui/styles.css` (append the "keys" block)
- Create: `tests/ui_e2e/keys.rs`; Modify: `tests/ui_e2e/main.rs` (`mod keys;`)
- Modify: `README.md`, `docs/operations.md` ("Web console" subsection "API keys")
- Create: `changelog/<ts>-m9-task-02-api-keys.md`

- [ ] **Step 1: Write the failing API tests**

Put this test module at the bottom of `src/http/api/keys.rs` (below Task 1's stub for now). It imports what it needs explicitly instead of `use super::*`, so it compiles against the stub.

```rust
#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::mcp::testing::TestApp;
    use crate::store::keys::{self as key_rows, Access, CreatedKey, Grants, NewKey};

    /// Creates a key straight in the store, as `okf-mcp key create` would,
    /// and polls so the server knows it.
    async fn stored(test: &TestApp, name: &str, access: Access, grants: Grants<'_>) -> CreatedKey {
        let new = NewKey {
            name,
            access,
            maintenance_report: false,
            grants,
            default_catalog: None,
            actor: name,
        };
        let created = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
            .await
            .expect("create a key");
        test.app.poll().await.expect("poll the key in");
        created
    }

    fn main_only() -> Vec<String> {
        vec!["main".to_string()]
    }

    #[tokio::test]
    async fn key_routes_need_an_admin_that_reaches_the_key() {
        let test = app().await;
        let main = main_only();
        let first = stored(&test, "target-one", Access::Read, Grants::Catalogs(&main)).await.id;
        let second = stored(&test, "target-two", Access::Read, Grants::Catalogs(&main)).await.id;
        // Rows run read, write, stranger, admin; only admin succeeds, last.
        // `first` is revoked by its last row, so its other rows come before.
        assert_matrix(
            &test,
            &[
                Row { method: "GET", uri: "/api/v1/keys".into(), body: None,
                      read: 403, write: 403, admin: 200, stranger: 200 },
                Row { method: "POST", uri: "/api/v1/keys".into(),
                      body: Some(json!({"name": "made", "access": "read",
                                        "catalogs": ["main"], "default_catalog": "main"})),
                      read: 403, write: 403, admin: 200, stranger: 404 },
                Row { method: "PUT", uri: format!("/api/v1/keys/{first}/catalogs/main"), body: None,
                      read: 403, write: 403, admin: 200, stranger: 404 },
                Row { method: "PATCH", uri: format!("/api/v1/keys/{first}"),
                      body: Some(json!({"maintenance_report": true})),
                      read: 403, write: 403, admin: 200, stranger: 404 },
                Row { method: "POST", uri: format!("/api/v1/keys/{first}/revoke"), body: None,
                      read: 403, write: 403, admin: 200, stranger: 404 },
                Row { method: "DELETE", uri: format!("/api/v1/keys/{second}/catalogs/main"), body: None,
                      read: 403, write: 403, admin: 200, stranger: 404 },
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn create_returns_the_secret_once_and_the_new_key_works() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = json!({"name": "fresh", "access": "write", "catalogs": ["main"],
                          "default_catalog": "main", "actor": "agent/fresh"});
        let (status, created) = send(&test, "POST", "/api/v1/keys", Some(&keys.admin), Some(body)).await;
        assert_eq!(status.as_u16(), 200, "{created}");
        let secret = created["secret"].as_str().expect("a secret").to_string();
        assert!(secret.starts_with(crate::keys::KEY_PREFIX));
        assert_eq!(created["key"]["name"], "fresh");
        assert_eq!(created["key"]["actor"], "agent/fresh");
        assert_eq!(created["key"]["access"], "write");
        assert_eq!(created["key"]["catalogs"], json!(["main"]));
        assert!(created["key"].get("sha256").is_none());

        // The poll after the change means the key works on the next request.
        let (status, me) = send(&test, "GET", "/api/v1/whoami", Some(&secret), None).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(me["name"], "fresh");

        // The list never shows a secret or a hash.
        let (_, listed) = send(&test, "GET", "/api/v1/keys", Some(&keys.admin), None).await;
        let text = listed.to_string();
        assert!(!text.contains(&secret));
        assert!(!text.contains("sha256"));
        let names: Vec<&str> = listed["keys"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|key| key["name"].as_str())
            .collect();
        assert!(names.contains(&"fresh"), "{names:?}");
    }

    #[tokio::test]
    async fn the_actor_defaults_to_the_name() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = json!({"name": "plain", "access": "read", "catalogs": ["main"]});
        let (status, created) = send(&test, "POST", "/api/v1/keys", Some(&keys.admin), Some(body)).await;
        assert_eq!(status.as_u16(), 200, "{created}");
        assert_eq!(created["key"]["actor"], "plain");
        assert_eq!(created["key"]["default_catalog"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn create_stays_within_the_callers_reach() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = stored(&test, "root", Access::Admin, Grants::All).await;
        let post = |body: serde_json::Value, key: String| {
            let test = &test;
            async move { send(test, "POST", "/api/v1/keys", Some(&key), Some(body)).await }
        };
        // A catalog the admin does not reach looks like an unknown one.
        let (status, _) = post(json!({"name": "x1", "access": "read", "catalogs": ["other"]}),
                               keys.admin.clone()).await;
        assert_eq!(status.as_u16(), 404);
        // Only a key that reaches every catalog may make one that does.
        let (status, _) = post(json!({"name": "x2", "access": "read", "all": true}),
                               keys.admin.clone()).await;
        assert_eq!(status.as_u16(), 403);
        let (status, _) = post(json!({"name": "x3", "access": "read", "all": true}),
                               root.key.clone()).await;
        assert_eq!(status.as_u16(), 200);
        // Bad input.
        let (status, body) = post(json!({"name": "x4", "access": "owner", "catalogs": ["main"]}),
                                  keys.admin.clone()).await;
        assert_eq!(status.as_u16(), 400);
        assert_eq!(body["error"], "InvalidPath");
        let (status, _) = post(json!({"name": "bad name", "access": "read", "catalogs": ["main"]}),
                               keys.admin.clone()).await;
        assert_eq!(status.as_u16(), 409);
        let (status, _) = post(json!({"name": "x5", "access": "read", "all": true, "catalogs": ["main"]}),
                               root.key.clone()).await;
        assert_eq!(status.as_u16(), 409);
        let (status, _) = post(json!({"name": "x6", "access": "read", "catalogs": ["main"], "extra": 1}),
                               keys.admin.clone()).await;
        assert_eq!(status.as_u16(), 422, "unknown fields are refused by the Json extractor");

        // The scoped admin does not see the all-catalogs keys; root sees every key.
        let names = |listed: &serde_json::Value| -> Vec<String> {
            listed["keys"]
                .as_array()
                .expect("a list")
                .iter()
                .filter_map(|key| key["name"].as_str().map(str::to_string))
                .collect()
        };
        let (_, scoped) = send(&test, "GET", "/api/v1/keys", Some(&keys.admin), None).await;
        let scoped = names(&scoped);
        assert!(!scoped.iter().any(|name| name == "root" || name == "x3" || name == "stranger"),
                "{scoped:?}");
        let (_, everything) = send(&test, "GET", "/api/v1/keys", Some(&root.key), None).await;
        let everything = names(&everything);
        assert!(["root", "x3", "stranger", "admin"].iter().all(|name| everything.iter().any(|n| n == name)),
                "{everything:?}");
    }

    #[tokio::test]
    async fn grant_and_ungrant_stay_within_reach() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = stored(&test, "root", Access::Admin, Grants::All).await;
        let main = main_only();
        let target = stored(&test, "target", Access::Read, Grants::Catalogs(&main)).await.id;

        // The scoped admin cannot grant a catalog it does not reach.
        let uri = format!("/api/v1/keys/{target}/catalogs/other");
        let (status, _) = send(&test, "PUT", &uri, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 404);
        // Root can; then the key reaches beyond the scoped admin, who loses it.
        let (status, granted) = send(&test, "PUT", &uri, Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 200, "{granted}");
        assert_eq!(granted["catalogs"], json!(["main", "other"]));
        let main_uri = format!("/api/v1/keys/{target}/catalogs/main");
        let (status, _) = send(&test, "DELETE", &main_uri, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 404);
        // Root takes `other` away again.
        let (status, ungranted) = send(&test, "DELETE", &uri, Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(ungranted["catalogs"], json!(["main"]));
        // An unknown catalog, and an unknown key id.
        let (status, _) = send(&test, "PUT", &format!("/api/v1/keys/{target}/catalogs/nope"),
                               Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 404);
        let (status, _) = send(&test, "PUT", "/api/v1/keys/no-such-id/catalogs/main",
                               Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 404);
        // A name is not an id: `{id}` never falls back to name resolution.
        let (status, _) = send(&test, "POST", "/api/v1/keys/target/revoke", Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 404);
    }

    #[tokio::test]
    async fn reporting_and_revoke_take_effect_at_once() {
        let test = app().await;
        let keys = keys(&test).await;
        let main = main_only();
        let target = stored(&test, "target", Access::Read, Grants::Catalogs(&main)).await;

        let (status, patched) = send(&test, "PATCH", &format!("/api/v1/keys/{}", target.id),
                                     Some(&keys.admin), Some(json!({"maintenance_report": true}))).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(patched["maintenance_report"], true);
        let (_, me) = send(&test, "GET", "/api/v1/whoami", Some(&target.key), None).await;
        assert_eq!(me["maintenance_report"], true);

        let revoke = format!("/api/v1/keys/{}/revoke", target.id);
        let (status, revoked) = send(&test, "POST", &revoke, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 200);
        assert!(revoked["revoked_at"].is_string());
        let (status, _) = send(&test, "GET", "/api/v1/whoami", Some(&target.key), None).await;
        assert_eq!(status.as_u16(), 401, "the revoked key is refused on its next request");
        let (status, _) = send(&test, "POST", &revoke, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 409, "already revoked");
    }
}
```

- [ ] **Step 2: Run the tests to make sure they fail**

Run: `cargo test --lib http::api::keys -- --test-threads=6 2>&1 | tail -20`
Expected: FAIL. The stub has no routes, so the first request of every test is a 404: `GET /api/v1/keys as read: ... left: 404, right: 403`, `create_returns_the_secret_once...` with `left: 404, right: 200`, and so on.

- [ ] **Step 3: Implement the key endpoints**

`src/http/api/keys.rs`, above the test module (replacing the stub):

```rust
//! `/api/v1/keys` (web console design, "API": Keys): what `okf-mcp key ...`
//! does, over HTTP. The handlers call the same `store::keys` functions the
//! CLI calls, always by immutable key id.
//!
//! Access (M9 Task 2, "Security decisions"):
//! - every route needs an admin key;
//! - an admin manages only keys inside its reach: all of them if it reaches
//!   every catalog, else the keys that are not all-catalogs keys and whose
//!   every catalog it reaches. Any other key is a 404, like an unknown id;
//! - a new key, or a grant, may name only catalogs the caller reaches, and
//!   `all: true` needs a caller that reaches every catalog;
//! - no response but `POST /keys` holds a secret, and none holds a hash.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, patch, post, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::require_admin;
use crate::http::AppError;
use crate::store::keys::{self as key_rows, Access, Grants, KeyRecord, NewKey};

/// One key as the API shows it: never its secret or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct KeyView {
    /// Its immutable id (the `{id}` of the other routes).
    pub id: String,
    /// Its grouping name.
    pub name: String,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// `read`, `write` or `admin`.
    pub access: &'static str,
    /// Whether it may submit maintenance reports.
    pub maintenance_report: bool,
    /// Granted every catalog, present and future.
    pub all_catalogs: bool,
    /// Granted catalogs, sorted (empty with `all_catalogs`).
    pub catalogs: Vec<String>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
    /// When it was created (`2026-11-09T12:00:00Z`).
    pub created_at: String,
    /// When it was revoked, if it was.
    pub revoked_at: Option<String>,
}

impl From<KeyRecord> for KeyView {
    fn from(record: KeyRecord) -> Self {
        // Destructuring names every field: `sha256: _` drops the hash on
        // purpose, and a field added to `KeyRecord` later fails to compile
        // here until someone decides whether the API may show it.
        let KeyRecord {
            id,
            name,
            sha256: _,
            access,
            maintenance_report,
            all_catalogs,
            catalogs,
            default_catalog,
            actor,
            created_at,
            revoked_at,
        } = record;
        Self {
            id,
            name,
            actor,
            access: access.as_str(),
            maintenance_report,
            all_catalogs,
            catalogs,
            default_catalog,
            created_at: format_timestamp(created_at),
            // `Option::map` with a function name applies it to the value, if any.
            revoked_at: revoked_at.map(format_timestamp),
        }
    }
}

/// `GET /keys`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct KeyList {
    /// The keys the caller manages, revoked ones included, in the store's
    /// order (name, actor, creation).
    pub keys: Vec<KeyView>,
}

/// `POST /keys`' answer: the new key and, this once, its secret.
// `Debug` is written by hand (not derived) so that a stray `{:?}` in a log
// line can never print the secret.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CreatedKeyView {
    /// The new key.
    pub key: KeyView,
    /// The bearer secret. It is not stored and cannot be shown again.
    pub secret: String,
}

impl std::fmt::Debug for CreatedKeyView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedKeyView")
            .field("key", &self.key)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// `POST /keys`' body. `deny_unknown_fields` turns a misspelt field into a
/// 422 instead of silently ignoring it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKeyBody {
    /// Grouping name, `[A-Za-z0-9._-]+`.
    name: String,
    /// `read`, `write` or `admin`. A string, because `Access` has no serde
    /// derive; parsed with `Access::parse`.
    access: String,
    /// Grant every catalog, present and future.
    #[serde(default)]
    all: bool,
    /// The catalogs to grant (without `all`).
    #[serde(default)]
    catalogs: Vec<String>,
    /// The default catalog (empty or absent: none).
    default_catalog: Option<String>,
    /// The actor (empty or absent: the name, as the CLI does).
    actor: Option<String>,
    /// May submit maintenance reports.
    #[serde(default)]
    maintenance_report: bool,
}

/// `PATCH /keys/{id}`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateKeyBody {
    /// The new maintenance-report flag.
    maintenance_report: bool,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{id}", patch(update_key))
        .route("/keys/{id}/revoke", post(revoke_key))
        .route("/keys/{id}/catalogs/{catalog}", put(grant_catalog).delete(ungrant_catalog))
}

/// Whether `principal` may see and change `record` (module doc, "Access").
///
/// # Returns
/// `true` for a caller reaching every catalog; otherwise `true` only for a
/// key that is not an all-catalogs key and whose every grant the caller reaches.
fn manages(principal: &Principal, record: &KeyRecord) -> bool {
    match &principal.reach {
        Reach::All => true,
        Reach::Catalogs(_) => {
            !record.all_catalogs && record.catalogs.iter().all(|catalog| principal.reaches(catalog))
        }
    }
}

/// The key with exactly this id, if `principal` is an admin that manages it.
///
/// # Errors
/// [`OkfError::Forbidden`] below admin; [`OkfError::NotFound`] for an
/// unknown id or a key the caller does not manage (worded alike);
/// [`OkfError::Database`].
async fn managed(app: &App, principal: &Principal, id: &str) -> Result<KeyRecord, OkfError> {
    require_admin(principal)?;
    key_rows::list(app.pool())
        .await?
        .into_iter()
        .find(|record| record.id == id && manages(principal, record))
        .ok_or_else(|| OkfError::NotFound { path: format!("keys/{id}") })
}

/// Reloads the key cache at once (plan Decision 12), then answers with the
/// key as it now is. The change is already committed, so a failed reload is
/// logged, not returned: the background poll retries within `poll_interval`.
///
/// # Errors
/// As [`managed`].
async fn changed(app: &App, principal: &Principal, id: &str) -> Result<Json<KeyView>, AppError> {
    if let Err(error) = app.poll().await {
        tracing::error!(%error, "reloading after a key change failed; the next poll retries");
    }
    Ok(Json(managed(app, principal, id).await?.into()))
}

/// `GET /keys`: the keys the caller manages.
///
/// # Errors
/// 403 below admin; 500 if the keys cannot be read.
async fn list_keys(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<KeyList>, AppError> {
    require_admin(&principal)?;
    let keys = key_rows::list(app.pool())
        .await?
        .into_iter()
        .filter(|record| manages(&principal, record))
        .map(KeyView::from)
        .collect();
    Ok(Json(KeyList { keys }))
}

/// `POST /keys`: creates a key and returns its secret, once.
///
/// # Errors
/// 403 below admin, or `all: true` from a caller that does not reach every
/// catalog; 404 for a catalog the caller does not reach (or that does not
/// exist); 400 for an unknown `access`; 409 for a bad or taken name, no
/// catalogs, `all` together with `catalogs`, or an ungranted default.
async fn create_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateKeyBody>,
) -> Result<Json<CreatedKeyView>, AppError> {
    require_admin(&principal)?;
    let access = Access::parse(&body.access).ok_or_else(|| OkfError::InvalidPath {
        path: "access".to_string(),
        reason: format!("must be read, write or admin, not {:?}", body.access),
    })?;
    let grants = if body.all {
        if !body.catalogs.is_empty() {
            return Err(OkfError::Refused {
                reason: "give `all` or `catalogs`, not both".to_string(),
            }
            .into());
        }
        if !matches!(principal.reach, Reach::All) {
            return Err(OkfError::Forbidden {
                reason: "only a key that reaches every catalog may create one that does".to_string(),
            }
            .into());
        }
        Grants::All
    } else {
        for catalog in &body.catalogs {
            principal.require(Access::Admin, catalog)?;
        }
        Grants::Catalogs(&body.catalogs)
    };
    // `filter` turns `Some("")` (an empty form field) into `None`.
    let default_catalog = body.default_catalog.as_deref().filter(|name| !name.is_empty());
    if let Some(name) = default_catalog {
        principal.require(Access::Admin, name)?;
    }
    let actor = body
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
        .unwrap_or(&body.name);
    let new = NewKey {
        name: &body.name,
        access,
        maintenance_report: body.maintenance_report,
        grants,
        default_catalog,
        actor,
    };
    let created = key_rows::create_with_id(app.pool(), &new, app.clock().now()).await?;
    let Json(key) = changed(&app, &principal, &created.id).await?;
    Ok(Json(CreatedKeyView { key, secret: created.key }))
}

/// `PATCH /keys/{id}`: sets the maintenance-report flag.
///
/// # Errors
/// 403 below admin; 404 for a key the caller does not manage; 409 for a
/// revoked key.
async fn update_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateKeyBody>,
) -> Result<Json<KeyView>, AppError> {
    let target = managed(&app, &principal, &id).await?;
    key_rows::set_maintenance_report_target(app.pool(), &target.id, None, body.maintenance_report)
        .await?;
    changed(&app, &principal, &target.id).await
}

/// `POST /keys/{id}/revoke`: the key authenticates nothing from its next
/// request on (the handler reloads the key cache).
///
/// # Errors
/// 403 below admin; 404 for a key the caller does not manage; 409 if it is
/// already revoked.
async fn revoke_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<KeyView>, AppError> {
    let target = managed(&app, &principal, &id).await?;
    key_rows::revoke_target(app.pool(), &target.id, None, app.clock().now()).await?;
    changed(&app, &principal, &target.id).await
}

/// `PUT /keys/{id}/catalogs/{catalog}`: grants a catalog (already granted:
/// no change).
///
/// # Errors
/// 403 below admin; 404 for a catalog the caller does not reach or a key it
/// does not manage; 409 for a revoked or all-catalogs key.
async fn grant_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    // Two path parameters arrive as a tuple, in the order of the pattern.
    Path((id, catalog)): Path<(String, String)>,
) -> Result<Json<KeyView>, AppError> {
    require_admin(&principal)?;
    principal.require(Access::Admin, &catalog)?;
    let target = managed(&app, &principal, &id).await?;
    key_rows::grant_target(app.pool(), &target.id, None, &catalog).await?;
    changed(&app, &principal, &target.id).await
}

/// `DELETE /keys/{id}/catalogs/{catalog}`: removes a grant, and the default
/// if it was that catalog (not granted: no change).
///
/// # Errors
/// As [`grant_catalog`].
async fn ungrant_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((id, catalog)): Path<(String, String)>,
) -> Result<Json<KeyView>, AppError> {
    require_admin(&principal)?;
    principal.require(Access::Admin, &catalog)?;
    let target = managed(&app, &principal, &id).await?;
    key_rows::ungrant_target(app.pool(), &target.id, None, &catalog).await?;
    changed(&app, &principal, &target.id).await
}
```

> **Corrected (issue #217, 2026-11-10).** Security decision 4 does not hold as written. For a row from before migration 0005 (`id == name`), `resolve_active` resolves the id by active name, so a `*_target` call with `&target.id` could revoke, re-flag or re-grant another live key of that name. That includes a replacement created between the handler's `managed` lookup and the store's transaction, which the caller may not manage (review of PR #226). `store::keys` therefore gains `set_maintenance_report_by_id(pool, id, enabled)`, `revoke_by_id(pool, id, now)`, `grant_by_id(pool, id, catalog)` and `ungrant_by_id(pool, id, catalog)`. They share their bodies with the `*_target` functions through a private `Selector` and lock exactly that row (`SELECT ... FOR UPDATE`), never falling back to a name. The handlers call these instead of `*_target(.., None, ..)`, and the revoked checks run on that row as before. Tests: `a_legacy_id_changes_only_its_own_key` (API) and `store::keys::tests::by_id_changes_never_reach_a_legacy_keys_replacement` (the lookup, rotate, mutate interleaving). Task 2 therefore also modifies `src/store/keys.rs`.

> `&app` is a `&Arc<App>` and `&principal` a `&Arc<Principal>`; Rust's deref coercion passes them where `&App` and `&Principal` are expected, like Python duck typing but checked at compile time.

- [ ] **Step 4: Run the tests to make sure they pass**

Run: `cargo test --lib http::api::keys -- --test-threads=6`
Expected: PASS: `key_routes_need_an_admin_that_reaches_the_key`, `create_returns_the_secret_once_and_the_new_key_works`, `the_actor_defaults_to_the_name`, `create_stays_within_the_callers_reach`, `grant_and_ungrant_stay_within_reach`, `reporting_and_revoke_take_effect_at_once`.

- [ ] **Step 5: The API keys page**

`src/http/ui/js/pages/keys.js`:

```js
// API keys (admin): the keys this admin manages, a form to create one (its
// secret shown once), and per-key grant, ungrant, reporting and revoke.
// The server enforces every rule; this page only hides what cannot work.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const LEVELS = ['read', 'write', 'admin'];
const ACCESS_KIND = { read: 'muted', write: 'info', admin: 'warn' };

function catalogsOf(key) {
    if (key.all_catalogs) return pill('all', 'info');
    return key.catalogs.length ? key.catalogs.join(', ') : '-';
}

function isMine(key) {
    return key.id === store.whoami?.key_id;
}

const COLUMNS = [
    { key: 'name', label: 'Name', sort: key => key.name,
      render: key => h('span', { class: 'key-name', 'data-test': `key-${key.name}` }, key.name, isMine(key) ? [' ', pill('you', 'info')] : null) },
    { key: 'actor', label: 'Actor', class: 'mono', sort: key => key.actor },
    { key: 'access', label: 'Access', sort: key => LEVELS.indexOf(key.access),
      render: key => pill(key.access, ACCESS_KIND[key.access]) },
    { key: 'catalogs', label: 'Catalogs', render: catalogsOf },
    { key: 'default_catalog', label: 'Default', class: 'mono' },
    { key: 'maintenance_report', label: 'Reporting',
      render: key => key.maintenance_report ? pill('reports', 'ok') : '-' },
    { key: 'created_at', label: 'Created', sort: key => key.created_at, render: key => when(key.created_at) },
    { key: 'revoked_at', label: 'State', sort: key => key.revoked_at ?? '',
      render: key => key.revoked_at
          ? [pill('revoked', 'error'), ' ', h('span', { class: 'muted small' }, when(key.revoked_at))]
          : pill('active', 'ok') },
];

export async function render(main) {
    const head = h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'API keys'),
            h('p', { class: 'muted' }, 'The keys inside your reach. A secret is shown once, when its key is created.')));
    main.append(head);
    if (!store.can('admin')) {
        main.append(h('p', { class: 'muted', id: 'keys-denied' }, 'Managing keys needs an admin key.'));
        return;
    }
    const showRevoked = h('input', { type: 'checkbox', id: 'keys-show-revoked' });
    const table = listTable(COLUMNS, [], { onRow: key => showKey(key, load), empty: 'No keys.' });
    table.id = 'keys-table';
    const applyFilter = () => table.setFilter(showRevoked.checked ? null : key => !key.revoked_at);
    showRevoked.addEventListener('change', applyFilter);
    head.append(h('div', { class: 'actions' },
        h('button', { type: 'button', id: 'key-new', onclick: () => newKey(load) }, 'New key')));
    main.append(h('label', { class: 'small muted' }, showRevoked, ' Show revoked keys'), table);

    async function load() {
        try {
            table.redraw((await api.get('/keys')).keys);
        } catch { /* toasted; a 401 has already signed the browser out */ }
    }
    applyFilter();
    await load();
}

// The create form, in the drawer.
function newKey(reload) {
    const me = store.whoami;
    const form = h('form', { id: 'key-form' },
        h('label', {}, 'Name', h('input', { name: 'name', required: true, autocomplete: 'off', placeholder: 'letters, digits, . _ -' })),
        h('label', {}, 'Actor', h('input', { name: 'actor', autocomplete: 'off', placeholder: 'defaults to the name' })),
        h('label', {}, 'Access', h('select', { name: 'access' }, LEVELS.map(level => h('option', { value: level }, level)))),
        h('fieldset', {},
            h('legend', {}, 'Catalogs'),
            me.all_catalogs
                ? h('label', {}, h('input', { type: 'checkbox', name: 'all' }), ' Every catalog, present and future')
                : null,
            me.catalogs.map(name => h('label', {},
                h('input', { type: 'checkbox', name: 'catalog', value: name }), ' ', h('span', { class: 'mono' }, name)))),
        h('label', {}, 'Default catalog',
            h('select', { name: 'default_catalog' },
                h('option', { value: '' }, 'None'),
                me.catalogs.map(name => h('option', { value: name }, name)))),
        h('label', {},
            h('input', { type: 'checkbox', name: 'maintenance_report', role: 'switch' }), ' May submit maintenance reports'),
        h('button', { type: 'submit' }, 'Create key'));
    form.addEventListener('submit', async event => {
        event.preventDefault();
        const data = new FormData(form);
        const all = data.get('all') === 'on';
        const body = {
            name: String(data.get('name') ?? '').trim(),
            access: String(data.get('access') ?? 'read'),
            all,
            catalogs: all ? [] : data.getAll('catalog').map(String),
            default_catalog: String(data.get('default_catalog') ?? '') || null,
            actor: String(data.get('actor') ?? '').trim() || null,
            maintenance_report: data.get('maintenance_report') === 'on',
        };
        try {
            const created = await api.post('/keys', body);
            closeDrawer();
            showSecret(created);
            await reload();
        } catch { /* the server's reason is toasted; the form stays filled */ }
    });
    openDrawer('New key', 'The secret is shown once, right after creation.', form);
}

// The one time a secret is on screen: a modal that only the explicit
// button closes, after which the secret is removed from the page.
function showSecret(created) {
    const code = h('code', { id: 'key-secret' }, created.secret);
    const copy = h('button', { type: 'button', id: 'key-secret-copy', class: 'secondary' }, 'Copy');
    const done = h('button', { type: 'button', id: 'key-secret-done' }, 'I have stored it');
    const dialog = h('dialog', { id: 'key-secret-dialog', class: 'confirm' },
        h('article', {},
            h('h3', {}, `Key ${created.key.name} created`),
            h('p', { class: 'warn-text' }, 'Copy this key now. It is not stored anywhere and cannot be shown again.'),
            h('pre', { class: 'secret' }, code),
            h('footer', {}, copy, done)));
    copy.addEventListener('click', async () => {
        try {
            // navigator.clipboard exists only in secure contexts (https or localhost).
            await navigator.clipboard.writeText(created.secret);
            toast('Copied to the clipboard.', 'ok');
        } catch {
            toast('Copying failed: select the key and copy it by hand.', 'error');
        }
    });
    done.addEventListener('click', () => {
        code.textContent = '';
        dialog.close();
        dialog.remove();
    });
    dialog.addEventListener('cancel', event => event.preventDefault()); // Escape does not close it
    document.body.append(dialog);
    dialog.showModal();
}

function fact(label, value) {
    return [h('dt', {}, label), h('dd', {}, value)];
}

// One key's drawer: its facts, then grant, ungrant, reporting and revoke.
function showKey(key, reload) {
    const me = store.whoami;
    const mine = isMine(key);
    const facts = h('dl', { class: 'facts' },
        fact('Id', h('code', {}, key.id)),
        fact('Actor', h('code', {}, key.actor)),
        fact('Access', pill(key.access, ACCESS_KIND[key.access])),
        fact('Catalogs', catalogsOf(key)),
        fact('Default', key.default_catalog ?? '-'),
        fact('Reporting', key.maintenance_report ? 'may submit maintenance reports' : 'no'),
        fact('Created', when(key.created_at)),
        fact('Revoked', key.revoked_at ? when(key.revoked_at) : 'no'));
    const subtitle = mine ? 'The key this browser is signed in with' : `${key.access} key`;
    if (key.revoked_at) {
        openDrawer(key.name, subtitle, facts,
            h('p', { class: 'muted' }, 'This key is revoked: it authenticates nothing and cannot be changed.'));
        return;
    }
    const change = async (request, message) => {
        try {
            const updated = await request();
            toast(message, 'ok');
            await reload();
            showKey(updated, reload);
        } catch { /* toasted */ }
    };
    const base = `/keys/${enc(key.id)}`;
    let grants;
    if (key.all_catalogs) {
        grants = h('p', { class: 'muted small' }, 'Granted every catalog: per-catalog grants do not apply.');
    } else {
        const grantable = me.catalogs.filter(name => !key.catalogs.includes(name));
        const select = h('select', { id: 'key-grant-select', 'aria-label': 'Catalog to grant' },
            grantable.map(name => h('option', { value: name }, name)));
        grants = h('section', {},
            h('h3', {}, 'Catalogs'),
            h('ul', { class: 'grant-list' }, key.catalogs.length
                ? key.catalogs.map(name => h('li', {},
                    h('span', { class: 'mono' }, name), ' ',
                    h('button', {
                        type: 'button', class: 'text-btn', dataset: { ungrant: name },
                        onclick: () => change(() => api.del(`${base}/catalogs/${enc(name)}`), `Removed ${name} from ${key.name}.`),
                    }, 'Remove')))
                : h('li', { class: 'muted' }, 'No catalogs: this key reaches nothing.')),
            grantable.length
                ? h('div', { class: 'inline-form' }, select,
                    h('button', {
                        type: 'button', id: 'key-grant',
                        onclick: () => change(() => api.put(`${base}/catalogs/${enc(select.value)}`), `Granted ${select.value} to ${key.name}.`),
                    }, 'Grant'))
                : null);
    }
    const report = h('button', {
        type: 'button', id: 'key-report-toggle', class: 'secondary',
        onclick: () => change(() => api.patch(base, { maintenance_report: !key.maintenance_report }),
            key.maintenance_report ? `${key.name} may no longer report.` : `${key.name} may now report.`),
    }, key.maintenance_report ? 'Disable reporting' : 'Enable reporting');
    const revoke = h('button', {
        type: 'button', id: 'key-revoke', class: 'danger',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Revoke ${key.name}?`,
                message: mine
                    ? 'This is the key this browser is signed in with: revoking it signs you out at once. This cannot be undone.'
                    : 'Anything using this key is refused from its next request. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try {
                await api.post(`${base}/revoke`);
                closeDrawer();
                toast(`Revoked ${key.name}.`, 'ok');
                await reload(); // for the signed-in key this is a 401: back to the login card
            } catch { /* toasted */ }
        },
    }, 'Revoke');
    openDrawer(key.name, subtitle, facts, grants, h('div', { class: 'drawer-actions' }, report, revoke));
}
```

Element ids and `data-test` attributes the journeys use: `[data-test=key-<name>]` (name cell), `#key-new`, `#key-form` (inputs `name`, `actor`, `access`, `catalog`, `all`, `default_catalog`, `maintenance_report`), `#key-secret`, `#key-secret-copy`, `#key-secret-done`, `#key-revoke`, `#key-report-toggle`, `#key-grant-select`, `#key-grant`, `[data-ungrant=<catalog>]`, `#keys-table`, `#keys-show-revoked`, `#keys-denied`, and Task 1's `#confirm-ok`.

Append to `src/http/ui/styles.css`:

```css
/* API keys (Task 2): drawer facts, grant list, the one-time secret */
/* One line box: a wrapped name leaves a gap at its centre, where a click
   lands on the cell instead of the name. */
.key-name { white-space: nowrap; }
dl.facts { display: grid; grid-template-columns: max-content 1fr; gap: .35rem 1rem; margin: 0 0 1rem; }
dl.facts dt { color: var(--fg-muted); font-size: .8rem; }
dl.facts dd { margin: 0; }
.grant-list { list-style: none; padding: 0; margin: 0 0 .75rem; }
.grant-list li { display: flex; gap: .5rem; align-items: center; padding: .15rem 0; }
.inline-form { display: flex; gap: .5rem; align-items: center; }
.inline-form select, .inline-form button { width: auto; margin: 0; }
pre.secret { white-space: pre-wrap; word-break: break-all; }
.warn-text { color: var(--warn); font-weight: 600; }
```

> **Corrected (issue #217, 2026-11-10).** Two changes to this step as first planned. The block no longer carries `.drawer-actions`: Task 1's `styles.css` already has it (with `flex-wrap`), so a second copy only overrode it. The name cell's span has `class: 'key-name'` and `.key-name { white-space: nowrap; }`: at the journeys' 800 px viewport the eight-column table wrapped `console-self you` onto two lines, the centre of the span fell between them on the bare cell, and rustwright refused the click with `Actionability(NotReceivingEvents)`.

- [ ] **Step 6: Browser journeys (failing first)**

`tests/ui_e2e/keys.rs`:

```rust
//! The API keys page: create a key and see its secret once, sign in with
//! it, revoke the signed-in key, and what a read key sees.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

async fn server() -> TestServer {
    TestServer::start(vec![("main", files(&[("index.md", "# Main\n")]))], Config::default()).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_creates_a_key_sees_its_secret_once_and_signs_in_with_it() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        ui.wait_visible("[data-test=key-console-admin]");
        ui.click("#key-new");
        ui.fill("#key-form input[name=name]", "console-made");
        ui.click("#key-form input[name=catalog][value=main]");
        ui.click("#key-form button[type=submit]");
        ui.wait_visible("#key-secret");
        let secret = ui.text("#key-secret");
        assert!(secret.starts_with("okf_"), "a key, not {secret:?}");
        ui.click("#key-secret-done");
        ui.wait_hidden("#key-secret");
        ui.wait_visible("[data-test=key-console-made]");
        // Once dismissed, the secret is nowhere on the page.
        let page = ui.eval("() => document.body.textContent", serde_json::json!(null));
        assert!(!page.as_str().unwrap_or_default().contains(&secret));
        ui.click("#signout");
        ui.wait_visible("#login");
        ui.login(&secret);
        ui.wait_text("#whoami", "console-made");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_the_signed_in_key_returns_the_browser_to_login() {
    let server = server().await;
    let key = server.key("console-self", Access::Admin, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/keys");
        ui.click("[data-test=key-console-self]");
        ui.wait_visible("#key-revoke");
        ui.click("#key-revoke");
        ui.click("#confirm-ok");
        // The list reload after the revoke is the key's next request: a 401.
        ui.wait_visible("#login");
        ui.wait_text("#login-error", "refused");
        ui.open("");
        ui.wait_visible("#login");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_key_never_sees_the_keys_page() {
    let server = server().await;
    let key = server.key("console-reader", Access::Read, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "read");
        assert!(!ui.page.is_visible("a[data-nav=keys]").expect("visible"));
        // Typed in by hand, the page explains instead of calling the API.
        ui.open("/keys");
        ui.wait_text("#main", "needs an admin key");
    })
    .await;
}
```

`tests/ui_e2e/main.rs`: add `mod keys;` after `mod login;`.

Run: `cargo test --test ui_e2e keys` before `keys.js` exists.
Expected: FAIL. With no `js/pages/keys.js` in the build, `/keys` shows "Not available yet", so the first journey times out on `[data-test=key-console-admin]` and the third on "needs an admin key".

- [ ] **Step 7: Run the journeys**

Run: `just test-ui`
Expected: PASS, including `an_admin_creates_a_key_sees_its_secret_once_and_signs_in_with_it`, `revoking_the_signed_in_key_returns_the_browser_to_login` and `a_read_key_never_sees_the_keys_page`, with no skip lines.

- [ ] **Step 8: Docs and changelog**

> **Corrected (issue #217, 2026-11-10).** The rows below went into the "API keys" subsection's own table, in Task 1's column layout (`Method and path | Key | Does`), not into the shared "API reference" table, so parallel tasks do not edit the same table. The example uses `okf-mcp.example.com`, as Task 1's does.

`docs/operations.md`, section "Web console": add subsection "API keys", saying that the page and its API are for admin keys only; that an admin key manages only the keys inside its reach (a key that reaches every catalog manages every key; one scoped to catalogs manages the keys whose every catalog it reaches, and no all-catalogs key); that a new key or a grant may name only catalogs the caller reaches, and `all` needs an all-catalogs caller; that the secret is in the `POST` response only; and that a revoke or grant takes effect on the next request. API reference rows:

| Method | Path | Access | Notes |
|---|---|---|---|
| GET | `/api/v1/keys` | admin | The keys the caller manages, revoked ones included: id, name, actor, access, reporting flag, catalogs or `all_catalogs`, default, created, revoked. Never a secret or hash. |
| POST | `/api/v1/keys` | admin | Body `{name, access: "read"\|"write"\|"admin", all, catalogs, default_catalog, actor, maintenance_report}` (`actor` defaults to `name`). Answers `{key, secret}`; the secret is shown only here. 400 bad `access`, 403 `all` from a scoped caller, 404 unreached catalog, 409 bad or taken name. |
| POST | `/api/v1/keys/{id}/revoke` | admin | By key id. Refused from the key's next request. 409 if already revoked. |
| PUT | `/api/v1/keys/{id}/catalogs/{catalog}` | admin | Grant. 404 for an unreached catalog or unmanaged key; 409 for a revoked or all-catalogs key. |
| DELETE | `/api/v1/keys/{id}/catalogs/{catalog}` | admin | Ungrant; clears the default if it was that catalog. |
| PATCH | `/api/v1/keys/{id}` | admin | Body `{maintenance_report: bool}`. |

with one example:

```bash
curl -fsS -H "Authorization: Bearer $OKF_MCP_KEY" -H 'Content-Type: application/json' \
    -d '{"name":"ci-reader","access":"read","catalogs":["main"],"default_catalog":"main"}' \
    https://okf.example.com/api/v1/keys
```

`README.md`, section "Web console": one sentence that admins manage API keys at `/ui/#/keys` (or `/api/v1/keys`) without a shell on the server.

`changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m9-task-02-api-keys.md`: Added (the six endpoints, the page, the journeys), Changed (none), Notes (the reach rule for scoped admins; key ids only in paths; secret only in the create response).

- [ ] **Step 9: Gates and commit**

Run every gate in "Conventions". Then:

```bash
git add src/http/api/keys.rs src/http/ui/js/pages/keys.js src/http/ui/styles.css \
    tests/ui_e2e/keys.rs tests/ui_e2e/main.rs README.md docs/operations.md changelog/
git commit -m "feat(console): API key endpoints and page (M9 task 2)"
```

---

### Task 3: Health dashboards

Depends on Task 1.

**Facts this task relies on** (checked at `62e88cd`):
- `src/mcp/reports.rs`: `CatalogParam { catalog, changeset }` (38), `OldestParams { catalog, changeset, n, open_questions_only }` (76), `SprawlParams { catalog, changeset, path, n }` (101), `LinksParams { catalog, changeset, mentions }` (124), `LintParams { catalog, changeset }` (146). Results: `OpenQuestions { catalog, concepts: Vec<ConceptQuestions { path, title, questions: Vec<Question { number, text }> }> }` (47-72), `Oldest { catalog, rows: Vec<OldestRow> }` (93), `Sprawl { catalog, reports: Vec<SprawlReport> }` (116), `Links { catalog, #[serde(flatten)] report: LinksReport }` (137), `Lint { catalog, pending, #[serde(flatten)] report: LintReport }` (156). Functions: `open_questions` (177), `oldest_report` (224), `sprawl_report` (255), `links` (289), `lint_report` (313). The tools' own default `n` is 5 (32); `mentions` defaults to true (292).
- Access: each report resolves its catalog through `ToolContext::one` -> `selection::resolve`, which answers `CatalogNotSelected` (404) for a catalog that does not exist or that the key does not reach (`src/selection.rs:155-159`). Any key level may read reports.
- `LintReport { errors: Vec<Finding>, warnings: Vec<Finding> }` (`src/okf/analysis/lint.rs:100-105`); `Finding { file, line, rule, message }`, `file` without a leading `/` (`src/okf/analysis/mod.rs:21-31`). A concept above `SPRAWL_WARN` (16.0, `src/okf/analysis/sprawl.rs:37`) that is not exempt gets one warning with rule `sprawl` (`lint.rs:242-255`), so "sprawling concepts" is the count of those warnings. A concept with frontmatter but no `type` is a `no-type` error (`lint.rs:270`); broken links are `broken-link` errors (`lint.rs:471-475`).
- `LinksReport { broken, broken_anchors, relative: Vec<LinkIssue { file, line, link }>, orphans, dead_ends: Vec<String>, index_drift: Vec<IndexDrift { file, problem }>, unlinked_mentions: Vec<UnlinkedMention { file, target, hits }>, summary: LinksSummary { concepts, broken, broken_anchors, relative, orphans, dead_ends, index_drift, unlinked_mentions } }` (`src/okf/analysis/links.rs:32-111`). Paths have no leading `/` (`doc.path()`, 140, 176). An orphan is a concept with no inbound link from another *concept*; an `index.md` link does not count (162-177). `mentions` only gates `unlinked_mentions` (186-188), so the counts can skip it.
- `OldestRow { path ("/"-prefixed), title, status, last_touched: Option<String>, last_touched_by: Option<&'static str>, age_days: Option<f64>, stale, open_questions: Vec<String> }`, `Serialize` and `PartialEq` only (f64) (`src/okf/analysis/oldest.rs:41-64`). `last_touched` is the newest of `generated`/`verified`/`reviewed`, so the tile shows the age of the stalest concept's newest stamp, not of `generated` alone.
- `SprawlReport.path` is `/`-prefixed (`sprawl.rs:73-75`).
- `OkfError::NotFound { path }` renders as `"{path}: no such concept or document"` and maps to 404 (`src/error.rs:15-19`, `src/http/mod.rs:84-86`).

**Unknown report name.** `GET .../reports/<anything else>` answers 404 `NotFound` (path `reports/<name>`), not 400 `InvalidPath`: the report name is a resource in the URL, an unknown one is a missing resource like any unmatched route, and `InvalidPath` means a malformed catalog path. The name is checked before any report runs, so the answer is the same for every caller.

**Files:**
- Modify: `src/http/api/health.rs` (replaces Task 1's stub)
- Create: `src/http/ui/js/pages/health.js`
- Create: `tests/ui_e2e/health.rs`
- Modify: `tests/ui_e2e/main.rs` (`mod health;`)
- Modify: `docs/operations.md` ("Web console": subsection "Health"), `README.md` (one sentence)
- Create: `changelog/<ts>-m9-task-03-health.md`

- [ ] **Step 1: Write the failing API tests**

At the bottom of `src/http/api/health.rs` (below Task 1's stub for now):

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::config::Config;
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::mcp::testing::{app_with, TestApp};

    /// A `main` with known problems: `gamma` has no `type` (a lint error) and
    /// nothing links to it (an orphan); `alpha` links to a missing `gone.md`
    /// (a broken link); `beta` queues one open question.
    const SICK: &[(&str, &str)] = &[
        ("index.md", "# Main\n\n- [Alpha](systems/alpha.md): the first system\n"),
        (
            "systems/alpha.md",
            "---\ntype: system\ntitle: Alpha\ndescription: The first system.\n---\n# Alpha\n\n\
             Alpha runs [Beta](beta.md) and [Gone](gone.md).\n",
        ),
        (
            "systems/beta.md",
            "---\ntype: system\ntitle: Beta\ndescription: The second system.\n\
             open_questions:\n  - Who owns Beta?\n---\n# Beta\n\nBeta serves [Alpha](alpha.md).\n",
        ),
        ("systems/gamma.md", "---\ntitle: Gamma\n---\n# Gamma\n\nNothing links here.\n"),
    ];

    async fn sick() -> TestApp {
        app_with(&[("main", SICK), ("other", &[("index.md", "# Other\n")])], Config::default())
            .await
    }

    /// GETs `uri` with `key`, asserts 200 and returns the body.
    async fn get_ok(test: &TestApp, key: &str, uri: &str) -> serde_json::Value {
        let (status, body) = send(test, "GET", uri, Some(key), None).await;
        assert_eq!(status.as_u16(), 200, "GET {uri}: {body}");
        body
    }

    #[tokio::test]
    async fn health_counts_match_the_reports() {
        let test = sick().await;
        let keys = keys(&test).await;
        let base = "/api/v1/catalogs/main";
        let health = get_ok(&test, &keys.read, &format!("{base}/health")).await;
        let lint = get_ok(&test, &keys.read, &format!("{base}/reports/lint")).await;
        let links = get_ok(&test, &keys.read, &format!("{base}/reports/links")).await;
        let oldest = get_ok(&test, &keys.read, &format!("{base}/reports/oldest?n=1")).await;
        let questions = get_ok(&test, &keys.read, &format!("{base}/reports/questions")).await;

        assert_eq!(health["catalog"], "main");
        let errors = lint["errors"].as_array().expect("lint errors");
        let warnings = lint["warnings"].as_array().expect("lint warnings");
        assert!(errors
            .iter()
            .any(|finding| finding["file"] == "systems/gamma.md" && finding["rule"] == "no-type"));
        assert_eq!(health["lint_errors"], json!(errors.len()));
        assert_eq!(health["lint_warnings"], json!(warnings.len()));
        let sprawling = warnings.iter().filter(|finding| finding["rule"] == "sprawl").count();
        assert_eq!(health["sprawl"], json!(sprawling));
        assert_eq!(links["orphans"], json!(["systems/gamma.md"]));
        assert_eq!(health["orphans"], 1);
        assert_eq!(links["summary"]["broken"], 1);
        assert_eq!(health["broken_links"], 1);
        assert_eq!(health["oldest"], oldest["rows"][0]);
        assert_eq!(questions["concepts"][0]["path"], "/systems/beta.md");
        assert_eq!(health["open_questions"], 1);
    }

    #[tokio::test]
    async fn reports_are_the_tool_results_with_bounded_row_counts() {
        let test = sick().await;
        let keys = keys(&test).await;
        let base = "/api/v1/catalogs/main/reports";
        // Default 20 rows: all three concepts.
        let oldest = get_ok(&test, &keys.read, &format!("{base}/oldest")).await;
        assert_eq!(oldest["rows"].as_array().expect("rows").len(), 3);
        let sprawl = get_ok(&test, &keys.read, &format!("{base}/sprawl?n=1")).await;
        assert_eq!(sprawl["reports"].as_array().expect("reports").len(), 1);
        // `n` is clamped to 1..=200.
        let clamped = get_ok(&test, &keys.read, &format!("{base}/oldest?n=0")).await;
        assert_eq!(clamped["rows"].as_array().expect("rows").len(), 1);
        let quiet = get_ok(&test, &keys.read, &format!("{base}/links?mentions=false")).await;
        assert_eq!(quiet["summary"]["unlinked_mentions"], 0);
        let lint = get_ok(&test, &keys.read, &format!("{base}/lint")).await;
        assert_eq!(lint["pending"], false);
    }

    #[tokio::test]
    async fn bad_report_requests_are_refused() {
        let test = sick().await;
        let keys = keys(&test).await;
        let (status, body) =
            send(&test, "GET", "/api/v1/catalogs/main/reports/bogus", Some(&keys.read), None).await;
        assert_eq!(status.as_u16(), 404);
        assert_eq!(body["error"], "NotFound");
        let (status, _) =
            send(&test, "GET", "/api/v1/catalogs/main/reports/oldest?n=x", Some(&keys.read), None)
                .await;
        assert_eq!(status.as_u16(), 400);
    }

    #[tokio::test]
    async fn health_routes_need_a_key_that_reaches_the_catalog() {
        let test = app().await;
        let row = |uri: String, reached: u16, stranger: u16| Row {
            method: "GET",
            uri,
            body: None,
            read: reached,
            write: reached,
            admin: reached,
            stranger,
        };
        let mut rows = vec![
            row("/api/v1/catalogs/main/health".into(), 200, 404),
            row("/api/v1/catalogs/nope/health".into(), 404, 404),
            row("/api/v1/catalogs/main/reports/bogus".into(), 404, 404),
        ];
        rows.extend(
            ["lint", "sprawl", "links", "oldest", "questions"]
                .iter()
                .map(|name| row(format!("/api/v1/catalogs/main/reports/{name}"), 200, 404)),
        );
        assert_matrix(&test, &rows).await;
    }
}
```

- [ ] **Step 2: Run the tests to make sure they fail**

Run: `cargo test --lib http::api::health -- --test-threads=6`
Expected: FAIL. The tests compile (they only use the router), and each panics on a 404, for example `GET /api/v1/catalogs/main/health: null`, because Task 1's stub has no routes. `bad_report_requests_are_refused` fails on `body["error"]` (an unmatched route has no JSON body).

- [ ] **Step 3: Implement the health area**

`src/http/api/health.rs` (above the tests, replacing the stub):

```rust
//! `/api/v1/catalogs/{catalog}/health` and `/api/v1/catalogs/{catalog}/reports/{report}`
//! (web console design, "API": Health). The counts and the reports come from
//! the functions behind the `lint`, `sprawl`, `links_report`, `oldest` and
//! `open_questions` tools, so the console never disagrees with them.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::api;
use crate::http::AppError;
use crate::mcp::reports::{
    self as tools, CatalogParam, Lint, LintParams, Links, LinksParams, Oldest, OldestParams,
    OpenQuestions, Sprawl, SprawlParams,
};
use crate::okf::analysis::oldest::OldestRow;

/// The lint rule `lint` raises for a concept above the sprawl threshold.
const SPRAWL_RULE: &str = "sprawl";
/// Rows the `oldest` and `sprawl` reports return when `?n=` is absent.
const DEFAULT_ROWS: usize = 20;
/// The most rows `?n=` may ask for.
const MAX_ROWS: usize = 200;

/// One catalog's health in numbers, for the console's tiles.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct HealthCounts {
    /// The catalog.
    pub catalog: String,
    /// `lint` errors (they block `publish`).
    pub lint_errors: usize,
    /// `lint` warnings, sprawl included.
    pub lint_warnings: usize,
    /// Concepts above the sprawl threshold (`lint`'s `sprawl` warnings).
    pub sprawl: usize,
    /// Concepts no other concept links to.
    pub orphans: usize,
    /// Links whose target does not exist.
    pub broken_links: usize,
    /// The stalest concept's `oldest` row (`None` without concepts).
    pub oldest: Option<OldestRow>,
    /// Queued `open_questions` entries, over every concept.
    pub open_questions: usize,
}

/// One report, serialized exactly as its tool's result: `untagged` writes the
/// inner value without naming the variant (like a Python union type).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum Report {
    /// `lint`.
    Lint(Lint),
    /// `sprawl`.
    Sprawl(Sprawl),
    /// `links_report`. Boxed: it is several times larger than the other
    /// variants, and an enum is as large as its largest variant
    /// (clippy's `large_enum_variant`).
    Links(Box<Links>),
    /// `oldest`.
    Oldest(Oldest),
    /// `open_questions`.
    Questions(OpenQuestions),
}

/// `?n=` (`oldest`, `sprawl`) and `?mentions=` (`links`).
#[derive(Debug, Deserialize)]
struct ReportQuery {
    /// Rows, clamped to 1..=[`MAX_ROWS`] (default [`DEFAULT_ROWS`]).
    n: Option<usize>,
    /// Look for unlinked mentions (default true, as the tool).
    mentions: Option<bool>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{catalog}/health", get(health))
        .route("/catalogs/{catalog}/reports/{report}", get(report))
}

/// `GET /catalogs/{catalog}/health`: the tiles' numbers.
///
/// # Errors
/// 404 for a catalog that does not exist or that the key does not reach.
async fn health(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<HealthCounts>, AppError> {
    let ctx = api::context(&app, principal);
    let named = Some(catalog);
    // `try_join!` polls the four reports at once (each runs on its own
    // blocking thread) and returns the first error, like
    // `asyncio.gather` that cancels on failure. Mentions are skipped: they
    // are the slowest part and no count uses them.
    let (lint, links, oldest, questions) = tokio::try_join!(
        tools::lint_report(&ctx, LintParams { catalog: named.clone(), changeset: None }),
        tools::links(
            &ctx,
            LinksParams { catalog: named.clone(), changeset: None, mentions: Some(false) }
        ),
        tools::oldest_report(
            &ctx,
            OldestParams {
                catalog: named.clone(),
                changeset: None,
                n: Some(1),
                open_questions_only: false,
            }
        ),
        tools::open_questions(&ctx, CatalogParam { catalog: named, changeset: None }),
    )?;
    let sprawl = lint.report.warnings.iter().filter(|finding| finding.rule == SPRAWL_RULE).count();
    Ok(Json(HealthCounts {
        catalog: lint.catalog,
        lint_errors: lint.report.errors.len(),
        lint_warnings: lint.report.warnings.len(),
        sprawl,
        orphans: links.report.summary.orphans,
        broken_links: links.report.summary.broken,
        oldest: oldest.rows.into_iter().next(),
        open_questions: questions.concepts.iter().map(|concept| concept.questions.len()).sum(),
    }))
}

/// `GET /catalogs/{catalog}/reports/{report}`: one report, as its tool returns it.
///
/// # Errors
/// 404 for an unknown report name (checked first), or for a catalog that
/// does not exist or that the key does not reach; 400 for a malformed query.
async fn report(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, name)): Path<(String, String)>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Report>, AppError> {
    let ctx = api::context(&app, principal);
    let catalog = Some(catalog);
    let n = Some(query.n.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS));
    // A string has no closed set of values, so this `match` needs the `_` arm.
    let report = match name.as_str() {
        "lint" => Report::Lint(tools::lint_report(&ctx, LintParams { catalog, changeset: None }).await?),
        "sprawl" => Report::Sprawl(
            tools::sprawl_report(&ctx, SprawlParams { catalog, changeset: None, path: None, n })
                .await?,
        ),
        "links" => Report::Links(Box::new(
            tools::links(&ctx, LinksParams { catalog, changeset: None, mentions: query.mentions })
                .await?,
        )),
        "oldest" => Report::Oldest(
            tools::oldest_report(
                &ctx,
                OldestParams { catalog, changeset: None, n, open_questions_only: false },
            )
            .await?,
        ),
        "questions" => Report::Questions(
            tools::open_questions(&ctx, CatalogParam { catalog, changeset: None }).await?,
        ),
        _ => return Err(OkfError::NotFound { path: format!("reports/{name}") }.into()),
    };
    Ok(Json(report))
}
```

- [ ] **Step 4: Run the tests to make sure they pass**

Run: `cargo test --lib http::api::health -- --test-threads=6`
Expected: PASS: `health_counts_match_the_reports`, `reports_are_the_tool_results_with_bounded_row_counts`, `bad_report_requests_are_refused`, `health_routes_need_a_key_that_reaches_the_catalog`.

> **Corrected (issue #218, 2026-11-10).** A fifth test, `row_limits_default_to_20_and_clamp_to_1_through_200`, builds a `main` of 201 concepts and checks `oldest` and `sprawl` return 20 rows without `n`, 1 for `n=0`, and 200 for `n=200` and `n=201`. The three-concept fixture could not tell the default or the upper clamp apart from wrong values (review of PR #225).

- [ ] **Step 5: The Health page**

`src/http/ui/js/pages/health.js`. Ids and attributes the journey uses:
- `#health-tiles`, one `#tile-<id>` per tile, its number `#n-<id>` and detail line `#d-<id>`, for `<id>` in `lint`, `sprawl`, `orphans`, `oldest`, `questions`;
- the report switcher's buttons `.segments .seg[data-value=<report>]`;
- `#health-report`, holding the current `table#report-table` (with `data-report`);
- `#health-refresh`.

```js
// Health (the landing page): five tiles from GET /catalogs/{c}/health, then
// one report table at a time, switched by a segmented control. A row opens
// its concept in Browse. Every number is computed by the server.
import { h, clear, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { ago, count } from '../core/format.js';
import { go, browseHash } from '../core/router.js';
import { listTable } from '../components/list-table.js';
import { segments } from '../components/segments.js';

// One row per link problem, broken links first.
function linkRows(report) {
    const issue = (kind, level) => item => ({ kind, level, path: item.file, line: item.line, detail: item.link });
    return [
        ...report.broken.map(issue('broken link', 'error')),
        ...report.broken_anchors.map(issue('broken anchor', 'error')),
        ...report.relative.map(issue('relative link', 'muted')),
        ...report.orphans.map(path => ({ kind: 'orphan', level: 'warn', path, line: null, detail: 'no concept links here' })),
        ...report.dead_ends.map(path => ({ kind: 'dead end', level: 'muted', path, line: null, detail: 'links to no other concept' })),
        ...report.index_drift.map(drift => ({ kind: 'index drift', level: 'warn', path: drift.file, line: null, detail: drift.problem })),
        ...report.unlinked_mentions.map(mention => ({
            kind: 'unlinked mention', level: 'info', path: mention.file, line: mention.hits[0]?.line ?? null,
            detail: `mentions ${mention.target} ${count(mention.hits.length, 'time')}`,
        })),
    ];
}

// Each report: its label, its rows from the API's JSON, the concept a row opens, its columns.
const REPORTS = {
    lint: {
        label: 'Lint',
        rows: report => [
            ...report.errors.map(finding => ({ ...finding, level: 'error' })),
            ...report.warnings.map(finding => ({ ...finding, level: 'warn' })),
        ],
        path: row => row.file,
        columns: [
            { key: 'level', label: 'Severity', render: row => pill(row.level === 'error' ? 'error' : 'warning', row.level), sort: row => row.level },
            { key: 'file', label: 'Path', class: 'mono', sort: row => row.file },
            { key: 'line', label: 'Line', sort: row => row.line },
            { key: 'rule', label: 'Rule', class: 'mono', sort: row => row.rule },
            { key: 'message', label: 'Message' },
        ],
    },
    sprawl: {
        label: 'Sprawl',
        rows: report => report.reports,
        path: row => row.path,
        columns: [
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'score', label: 'Score', sort: row => row.score },
            { key: 'lines', label: 'Lines', sort: row => row.lines },
            { key: 'sections', label: 'Sections', sort: row => row.sections },
            { key: 'volatile', label: 'Volatile', sort: row => row.volatile },
            { key: 'exempt', label: 'Exempt', render: row => row.exempt ?? '-' },
        ],
    },
    links: {
        label: 'Links',
        rows: linkRows,
        path: row => row.path,
        columns: [
            { key: 'kind', label: 'Problem', render: row => pill(row.kind, row.level), sort: row => row.kind },
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'line', label: 'Line', render: row => row.line ?? '-' },
            { key: 'detail', label: 'Detail' },
        ],
    },
    oldest: {
        label: 'Oldest',
        rows: report => report.rows,
        path: row => row.path,
        columns: [
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'title', label: 'Title', render: row => row.title ?? '-' },
            { key: 'last_touched', label: 'Last stamp', render: row => (row.last_touched ? `${ago(row.last_touched)} (${row.last_touched_by})` : 'never'), sort: row => row.last_touched ?? '' },
            { key: 'age_days', label: 'Age (days)', render: row => row.age_days ?? '-', sort: row => row.age_days ?? Infinity },
            { key: 'stale', label: 'Stale', render: row => (row.stale ? pill('stale', 'warn') : '-') },
        ],
    },
    questions: {
        label: 'Open questions',
        rows: report => report.concepts.flatMap(concept =>
            concept.questions.map(question => ({ path: concept.path, number: question.number, text: question.text }))),
        path: row => row.path,
        columns: [
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'number', label: '#' },
            { key: 'text', label: 'Question' },
        ],
    },
};

function tile(id, value, label, detail) {
    return h('div', { class: 'tile', id: `tile-${id}` },
        h('div', { class: 'n', id: `n-${id}` }, String(value)),
        h('div', { class: 'l' }, label),
        detail ? h('div', { class: 'l mono', id: `d-${id}` }, detail) : null);
}

async function loadTiles(catalog, tiles) {
    try {
        const counts = await api.get(`/catalogs/${enc(catalog)}/health`);
        const oldest = counts.oldest === null ? '-'
            : counts.oldest.age_days === null ? 'never'
                : `${Math.floor(counts.oldest.age_days)} d`;
        clear(tiles).append(
            tile('lint', counts.lint_errors, 'lint errors', count(counts.lint_warnings, 'warning')),
            tile('sprawl', counts.sprawl, 'sprawling concepts', null),
            tile('orphans', counts.orphans, 'orphans', count(counts.broken_links, 'broken link')),
            tile('oldest', oldest, 'oldest stamp', counts.oldest ? counts.oldest.path : 'no concepts'),
            tile('questions', counts.open_questions, 'open questions', null));
    } catch {
        clear(tiles).append(h('p', { class: 'muted' }, 'The counts could not be loaded.'));
    }
}

export async function render(main) {
    const catalog = store.catalog;
    main.append(h('header', { class: 'page-head' },
        h('div', {}, h('h1', {}, 'Health'), h('p', { class: 'muted' }, catalog ? `Catalog ${catalog}` : 'No catalog selected.')),
        catalog
            ? h('div', { class: 'actions' }, h('button', {
                type: 'button', id: 'health-refresh', class: 'secondary',
                onclick: () => { render(clear(main)); },
            }, 'Refresh'))
            : null));
    if (!catalog) return;

    const tiles = h('section', { class: 'tiles', id: 'health-tiles' }, h('p', { class: 'muted' }, 'Loading...'));
    const holder = h('section', { id: 'health-report' });
    const cache = new Map();   // report name -> the API's JSON, until Refresh
    let ticket = 0;            // a newer switch wins over a slower older fetch

    async function show(name) {
        const mine = ++ticket;
        const spec = REPORTS[name];
        clear(holder).append(h('p', { class: 'muted' }, 'Loading...'));
        try {
            if (!cache.has(name)) cache.set(name, await api.get(`/catalogs/${enc(catalog)}/reports/${name}`));
            if (mine !== ticket) return;
            const table = listTable(spec.columns, spec.rows(cache.get(name)), {
                onRow: row => go(browseHash(catalog, spec.path(row))),
                empty: 'Nothing to report.',
            });
            table.id = 'report-table';
            table.dataset.report = name;
            clear(holder).append(table);
        } catch {
            if (mine === ticket) clear(holder).append(h('p', { class: 'muted' }, 'This report could not be loaded.'));
        }
    }

    const switcher = segments(Object.entries(REPORTS).map(([value, spec]) => ({ value, label: spec.label })), 'lint', show);
    main.append(tiles, switcher, holder);
    await Promise.all([loadTiles(catalog, tiles), show('lint')]);
}
```

`render(clear(main))` works because `clear` returns the element. Until Task 4 lands, a row's `#/browse/...` link shows the router's "Not available yet" page, as the plan's "Task dependencies" expects.

- [ ] **Step 6: The browser journey**

`tests/ui_e2e/health.rs`:

```rust
//! The Health page: its tiles equal the API's counts, the Links report lists
//! an orphan, and a report row opens its concept in Browse.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};

use crate::harness::drive;
use crate::support::server::{files, TestServer};

const INDEX: &str = "# Main\n\n- [Alpha](systems/alpha.md): the first system\n";
const ALPHA: &str = "---\ntype: system\ntitle: Alpha\ndescription: The first system.\n---\n# Alpha\n\nAlpha runs [Beta](beta.md) and [Gone](gone.md).\n";
const BETA: &str = "---\ntype: system\ntitle: Beta\ndescription: The second system.\nopen_questions:\n  - Who owns Beta?\n---\n# Beta\n\nBeta serves [Alpha](alpha.md).\n";
// No `type` (a lint error), and nothing links here (an orphan).
const GAMMA: &str = "---\ntitle: Gamma\n---\n# Gamma\n\nNothing links here.\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_tiles_match_the_api_and_a_row_opens_its_concept() {
    let server = TestServer::start(
        vec![(
            "main",
            files(&[
                ("index.md", INDEX),
                ("systems/alpha.md", ALPHA),
                ("systems/beta.md", BETA),
                ("systems/gamma.md", GAMMA),
            ]),
        )],
        Config::default(),
    )
    .await;
    let key = server.key("console-health", Access::Read, &["main"], Some("main")).await;
    let health: Value = reqwest::Client::new()
        .get(format!("{}/api/v1/catalogs/main/health", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .expect("GET health")
        .json()
        .await
        .expect("health as JSON");
    assert_eq!(health["orphans"], 1, "{health}");
    assert!(health["lint_errors"].as_u64().unwrap_or(0) >= 1, "{health}");
    assert!(health["oldest"]["age_days"].is_null(), "no concept is stamped: {health}");
    let broken = health["broken_links"].as_u64().expect("broken_links");
    let broken_line = if broken == 1 {
        "1 broken link".to_string()
    } else {
        format!("{broken} broken links")
    };
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/health");
        ui.wait_visible("#n-lint");
        for (tile, field) in [
            ("lint", "lint_errors"),
            ("sprawl", "sprawl"),
            ("orphans", "orphans"),
            ("questions", "open_questions"),
        ] {
            assert_eq!(ui.text(&format!("#n-{tile}")), health[field].to_string(), "tile {tile}");
        }
        assert_eq!(ui.text("#d-orphans"), broken_line);
        assert_eq!(ui.text("#n-oldest"), "never");

        ui.click(".segments .seg[data-value=links]");
        ui.wait_text("#report-table", "systems/gamma.md");
        let clicked = ui.eval(
            "(path) => { const row = [...document.querySelectorAll('#report-table tbody tr')]\
             .find(r => r.textContent.includes(path) && r.textContent.includes('orphan')); \
             if (!row) return false; row.click(); return true; }",
            json!("systems/gamma.md"),
        );
        assert_eq!(clicked, json!(true));
        assert_eq!(
            ui.eval("() => location.hash", json!(null)),
            json!("#/browse/main/systems/gamma.md")
        );
    })
    .await;
}
```

`tests/ui_e2e/main.rs`: add `mod health;` (keep the other tasks' lines).

Run: `just test-ui`
Expected: `health_tiles_match_the_api_and_a_row_opens_its_concept` passes with Task 1's login journeys, with no skip lines.

- [ ] **Step 7: Docs and changelog**

- `docs/operations.md`, "Web console", subsection "Health". Rows for the API reference table:

  | Method | Path | Access | Notes |
  |---|---|---|---|
  | GET | `/api/v1/catalogs/{catalog}/health` | read | `{catalog, lint_errors, lint_warnings, sprawl, orphans, broken_links, oldest, open_questions}`. `sprawl` counts `lint`'s `sprawl` warnings; `oldest` is the stalest concept's `oldest` row (`null` without concepts); links are counted without mentions. |
  | GET | `/api/v1/catalogs/{catalog}/reports/{report}` | read | `report` is `lint`, `sprawl`, `links`, `oldest` or `questions`; the body is that tool's result unchanged. `?n=` for `oldest` and `sprawl` (default 20, clamped to 1..=200; the MCP tools default to 5), `?mentions=false` for `links`. Another name is 404 `NotFound`. |

  Then one paragraph: the Health page is the console's landing page; its tiles are these counts, the switcher shows one report, and a row opens its concept in Browse.

  > **Corrected (issue #218, 2026-11-10).** `docs/operations.md`'s merged "API reference" table has three columns (`Method and path | Key | Does`), not the four above. The Health rows use that shape, in their own `### Health` subsection placed before "Testing the console", so each area's subsection sits beside the others and a merge keeps both sides. Tasks 2 and 4 to 7: write your rows the same way.
- `README.md`, "Web console": add "The Health page shows lint, sprawl, link, staleness and open-question counts, with each report one click away."
- `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m9-task-03-health.md`: Added (the two routes, `HealthCounts`, the Health page, the journey); Notes (unknown report is 404; `?n=` default 20 vs the tools' 5; sprawl counted from lint).

- [ ] **Step 8: Gates and commit**

Run every gate in "Conventions" (no parity run: nothing under `src/okf/` or `src/mcp/` changes). Then:

```bash
git add src/http/api/health.rs src/http/ui/js/pages/health.js tests/ui_e2e/health.rs \
    tests/ui_e2e/main.rs docs/operations.md README.md changelog/
git commit -m "feat(console): health counts, report endpoints and the Health page (M9 task 3)"
```

---

### Task 4: Browse and search

Depends on: Task 1. Parallel with Tasks 2, 3, 5, 6, 7 (shared files: `Cargo.toml`, `styles.css`, `tests/ui_e2e/main.rs`, `README.md`, `docs/operations.md`; keep both sides).

**Facts this task relies on** (checked at `62e88cd`; the code below was compiled, and its unit, API and browser tests were run, in a scratch assembly of Task 1 and this task on 2026-11-09):
- Outline slugs: `Outline::of` splits with `body_split` and takes headings and slugs from `headings` (`src/okf/outline.rs:74-76`). `headings` numbers repeats `x`, `x-1` (`src/okf/markdown.rs:232-269`), with `slugify` (`:277`). `body_split` (`:160`) is the split the renderer uses, so ids match the outline.
- `get_concept`'s `body` is `doc.doc().body()` (`src/mcp/documents.rs:214`), and its outline comes from `Outline::of(doc.text())` (`:225`). `raw` is returned only within `concept_outline_chars` or with `full` (`(params.raw && within)`, in `concept()` at `:207`). So `raw` present means the body is present, and the console renders from `raw` through `universal_newlines` (`src/okf/pystr.rs:67`), which is the snapshot's `text()`.
- `read_document` serves any folder's `index.md` or `log.md` and the root `AGENTS.md` only (`readable`, `src/mcp/documents.rs:413`; `read_document`, `:432`). Anything else is `InvalidPath`, which is 400.
- `resolve_link(workspace, link, source)` (`src/okf/markdown.rs:434`) returns `None` for a scheme or a pure `#fragment`, and turns a folder link into its `index.md`. `escapes_root` (`src/okf/paths.rs:90`).
- Snapshot: `docs()` (`src/okf/snapshot.rs:307`), `DocKind { Concept, Index, Log, Machinery }` (`:58`), `title()` (`:185`). `CatalogState::current() -> Arc<Loaded>` (`src/catalog.rs:187`). `Loaded::workspace() -> &MemWorkspace` (`:69`), and `MemWorkspace` implements `Workspace` (`src/okf/workspace.rs:814`).
- `ToolContext::one(Some(name))` (`src/mcp/context.rs:206`) goes through `selection::resolve` (`src/selection.rs:129`). An unreached or unknown catalog is `CatalogNotSelected` (404), and an empty `catalogs` list counts as unset, so it falls back to the key default (`:139-142`).
- `history(ctx, HistoryParams { path, catalog, limit })` (`src/mcp/history.rs:113`) resolves `x` and `x.md`, and answers `NotFound` for a path without revisions. `content_at(pool, catalog_id, path, revision)` (`src/store/history.rs:80`).
- `unified_diff(path, published, pending)` is private in `src/mcp/changesets.rs:680`. This task makes it `pub(crate)` and reuses it for revision diffs.
- `SearchParams { query, catalogs, concept_type, tags, status, limit, changeset }` (`src/mcp/search.rs:30-57`); `run` (`:100`). `CatalogHit` flattens `SearchHit { path, title, description, snippet, lines, chars }` (`src/okf/search.rs:182`, `:280`).
- Frontmatter keys: `generated`, `reviewed`, `open_questions`, `sources` (`KEY_ORDER`, `src/okf/doc.rs:34-48`). `generated` and `reviewed` are maps with `by` and `at` (`src/mcp/writes.rs:220`; fixtures `tests/fixtures/catalog/analysis/stamps.md:9`).
- `crate::mcp::blocking` (`src/mcp/mod.rs:166`). `Config::concept_outline_chars` is a pub field (`src/config.rs:83`).
- pulldown-cmark 0.13.6 (REG/pulldown-cmark-0.13.6):
  - `Tag::HtmlBlock` writes nothing (src/html.rs:161).
  - Table alignment is written as a `style="text-align: ..."` attribute (html.rs:232-234). The console's CSP (`style-src 'self'`) refuses it and reports a violation, so the renderer drops alignments.
  - A heading's `attrs` are written even without `ENABLE_HEADING_ATTRIBUTES` (html.rs:196-206).
  - An `Email` link gets its `mailto:` from the writer (html.rs:329-336).
  - `Parser::into_offset_iter` yields `(Event, Range<usize>)` (src/parse.rs:1473, :2235).
  - `Tag::Table(Vec<Alignment>)` (lib.rs:225), and `Alignment` is `Copy` (lib.rs:635).
- rustwright 0.3.0: `is_visible` answered `true` for a link inside a closed `<details>` (observed), so the journey checks `details.open` with `eval`.

**URL scheme (a deviation from the spec's paths).** An axum wildcard must be the last segment, so `/concepts/{*path}/links` cannot be routed. The per-document routes put their kind before the path:

| Spec | This task |
|---|---|
| `GET /catalogs/{c}/concepts/{*path}/links` | `GET /catalogs/{catalog}/links/{*path}` |
| `GET /catalogs/{c}/concepts/{*path}/history` | `GET /catalogs/{catalog}/history/{*path}` |
| `GET /catalogs/{c}/concepts/{*path}/revisions/{n}` | `GET /catalogs/{catalog}/revisions/{revision}/{*path}` |

**Console routes.** `#/browse/<catalog>/<path segments>[/<slug>]`. The path ends at the first segment ending in `.md`, and one more segment is a heading slug (a section). `#/browse/<catalog>/<folder...>` shows that folder's `index.md`, and `#/browse` shows the selected catalog's root `index.md`. `src/render.rs` writes rendered links in the same shape (`browse_route`), so a link to `beta.md#config` opens Beta at its Config heading. In-page `#fragment` links are rewritten too, since a bare `#config` would replace the route.

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock` (`pulldown-cmark`)
- Modify: `src/lib.rs` (`pub mod render;`)
- Create: `src/render.rs`
- Modify: `src/mcp/changesets.rs:680` (`unified_diff` becomes `pub(crate)`)
- Modify: `src/http/api/browse.rs`, `src/http/api/search.rs` (replace Task 1's stubs)
- Create: `src/http/ui/js/pages/browse.js`, `src/http/ui/js/pages/search.js`, `src/http/ui/js/components/tree.js`
- Modify: `src/http/ui/styles.css` (append)
- Create: `tests/ui_e2e/browse.rs`; Modify: `tests/ui_e2e/main.rs` (`mod browse;`)
- Modify: `README.md`, `docs/operations.md` ("Web console": "Browse and search")
- Create: `changelog/<ts>-m9-task-04-browse-and-search.md`

- [ ] **Step 1: Dependency and module wiring**

`Cargo.toml`, under `[dependencies]`, after `indexmap`:

```toml
# Markdown to HTML for the web console (src/render.rs), which escapes raw
# HTML and filters link schemes itself. No default features: they only add
# getopts for pulldown-cmark's own command-line tool.
pulldown-cmark = { version = "0.13.6", default-features = false, features = ["html"] }
```

`src/lib.rs`: add `pub mod render;` after `pub mod overlay;`.

`src/mcp/changesets.rs:680`: change `fn unified_diff(` to `pub(crate) fn unified_diff(`. The function itself is unchanged. Its doc comment speaks of the "published" and "pending" sides; revision diffs pass the previous and the shown revision.

- [ ] **Step 2: Write the failing renderer tests**

Create `src/render.rs` with only its `#[cfg(test)] mod tests` block. That block is the bottom of the full file in Step 4, from `#[cfg(test)]` to the end.

Run: `cargo test --lib render:: 2>&1 | tail -20`
Expected: it does not compile, because `to_html`, `Page`, `browse_route`, `scheme`, `has_scheme`, `HEADING_ID_PREFIX` and `BROKEN_LINK_TITLE` do not exist.

- [ ] **Step 3: Write the failing API tests**

In `src/http/api/browse.rs` and `src/http/api/search.rs`, keep Task 1's stub `routes()` and append the `#[cfg(test)] mod tests` blocks from Step 5.

Run: `cargo test --lib http::api:: 2>&1 | tail -20`
Expected: it does not compile, because `catalog_names` does not exist and the browse tests' routes are missing. With only the browse block added, the tests compile and fail with 404s.

- [ ] **Step 4: Implement the renderer**

`src/render.rs` (whole file):

```rust
//! Markdown to safe HTML for the web console (web console design, "Markdown
//! rendering"). The console inserts this HTML as markup, so everything that
//! could run script is removed here, on the server:
//!
//! - raw HTML in the source (blocks and inline tags) is shown as escaped
//!   text, an HTML block as a code block;
//! - a link may point only at `http`, `https`, `mailto`, or a document of the
//!   same catalog, which becomes a console route (`#/browse/<catalog>/<path>`,
//!   plus `/<slug>` for a `#fragment`); any other link is plain text;
//! - an image becomes a link to its `http(s)` URL (the console's CSP loads no
//!   outside images, and it serves no catalog files), else its alt text;
//! - table alignment is dropped, because pulldown-cmark writes it as a
//!   `style` attribute, which the console's CSP refuses;
//! - headings get `id="section-<slug>"` and `data-slug="<slug>"`, the slug
//!   being the outline's ([`crate::okf::outline::Outline`]), so a section
//!   link finds its heading. The prefix keeps a heading named `Main` or
//!   `Drawer` from taking the id of a console element.
//!
//! Only the body is rendered: the frontmatter is split off exactly as the
//! outline splits it ([`body_split`]).

use std::collections::{HashMap, HashSet};

use pulldown_cmark::{html, Alignment, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType};
use pulldown_cmark::{Options, Parser, Tag, TagEnd};

use crate::okf::markdown::{body_split, headings, resolve_link, slugify};
use crate::okf::paths::escapes_root;
use crate::okf::workspace::Workspace;

/// The prefix of every heading's `id`.
pub const HEADING_ID_PREFIX: &str = "section-";

/// The schemes an outside link may use (compared case-insensitively).
const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// The schemes an image may use; it is shown as a link to its URL.
const IMAGE_SCHEMES: [&str; 2] = ["http", "https"];

/// The `title` of a link into the catalog whose target does not exist.
pub const BROKEN_LINK_TITLE: &str = "Not in this catalog";

/// The document being rendered: where its relative links resolve.
// `<'a>`: the struct borrows the names and the workspace for as long as the
// render runs, instead of copying them.
pub struct Page<'a> {
    /// The catalog's name, the first segment of every console route.
    catalog: &'a str,
    /// The document's catalog-relative path (no leading `/`).
    path: &'a str,
    /// The catalog's files, to tell a folder link from a file link and a
    /// broken link from a live one.
    workspace: &'a dyn Workspace,
}

impl<'a> Page<'a> {
    /// Describes the document being rendered.
    ///
    /// # Arguments
    /// * `catalog` - the catalog's name.
    /// * `path` - the document's catalog-relative path, without a leading `/`.
    /// * `workspace` - the catalog's files (a loaded catalog's
    ///   [`crate::catalog::Loaded::workspace`]).
    pub fn new(catalog: &'a str, path: &'a str, workspace: &'a dyn Workspace) -> Self {
        Self {
            catalog,
            path,
            workspace,
        }
    }
}

/// What becomes of one link.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    /// An allowed outside URL: written as it is.
    Keep,
    /// A document of the catalog: this console route, and whether the
    /// document is missing.
    Route { href: String, broken: bool },
    /// Anything else: the link text stays, as plain text.
    Text,
}

/// Whether an open link or image was written as an `<a>` (so its end is
/// written too) or dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Kept,
    Dropped,
}

/// The markdown extensions the console renders: GitHub tables,
/// strikethrough and task lists. Heading attributes (`{#id}`) stay off, so
/// a heading's text and slug are the outline's.
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Renders a document's body to HTML that is safe to insert as markup.
///
/// # Arguments
/// * `text` - the whole file with `\n` line endings (frontmatter included),
///   as the snapshot's `text()` or a raw file passed through
///   `universal_newlines`.
/// * `page` - where the document lives, for its links.
///
/// # Returns
/// The HTML of the body (the frontmatter is not rendered).
///
/// # Examples
/// ```
/// use okf_mcp::okf::workspace::MemWorkspace;
/// use okf_mcp::render::{to_html, Page};
/// let files = MemWorkspace::from_files([("a.md", "# A\n"), ("b.md", "# B\n")]);
/// let html = to_html("---\ntitle: A\n---\n# A\n\nSee [B](b.md).\n", &Page::new("main", "a.md", &files));
/// assert!(html.contains(r#"<h1 id="section-a" data-slug="a">A</h1>"#));
/// assert!(html.contains(r##"<a href="#/browse/main/b.md">B</a>"##));
/// assert!(!html.contains("title: A"));
/// ```
pub fn to_html(text: &str, page: &Page<'_>) -> String {
    let (_, lines) = body_split(text);
    // The outline's headings and slugs, found on the same body lines.
    let outlined = headings(&lines);
    let body = lines.join("\n");
    // Byte offset where each body line starts, to map a heading's offset to
    // its line (a binary search, so the whole pass stays O(n log n)).
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let slug_at_line: HashMap<usize, &str> = outlined
        .iter()
        .map(|heading| (heading.index, heading.slug.as_str()))
        .collect();
    let mut taken: HashSet<String> = outlined
        .iter()
        .map(|heading| heading.slug.clone())
        .collect();
    let mut events: Vec<Event<'_>> = Vec::new();
    // One entry per open link or image, so its end event knows its fate.
    let mut open: Vec<Fate> = Vec::new();
    // A heading the outline does not list (setext, in a quote): where its
    // start event is in `events`, and its text so far.
    let mut unlisted: Option<(usize, String)> = None;
    // `into_offset_iter` yields each event with its byte range in `body`.
    for (event, range) in Parser::new_ext(&body, options()).into_offset_iter() {
        match event {
            // Raw HTML becomes text, which push_html escapes.
            Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Text(raw)),
            Event::Start(Tag::HtmlBlock) => {
                events.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)));
            }
            Event::End(TagEnd::HtmlBlock) => events.push(Event::End(TagEnd::CodeBlock)),
            Event::Start(Tag::Table(alignments)) => {
                let plain = vec![Alignment::None; alignments.len()];
                events.push(Event::Start(Tag::Table(plain)));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                // Never 0: `line_starts[0]` is 0, which every offset passes.
                let line = line_starts.partition_point(|&start| start <= range.start) - 1;
                match slug_at_line.get(&line) {
                    Some(slug) => events.push(heading(level, slug)),
                    None => {
                        unlisted = Some((events.len(), String::new()));
                        events.push(heading(level, ""));
                    }
                }
            }
            Event::End(TagEnd::Heading(level)) => {
                // `take` moves the value out and leaves `None` behind.
                if let Some((at, text)) = unlisted.take() {
                    let slug = unique(slugify(&text), &mut taken);
                    events[at] = heading(level, &slug);
                }
                events.push(Event::End(TagEnd::Heading(level)));
            }
            Event::Text(text) => {
                if let Some((_, heading_text)) = unlisted.as_mut() {
                    heading_text.push_str(&text);
                }
                events.push(Event::Text(text));
            }
            Event::Code(code) => {
                if let Some((_, heading_text)) = unlisted.as_mut() {
                    heading_text.push_str(&code);
                }
                events.push(Event::Code(code));
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => match destination(link_type, &dest_url, page) {
                Destination::Keep => {
                    open.push(Fate::Kept);
                    events.push(Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }));
                }
                Destination::Route { href, broken } => {
                    open.push(Fate::Kept);
                    let title = if broken {
                        CowStr::Borrowed(BROKEN_LINK_TITLE)
                    } else {
                        title
                    };
                    events.push(Event::Start(Tag::Link {
                        link_type: LinkType::Inline,
                        dest_url: CowStr::from(href),
                        title,
                        id,
                    }));
                }
                Destination::Text => open.push(Fate::Dropped),
            },
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                // An image inside a link stays its alt text: no `<a>` in an `<a>`.
                let in_link = open.contains(&Fate::Kept);
                if !in_link && has_scheme(&dest_url, &IMAGE_SCHEMES) {
                    open.push(Fate::Kept);
                    events.push(Event::Start(Tag::Link {
                        link_type: LinkType::Inline,
                        dest_url,
                        title,
                        id: CowStr::Borrowed(""),
                    }));
                } else {
                    open.push(Fate::Dropped);
                }
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                if open.pop() == Some(Fate::Kept) {
                    events.push(Event::End(TagEnd::Link));
                }
            }
            other => events.push(other),
        }
    }
    let mut out = String::with_capacity(body.len() + body.len() / 2);
    html::push_html(&mut out, events.into_iter());
    out
}

/// A heading's start event with the console's id and the slug.
///
/// # Arguments
/// * `level` - `h1` to `h6`.
/// * `slug` - the outline's slug (`""` for a placeholder, replaced at the
///   heading's end).
///
/// # Returns
/// An owned event (`'static`: it borrows nothing), which fits a list of
/// events borrowing the body.
fn heading(level: HeadingLevel, slug: &str) -> Event<'static> {
    Event::Start(Tag::Heading {
        level,
        id: Some(CowStr::from(format!("{HEADING_ID_PREFIX}{slug}"))),
        classes: Vec::new(),
        attrs: vec![(
            CowStr::Borrowed("data-slug"),
            Some(CowStr::from(slug.to_string())),
        )],
    })
}

/// `base`, or `base-1`, `base-2`, ... : the first not yet taken, which is
/// then taken (the outline numbers repeated headings the same way).
fn unique(base: String, taken: &mut HashSet<String>) -> String {
    let slug = if taken.contains(&base) {
        (1..)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| !taken.contains(candidate))
            .expect("an unbounded range always has an untaken number")
    } else {
        base
    };
    taken.insert(slug.clone());
    slug
}

/// The URL's scheme, when it starts with one (`scheme ":"`, RFC 3986).
///
/// # Returns
/// The scheme as written; `None` for a relative URL or a bare `#fragment`.
fn scheme(url: &str) -> Option<&str> {
    let end = url.find(':')?;
    let candidate = &url[..end];
    let mut chars = candidate.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(candidate)
}

/// Whether `url` is free of control characters and uses one of `schemes`.
/// Browsers drop tabs and newlines inside a URL and leading spaces, so
/// `jav&#9;ascript:` would run; a URL with any control character is refused.
fn has_scheme(url: &str, schemes: &[&str]) -> bool {
    !url.chars().any(|c| c.is_ascii_control())
        && scheme(url.trim_start_matches(' ')).is_some_and(|found| {
            schemes
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(found))
        })
}

/// Decides what becomes of a link.
///
/// # Arguments
/// * `link_type` - how it was written (an email autolink gets `mailto:`
///   from the writer).
/// * `url` - its destination as parsed (entities decoded).
/// * `page` - the document holding it.
///
/// # Returns
/// [`Destination::Keep`] for an allowed scheme; [`Destination::Route`] for
/// a `#fragment` or a relative link to a `.md` file inside the catalog;
/// [`Destination::Text`] for anything else (another scheme, a control
/// character, a protocol-relative `//host` URL, a path leaving the catalog,
/// a link to a file that is not markdown).
fn destination(link_type: LinkType, url: &str, page: &Page<'_>) -> Destination {
    if link_type == LinkType::Email {
        return Destination::Keep;
    }
    if url.chars().any(|c| c.is_ascii_control()) || url.starts_with("//") {
        return Destination::Text;
    }
    if scheme(url.trim_start_matches(' ')).is_some() {
        return if has_scheme(url, &ALLOWED_SCHEMES) {
            Destination::Keep
        } else {
            Destination::Text
        };
    }
    if let Some(fragment) = url.strip_prefix('#') {
        return Destination::Route {
            href: browse_route(page.catalog, page.path, Some(fragment)),
            broken: false,
        };
    }
    // `let ... else`: bind the `Some` value or leave the function.
    let Some(resolved) = resolve_link(page.workspace, url, page.path) else {
        return Destination::Text;
    };
    if escapes_root(&resolved.path) || !resolved.path.ends_with(".md") {
        return Destination::Text;
    }
    let fragment = url.split_once('#').map(|(_, fragment)| fragment);
    Destination::Route {
        href: browse_route(page.catalog, &resolved.path, fragment),
        broken: !page.workspace.is_file(&resolved.path),
    }
}

/// The console route of a document, and optionally one of its sections:
/// `#/browse/<catalog>/<path segments>[/<slug>]`, each segment
/// percent-encoded (the console's router decodes each one).
///
/// # Arguments
/// * `catalog` - the catalog.
/// * `path` - the document's catalog-relative path (a leading `/` is ignored).
/// * `section` - a heading slug; `None` or `""` for the top.
///
/// # Returns
/// The route, with its leading `#`.
///
/// # Examples
/// ```
/// use okf_mcp::render::browse_route;
/// assert_eq!(browse_route("main", "/a b/c.md", Some("notes")), "#/browse/main/a%20b/c.md/notes");
/// ```
pub fn browse_route(catalog: &str, path: &str, section: Option<&str>) -> String {
    let mut route = String::with_capacity(16 + catalog.len() + path.len());
    route.push_str("#/browse/");
    push_segment(&mut route, catalog);
    for segment in path.trim_start_matches('/').split('/') {
        route.push('/');
        push_segment(&mut route, segment);
    }
    if let Some(section) = section.filter(|section| !section.is_empty()) {
        route.push('/');
        push_segment(&mut route, section);
    }
    route
}

/// Appends `segment` percent-encoded: every byte but the RFC 3986
/// unreserved ones (`A-Z a-z 0-9 - . _ ~`) becomes `%XX`.
fn push_segment(out: &mut String, segment: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::outline::Outline;
    use crate::okf::workspace::MemWorkspace;

    /// A small catalog: `systems/alpha.md` links around it.
    fn catalog() -> MemWorkspace {
        MemWorkspace::from_files([
            ("index.md", "# Main\n"),
            ("systems/index.md", "# Systems\n"),
            ("systems/alpha.md", "# Alpha\n"),
            ("systems/beta.md", "# Beta\n\n## Config\n"),
            ("tools/okf.py", "print()\n"),
        ])
    }

    /// Renders `text` as `systems/alpha.md` of `main`.
    fn render(text: &str) -> String {
        let files = catalog();
        to_html(text, &Page::new("main", "systems/alpha.md", &files))
    }

    #[test]
    fn raw_html_is_escaped_never_passed_through() {
        let html =
            render("<script>alert(1)</script>\n\nText <b>bold</b> <img src=x onerror=alert(2)>\n");

        assert!(
            html.contains("<pre><code>&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );
        assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"), "{html}");
        assert!(
            html.contains("&lt;img src=x onerror=alert(2)&gt;"),
            "{html}"
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        assert!(!html.contains("<img"), "{html}");
    }

    #[test]
    fn script_schemes_in_links_and_images_become_text() {
        let html = render(
            "[click](javascript:alert(1)) ![pic](javascript:alert(2)) <javascript:alert(3)> \
             [upper](JAVASCRIPT:alert(4)) [data](data:text/html,hi) \
             [tab](jav&#x09;ascript:alert(5)) ![svg](data:image/svg+xml,x) [proto](//example.com/x)\n",
        );

        let lowered = html.to_lowercase();
        assert!(!lowered.contains("href=\"javascript"), "{html}");
        assert!(!lowered.contains("href=\"jav"), "{html}");
        assert!(!lowered.contains("href=\"data"), "{html}");
        assert!(!lowered.contains("href=\"//"), "{html}");
        assert!(!lowered.contains("<img"), "{html}");
        assert!(!html.contains("<a"), "no link survives: {html}");
        for text in ["click", "pic", "upper", "data", "tab", "svg", "proto"] {
            assert!(html.contains(text), "{text} stays as text: {html}");
        }
    }

    #[test]
    fn outside_links_keep_allowed_schemes_and_images_become_links() {
        let html = render(
            "[site](https://example.com/x \"Site\") [plain](http://example.com) \
             [mail](mailto:ops@example.com) <ops@example.com> ![logo](https://example.com/l.png)\n",
        );

        assert!(
            html.contains(r#"<a href="https://example.com/x" title="Site">site</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="http://example.com">plain</a>"#),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"href="mailto:ops@example.com""#).count(),
            2,
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="https://example.com/l.png">logo</a>"#),
            "{html}"
        );
    }

    #[test]
    fn relative_links_become_console_routes() {
        let html = render(
            "[Beta](beta.md) [Home](../index.md) [Cfg](beta.md#config) [Here](#notes) \
             [Folder](./) [Abs](/systems/beta.md)\n",
        );

        for href in [
            "#/browse/main/systems/beta.md",
            "#/browse/main/index.md",
            "#/browse/main/systems/beta.md/config",
            "#/browse/main/systems/alpha.md/notes",
            "#/browse/main/systems/index.md",
        ] {
            assert!(
                html.contains(&format!("href=\"{href}\"")),
                "{href} in {html}"
            );
        }
        assert_eq!(
            html.matches("href=\"#/browse/main/systems/beta.md\"")
                .count(),
            2,
            "{html}"
        );
        assert!(!html.contains(BROKEN_LINK_TITLE), "{html}");
    }

    #[test]
    fn links_leaving_the_catalog_or_to_other_files_are_text_and_missing_ones_are_marked() {
        let html = render("[Out](../../etc/passwd.md) [Tool](../tools/okf.py) [Gone](gone.md)\n");

        assert!(!html.contains("passwd"), "{html}");
        assert!(!html.contains("okf.py"), "{html}");
        assert!(html.contains("Out") && html.contains("Tool"), "{html}");
        assert!(
            html.contains(&format!(
                "<a href=\"#/browse/main/systems/gone.md\" title=\"{BROKEN_LINK_TITLE}\">Gone</a>"
            )),
            "{html}"
        );
    }

    #[test]
    fn heading_ids_are_the_outline_slugs() {
        let text =
            "---\ntitle: A\n---\n# Alpha\n\n## Notes\n\n## Notes\n\n```\n# not a heading\n```\n\
                    ## Use `okf.py` [here](beta.md)\n";
        let html = render(text);

        let slugs: Vec<String> = Outline::of(text)
            .entries()
            .into_iter()
            .map(|entry| entry.slug)
            .collect();
        assert_eq!(slugs, ["alpha", "notes", "notes-1", "use-okfpy-here"]);
        for slug in &slugs {
            assert!(
                html.contains(&format!(
                    "id=\"{HEADING_ID_PREFIX}{slug}\" data-slug=\"{slug}\""
                )),
                "{slug} in {html}"
            );
        }
        assert_eq!(html.matches("data-slug=").count(), slugs.len(), "{html}");
    }

    #[test]
    fn headings_the_outline_skips_get_unused_slugs() {
        let html = render("Title\n=====\n\n# Title\n\n> # Quoted\n");

        assert!(
            html.contains(r#"<h1 id="section-title" data-slug="title">Title</h1>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-slug="title-1""#),
            "the setext heading: {html}"
        );
        assert!(html.contains(r#"data-slug="quoted""#), "{html}");
    }

    #[test]
    fn frontmatter_is_not_rendered() {
        let html = render("---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nBody.\n");

        assert!(!html.contains("type: system"), "{html}");
        assert!(!html.contains("<hr"), "{html}");
        assert!(html.contains("<p>Body.</p>"), "{html}");
    }

    #[test]
    fn tables_strikethrough_and_task_lists_render_without_inline_styles() {
        let html = render("| a | b |\n|:--|--:|\n| 1 | 2 |\n\n~~old~~\n\n- [x] done\n- [ ] open\n");

        assert!(html.contains("<table>"), "{html}");
        assert!(
            html.contains("<th>a</th>") && html.contains("<td>2</td>"),
            "{html}"
        );
        assert!(
            !html.contains("style="),
            "the CSP refuses style attributes: {html}"
        );
        assert!(html.contains("<del>old</del>"), "{html}");
        assert!(html.contains(r#"type="checkbox""#), "{html}");
    }

    #[test]
    fn routes_percent_encode_each_segment() {
        assert_eq!(
            browse_route("main", "a b/c#d.md", Some("x y")),
            "#/browse/main/a%20b/c%23d.md/x%20y"
        );
        assert_eq!(
            browse_route("main", "/index.md", Some("")),
            "#/browse/main/index.md"
        );
        assert_eq!(
            browse_route("main", "n\u{e9}.md", None),
            "#/browse/main/n%C3%A9.md"
        );
    }

    #[test]
    fn schemes_are_recognized_only_at_the_start() {
        assert_eq!(scheme("https://example.com"), Some("https"));
        assert_eq!(scheme("dir/a:b.md"), None);
        assert_eq!(scheme("#x:y"), None);
        assert!(has_scheme(" HTTPS://example.com", &ALLOWED_SCHEMES));
        assert!(!has_scheme("ht\ttps://example.com", &ALLOWED_SCHEMES));
    }
}
```

Run: `cargo test --lib render:: && cargo test --doc render`
Expected: PASS. That is 11 unit tests (`raw_html_is_escaped_never_passed_through`, `script_schemes_in_links_and_images_become_text`, `outside_links_keep_allowed_schemes_and_images_become_links`, `relative_links_become_console_routes`, `links_leaving_the_catalog_or_to_other_files_are_text_and_missing_ones_are_marked`, `heading_ids_are_the_outline_slugs`, `headings_the_outline_skips_get_unused_slugs`, `frontmatter_is_not_rendered`, `tables_strikethrough_and_task_lists_render_without_inline_styles`, `routes_percent_encode_each_segment`, `schemes_are_recognized_only_at_the_start`) and 2 doctests (`to_html`, `browse_route`).

- [ ] **Step 5: Implement the browse and search endpoints**

`src/http/api/browse.rs` (whole file, replacing the stub):

```rust
//! Browsing under `/api/v1` (web console design, "API": Browse): a catalog's
//! tree, a concept with its body rendered to safe HTML, its links, its
//! history, one revision with its diff, and the non-concept documents.
//!
//! An axum wildcard (`{*path}`) must be the last segment of a route, so the
//! per-concept routes put their kind before the path
//! (`/catalogs/{catalog}/links/{*path}`), not after it as the design wrote
//! (`/concepts/{*path}/links`).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::catalog::Loaded;
use crate::error::OkfError;
use crate::http::api::context;
use crate::http::AppError;
use crate::mcp::blocking;
use crate::mcp::changesets::unified_diff;
use crate::mcp::documents::{
    get_concept, read_document, Concept, Document, GetConceptParams, InboundLink, OutboundLink,
    ReadDocumentParams,
};
use crate::mcp::history::{history, History, HistoryParams};
use crate::okf::outline::Outline;
use crate::okf::pystr::universal_newlines;
use crate::okf::snapshot::DocKind;
use crate::render::{to_html, Page};
use crate::store::history::content_at;
use crate::store::keys::Access;

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{catalog}/tree", get(tree))
        .route("/catalogs/{catalog}/concepts/{*path}", get(concept))
        .route("/catalogs/{catalog}/links/{*path}", get(links))
        .route("/catalogs/{catalog}/history/{*path}", get(revisions))
        .route(
            "/catalogs/{catalog}/revisions/{revision}/{*path}",
            get(revision),
        )
        .route("/catalogs/{catalog}/documents/{*path}", get(document))
}

/// One document in a catalog's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TreeDocument {
    /// `/`-prefixed path.
    pub path: String,
    /// `concept`, `index`, `log` or `machinery` (the root `AGENTS.md`).
    pub kind: &'static str,
    /// Its title, if it has one.
    pub title: Option<String>,
}

/// `GET /catalogs/{catalog}/tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Tree {
    /// The catalog.
    pub catalog: String,
    /// Every document the console can open, in the catalog's walk order.
    /// The client builds the folders from the paths.
    pub documents: Vec<TreeDocument>,
}

/// `GET /catalogs/{catalog}/concepts/{*path}`: `get_concept`'s result with
/// the rendered body.
// No `Eq`: `Concept` holds JSON frontmatter, which is only `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ConceptView {
    /// The concept, as `get_concept` returns it with `raw`; `outline` is
    /// always present (the whole outline, also within the limit).
    // `flatten` writes the concept's fields beside `body_html`.
    #[serde(flatten)]
    pub concept: Concept,
    /// The body as safe HTML ([`crate::render`]); `None` when the body is over
    /// `concept_outline_chars` and `full` was not passed.
    pub body_html: Option<String>,
}

/// `GET /catalogs/{catalog}/links/{*path}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ConceptLinks {
    /// The catalog.
    pub catalog: String,
    /// `/`-prefixed path of the concept.
    pub path: String,
    /// Its outbound links, in-page links left out.
    pub links: Vec<OutboundLink>,
    /// The documents linking to it.
    pub backlinks: Vec<InboundLink>,
}

/// `GET /catalogs/{catalog}/revisions/{revision}/{*path}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RevisionView {
    /// The catalog.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// The revision shown.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// The file at this revision; `None` for a deletion or bytes that are not UTF-8.
    pub text: Option<String>,
    /// A unified diff from the previous revision (from nothing for revision 1).
    pub diff: String,
}

/// `GET /catalogs/{catalog}/documents/{*path}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DocumentView {
    /// The file, as `read_document` returns it (`catalog`, `path`, `text`).
    #[serde(flatten)]
    pub document: Document,
    /// The body as safe HTML.
    pub body_html: String,
}

/// `?full=` on the concept route.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct FullQuery {
    /// Render the whole body even over `concept_outline_chars`.
    // `default`: an absent parameter is `false`.
    #[serde(default)]
    full: bool,
}

/// `?limit=` on the history route.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct LimitQuery {
    /// At most this many revisions (as `history`: default 20, at most 200).
    limit: Option<u32>,
}

/// The tree's name for a document kind, or `None` for a file the console
/// cannot open: `read_document` serves only the root `AGENTS.md` among the
/// machinery files.
///
/// # Arguments
/// * `kind` - the document's kind.
/// * `path` - its catalog-relative path.
fn kind_name(kind: DocKind, path: &str) -> Option<&'static str> {
    match kind {
        DocKind::Concept => Some("concept"),
        DocKind::Index => Some("index"),
        DocKind::Log => Some("log"),
        // `bool::then_some(x)` is `Some(x)` when true, else `None`.
        DocKind::Machinery => (path == "AGENTS.md").then_some("machinery"),
    }
}

/// `GET /catalogs/{catalog}/tree`.
///
/// # Errors
/// 404 for a catalog the key does not reach.
async fn tree(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<Tree>, AppError> {
    let ctx = context(&app, principal);
    let state = ctx.one(Some(&catalog))?;
    let loaded = state.current();
    let documents = loaded
        .snapshot()
        .docs()
        .iter()
        // `filter_map` keeps the `Some`s; `?` inside the closure skips a doc.
        .filter_map(|doc| {
            Some(TreeDocument {
                path: format!("/{}", doc.path()),
                kind: kind_name(doc.kind(), doc.path())?,
                title: doc.title().map(str::to_string),
            })
        })
        .collect();
    Ok(Json(Tree {
        catalog: state.name().to_string(),
        documents,
    }))
}

/// `GET /catalogs/{catalog}/concepts/{*path}?full=`: `get_concept` with
/// `raw`, plus `body_html` and the whole outline.
///
/// # Errors
/// As `get_concept`: 404 for an unreached catalog or a missing concept, 400
/// for a path that is not a concept.
async fn concept(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
    Query(query): Query<FullQuery>,
) -> Result<Json<ConceptView>, AppError> {
    let ctx = context(&app, principal);
    let concept = get_concept(
        &ctx,
        GetConceptParams {
            path,
            catalog: Some(catalog),
            links: false,
            raw: true,
            section: None,
            full: query.full,
            changeset: None,
        },
    )
    .await?;
    let loaded = ctx.one(Some(&concept.catalog))?.current();
    // Rendering is CPU work proportional to the body: off the async threads.
    // `move` hands `concept` and `loaded` (an `Arc`) to the blocking thread.
    Ok(Json(blocking(move || with_html(concept, &loaded)).await))
}

/// Adds the rendered body and the whole outline to a concept.
///
/// # Arguments
/// * `concept` - `get_concept`'s result, read with `raw`.
/// * `loaded` - its catalog, for resolving links.
///
/// # Returns
/// The view. `raw` is present exactly when the body is (within the limit, or
/// `full`), so a concept without `raw` keeps `get_concept`'s outline and gets
/// no `body_html`.
fn with_html(mut concept: Concept, loaded: &Loaded) -> ConceptView {
    // `let ... else`: bind the `Some` value or return early.
    let Some(raw) = concept.raw.as_deref() else {
        return ConceptView {
            concept,
            body_html: None,
        };
    };
    // The outline's view of the file: `\r\n` read as `\n`, as the snapshot reads it.
    let text = universal_newlines(raw);
    let page = Page::new(
        &concept.catalog,
        concept.path.trim_start_matches('/'),
        loaded.workspace(),
    );
    let body_html = to_html(&text, &page);
    let outline = Outline::of(&text).entries();
    // The borrows of `concept.raw` (through `text`) end above, so the
    // concept can be changed and moved now.
    concept.outline = Some(outline);
    ConceptView {
        concept,
        body_html: Some(body_html),
    }
}

/// `GET /catalogs/{catalog}/links/{*path}`.
///
/// # Errors
/// As `get_concept`.
async fn links(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
) -> Result<Json<ConceptLinks>, AppError> {
    let ctx = context(&app, principal);
    let concept = get_concept(
        &ctx,
        GetConceptParams {
            path,
            catalog: Some(catalog),
            links: true,
            raw: false,
            section: None,
            full: false,
            changeset: None,
        },
    )
    .await?;
    Ok(Json(ConceptLinks {
        catalog: concept.catalog,
        path: concept.path,
        // With `links: true` both are `Some`; `unwrap_or_default` keeps the
        // handler total anyway.
        links: concept.links.unwrap_or_default(),
        backlinks: concept.backlinks.unwrap_or_default(),
    }))
}

/// `GET /catalogs/{catalog}/history/{*path}?limit=`: the `history` tool.
///
/// # Errors
/// As `history`: 404 for an unreached catalog or a path without revisions.
async fn revisions(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<History>, AppError> {
    let ctx = context(&app, principal);
    let found = history(
        &ctx,
        HistoryParams {
            path,
            catalog: Some(catalog),
            limit: query.limit,
        },
    )
    .await?;
    Ok(Json(found))
}

/// `GET /catalogs/{catalog}/revisions/{revision}/{*path}`: the file at a
/// revision and a unified diff against the revision before it.
///
/// # Errors
/// 404 for an unreached catalog, a path without revisions, or a revision
/// the path never had; 400 for a revision that is not a number.
async fn revision(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, revision, path)): Path<(String, i32, String)>,
) -> Result<Json<RevisionView>, AppError> {
    // This reads the store directly, so it checks access itself (before the
    // principal moves into the context).
    principal.require(Access::Read, &catalog)?;
    let ctx = context(&app, principal);
    // `history` resolves the path as the History tab spelled it (`.md`
    // optional) and answers 404 for a path that never existed.
    let found = history(
        &ctx,
        HistoryParams {
            path,
            catalog: Some(catalog),
            limit: Some(1),
        },
    )
    .await?;
    let state = ctx.one(Some(&found.catalog))?;
    let stored = found.path.trim_start_matches('/');
    let current = content_at(app.pool(), state.id(), stored, revision)
        .await?
        .ok_or_else(|| OkfError::NotFound {
            path: format!("{} (revision {revision})", found.path),
        })?;
    let previous = if revision > 1 {
        // `and_then`: a missing row or a deletion both mean "no text before".
        content_at(app.pool(), state.id(), stored, revision - 1)
            .await?
            .and_then(|row| row.content)
    } else {
        None
    };
    let diff = unified_diff(stored, previous.as_deref(), current.content.as_deref());
    // `.ok()` turns bytes that are not UTF-8 into `None`, like a deletion.
    let text = current
        .content
        .and_then(|bytes| String::from_utf8(bytes).ok());
    Ok(Json(RevisionView {
        catalog: found.catalog,
        path: found.path,
        revision,
        op: current.op,
        text,
        diff,
    }))
}

/// `GET /catalogs/{catalog}/documents/{*path}`: `read_document` (any
/// folder's `index.md` or `log.md`, the root `AGENTS.md`) with `body_html`.
///
/// # Errors
/// As `read_document`: 400 for any other path, 404 for an unreached catalog
/// or a missing file.
async fn document(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
) -> Result<Json<DocumentView>, AppError> {
    let ctx = context(&app, principal);
    let document = read_document(
        &ctx,
        ReadDocumentParams {
            path: Some(path),
            catalog: Some(catalog),
            changeset: None,
        },
    )
    .await?;
    let loaded = ctx.one(Some(&document.catalog))?.current();
    let view = blocking(move || {
        // A block, so the borrow of `document` ends before it moves below.
        let body_html = {
            let text = universal_newlines(&document.text);
            let page = Page::new(
                &document.catalog,
                document.path.trim_start_matches('/'),
                loaded.workspace(),
            );
            to_html(&text, &page)
        };
        DocumentView {
            document,
            body_html,
        }
    })
    .await;
    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::{json, Value};

    use crate::archive::files;
    use crate::config::Config;
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row, MAIN};
    use crate::mcp::testing::app_with;
    use crate::store::import::{import, ImportOptions, ImportScope};

    /// The `main` files with `systems/alpha.md`'s last line changed.
    fn edited_main() -> Vec<(&'static str, String)> {
        MAIN.iter()
            .map(|(path, text)| {
                let text = if *path == "systems/alpha.md" {
                    text.replace("Nothing yet.", "Two settings.")
                } else {
                    (*text).to_string()
                };
                (*path, text)
            })
            .collect()
    }

    /// The entry for `path` in a tree response.
    fn entry<'a>(tree: &'a Value, path: &str) -> &'a Value {
        tree["documents"]
            .as_array()
            .expect("documents")
            .iter()
            .find(|doc| doc["path"] == path)
            .unwrap_or_else(|| panic!("{path} in {tree}"))
    }

    #[tokio::test]
    async fn every_browse_route_needs_read_on_its_catalog() {
        let test = app().await;
        let rows: Vec<Row> = [
            "/api/v1/catalogs/main/tree",
            "/api/v1/catalogs/main/concepts/systems/alpha.md",
            "/api/v1/catalogs/main/links/systems/alpha.md",
            "/api/v1/catalogs/main/history/systems/alpha.md",
            "/api/v1/catalogs/main/revisions/1/systems/alpha.md",
            "/api/v1/catalogs/main/documents/systems/index.md",
        ]
        .into_iter()
        .map(|uri| Row {
            method: "GET",
            uri: uri.to_string(),
            body: None,
            read: 200,
            write: 200,
            admin: 200,
            stranger: 404,
        })
        .collect();

        assert_matrix(&test, &rows).await;
    }

    #[tokio::test]
    async fn the_tree_lists_concepts_indexes_the_log_and_agents_with_titles() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, tree) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/tree",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{tree}");
        assert_eq!(tree["catalog"], "main");
        assert_eq!(
            entry(&tree, "/systems/alpha.md"),
            &json!({"path": "/systems/alpha.md", "kind": "concept", "title": "Alpha"})
        );
        assert_eq!(entry(&tree, "/systems/index.md")["kind"], "index");
        assert_eq!(entry(&tree, "/log.md")["kind"], "log");
        assert_eq!(entry(&tree, "/AGENTS.md")["kind"], "machinery");
        assert_eq!(tree["documents"].as_array().map(Vec::len), Some(MAIN.len()));
    }

    #[tokio::test]
    async fn a_concept_comes_with_its_rendered_body_raw_text_and_outline() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, concept) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/concepts/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{concept}");
        assert_eq!(concept["path"], "/systems/alpha.md");
        assert_eq!(concept["frontmatter"]["title"], "Alpha");
        let html = concept["body_html"].as_str().expect("body_html");
        assert!(
            html.contains(r#"<h1 id="section-alpha" data-slug="alpha">Alpha</h1>"#),
            "{html}"
        );
        assert!(
            html.contains(r##"<a href="#/browse/main/systems/beta.md">Beta</a>"##),
            "{html}"
        );
        assert!(!html.contains("type: system"), "no frontmatter in {html}");
        assert!(concept["raw"]
            .as_str()
            .expect("raw")
            .starts_with("---\ntype: system\n"));
        let slugs: Vec<&str> = concept["outline"]
            .as_array()
            .expect("outline")
            .iter()
            .filter_map(|heading| heading["slug"].as_str())
            .collect();
        assert_eq!(slugs, ["alpha", "config"]);
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_outlined_until_full_is_asked_for() {
        let config = Config {
            concept_outline_chars: 10,
            ..Config::default()
        };
        let test = app_with(
            &[("main", MAIN), ("other", &[("index.md", "# Other\n")])],
            config,
        )
        .await;
        let keys = keys(&test).await;
        let uri = "/api/v1/catalogs/main/concepts/systems/alpha.md";

        let (_, outlined) = send(&test, "GET", uri, Some(&keys.read), None).await;
        let (_, full) = send(
            &test,
            "GET",
            &format!("{uri}?full=true"),
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(outlined["truncated"], true);
        assert_eq!(outlined["body_html"], Value::Null);
        assert!(outlined.get("raw").is_none(), "{outlined}");
        assert_eq!(outlined["outline"][1]["slug"], "config");
        assert_eq!(full["truncated"], false);
        assert!(full["body_html"]
            .as_str()
            .is_some_and(|html| html.contains("Nothing yet.")));
    }

    #[tokio::test]
    async fn links_list_targets_and_backlinks() {
        let test = app().await;
        let keys = keys(&test).await;

        let (_, alpha) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/links/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, beta) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/links/systems/beta.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(alpha["links"][0]["target"], "/systems/beta.md");
        assert_eq!(alpha["links"][0]["broken"], false);
        let sources: Vec<&str> = beta["backlinks"]
            .as_array()
            .expect("backlinks")
            .iter()
            .filter_map(|link| link["path"].as_str())
            .collect();
        assert!(sources.contains(&"/systems/alpha.md"), "{beta}");
        assert!(sources.contains(&"/systems/index.md"), "{beta}");
    }

    #[tokio::test]
    async fn history_lists_revisions_and_a_revision_diffs_against_the_one_before() {
        let test = app().await;
        let keys = keys(&test).await;
        let edited = edited_main();
        let entries: Vec<(&str, &str)> = edited
            .iter()
            .map(|(path, text)| (*path, text.as_str()))
            .collect();
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "editor",
            now: Utc::now(),
        };
        import(test.app.pool(), "main", files(&entries), &options)
            .await
            .expect("import the edit");

        let (status, listed) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/history/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, second) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/2/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, first) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/1/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (missing, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/9/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (bad, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/latest/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{listed}");
        assert_eq!(listed["revisions"][0]["revision"], 2);
        assert_eq!(listed["revisions"][0]["actor"], "editor");
        assert_eq!(second["op"], "import");
        let diff = second["diff"].as_str().expect("diff");
        assert!(
            diff.contains("-Nothing yet.\n") && diff.contains("+Two settings.\n"),
            "{diff}"
        );
        assert!(second["text"]
            .as_str()
            .is_some_and(|text| text.contains("Two settings.")));
        assert!(first["diff"]
            .as_str()
            .is_some_and(|diff| diff.starts_with("--- /dev/null")));
        assert_eq!(missing.as_u16(), 404);
        assert_eq!(bad.as_u16(), 400);
    }

    #[tokio::test]
    async fn documents_render_indexes_and_refuse_concepts() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, index) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/documents/systems/index.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (concept, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/documents/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (unknown, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/concepts/systems/gamma.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{index}");
        assert_eq!(
            index["text"],
            "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n"
        );
        let html = index["body_html"].as_str().expect("body_html");
        assert!(
            html.contains(r##"href="#/browse/main/systems/alpha.md""##),
            "{html}"
        );
        assert_eq!(concept.as_u16(), 400);
        assert_eq!(unknown.as_u16(), 404);
    }
}
```

`src/http/api/search.rs` (whole file, replacing the stub):

```rust
//! `GET /api/v1/search` (web console design, "API": Search): the `search`
//! tool over the catalogs a request names, or every catalog the key reaches.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::auth::Principal;
use crate::http::api::context;
use crate::http::AppError;
use crate::mcp::search::{run, SearchParams, SearchResult};

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new().route("/search", get(search))
}

/// `?q=&catalogs=&limit=`.
#[derive(Debug, Clone, Default, Deserialize)]
struct SearchArgs {
    /// Free text, as the tool's `query`; absent means every concept.
    #[serde(default)]
    q: String,
    /// Comma-separated catalog names; absent or empty means every catalog
    /// the key reaches.
    catalogs: Option<String>,
    /// At most this many hits (as the tool: default 20, at most 200).
    limit: Option<usize>,
}

/// The names in a `catalogs=` value.
///
/// # Arguments
/// * `listed` - the raw value, such as `main, other`.
///
/// # Returns
/// The names, trimmed, empty ones dropped, in order.
fn catalog_names(listed: &str) -> Vec<String> {
    listed
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// `GET /search`.
///
/// Unlike the MCP tool, whose default is the connection's catalogs (else
/// the key's default catalog), the console has no connection: without
/// `catalogs` it searches every loaded catalog the key reaches.
///
/// # Errors
/// As `search`: 404 for a named catalog the key does not reach or that does
/// not exist.
async fn search(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(args): Query<SearchArgs>,
) -> Result<Json<SearchResult>, AppError> {
    let named = args
        .catalogs
        .as_deref()
        .map(catalog_names)
        .unwrap_or_default();
    let catalogs = if named.is_empty() {
        // `Catalogs` is a BTreeMap, so the names come out sorted. An empty
        // list (a key that reaches nothing) falls back to the key's default
        // inside the selection, which then refuses as the tool would.
        app.catalogs()
            .keys()
            .filter(|name| principal.reaches(name))
            .cloned()
            .collect()
    } else {
        named
    };
    let ctx = context(&app, principal);
    let result = run(
        &ctx,
        SearchParams {
            query: args.q,
            catalogs: Some(catalogs),
            concept_type: None,
            tags: Vec::new(),
            status: None,
            limit: args.limit,
            changeset: None,
        },
    )
    .await?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};

    #[test]
    fn catalog_names_are_trimmed_and_empty_ones_dropped() {
        assert_eq!(catalog_names(" main, other,,"), ["main", "other"]);
        assert!(catalog_names("").is_empty());
    }

    #[tokio::test]
    async fn search_needs_read_on_every_named_catalog() {
        let test = app().await;
        let row = |uri: &str, stranger: u16| Row {
            method: "GET",
            uri: uri.to_string(),
            body: None,
            read: 200,
            write: 200,
            admin: 200,
            stranger,
        };

        assert_matrix(
            &test,
            &[
                row("/api/v1/search?q=alpha&catalogs=main", 404),
                // Without `catalogs`, each key searches what it reaches.
                row("/api/v1/search?q=alpha", 200),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn search_finds_a_concept_with_its_catalog_and_snippet() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, found) = send(
            &test,
            "GET",
            "/api/v1/search?q=runs%20beta",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, limited) = send(
            &test,
            "GET",
            "/api/v1/search?q=&limit=1",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, elsewhere) = send(
            &test,
            "GET",
            "/api/v1/search?q=alpha",
            Some(&keys.stranger),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{found}");
        assert_eq!(found["hits"][0]["catalog"], "main");
        assert_eq!(found["hits"][0]["path"], "/systems/alpha.md");
        assert_eq!(found["hits"][0]["snippet"], "Alpha runs [Beta](beta.md).");
        assert_eq!(limited["hits"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            elsewhere["hits"],
            serde_json::json!([]),
            "other has no concepts"
        );
    }
}
```

- [ ] **Step 6: Run the API tests**

Run: `cargo test --lib -- render:: http::api::browse http::api::search --test-threads=6`
Expected: PASS, 21 tests. The browse tests are `every_browse_route_needs_read_on_its_catalog`, `the_tree_lists_concepts_indexes_the_log_and_agents_with_titles`, `a_concept_comes_with_its_rendered_body_raw_text_and_outline`, `a_body_over_the_limit_is_outlined_until_full_is_asked_for`, `links_list_targets_and_backlinks`, `history_lists_revisions_and_a_revision_diffs_against_the_one_before` and `documents_render_indexes_and_refuse_concepts`. The search tests are `catalog_names_are_trimmed_and_empty_ones_dropped`, `search_needs_read_on_every_named_catalog` and `search_finds_a_concept_with_its_catalog_and_snippet`. The other 11 are the renderer's.

- [ ] **Step 7: The Browse and Search pages**

Element ids and attributes the journeys use:
- Browse:
  - `#catalog-tree`. Folders are `details[data-folder="<path>"] > summary`, closed unless the open document is inside. Documents are `a[data-path="/<path>"]`.
  - `#concept-title`, `#provenance` and `#concept-tabs`, whose buttons are `[data-value=content|links|history|raw]`.
  - `#concept-pane`, holding `#outline`, `#markdown-body` (rendered HTML; headings carry `data-slug`), `#concept-truncated` with `#concept-full` (over the limit), `#links-out`, `#backlinks`, `#history` and `#concept-raw`.
  - The revision drawer (Task 1's `#drawer`, `#drawer-close`), holding `#revision-diff` (lines in `span.add`/`span.del`) and `#revision-text`.
- Search: `#search-form`, `#search-query`, `#search-results`, `#search-count`, and hits as `a.hit-title[data-catalog][data-path]`.

`src/http/ui/js/components/tree.js`:

```js
// The catalog tree for Browse, and the console route of a document.
// Folders are native <details> elements, so they collapse without script.
// documents: [{ path: '/a/b.md', kind: 'concept'|'index'|'log'|'machinery', title }]
import { h } from '../core/dom.js';
import { browseHash } from '../core/router.js';

const KIND_ORDER = { index: 0, concept: 1, log: 2, machinery: 3 };

function label(doc) {
    if (doc.kind === 'index') return 'Overview';
    return doc.title ?? doc.path.split('/').pop();
}

// selected: the '/'-prefixed path of the open document; the folders above it start open.
export function tree(documents, { catalog, selected }) {
    const root = { folders: new Map(), files: [] };
    for (const doc of documents) {
        const names = doc.path.replace(/^\//, '').split('/');
        let node = root;
        for (const name of names.slice(0, -1)) {
            if (!node.folders.has(name)) node.folders.set(name, { folders: new Map(), files: [] });
            node = node.folders.get(name);
        }
        node.files.push(doc);
    }
    return h('div', { class: 'tree', id: 'catalog-tree' }, branch(root, '', catalog, selected));
}

function branch(node, prefix, catalog, selected) {
    const folders = [...node.folders.keys()].sort().map(name => {
        const path = prefix + name;
        return h('li', {},
            h('details', { class: 'tree-folder', dataset: { folder: path }, open: selected.startsWith(`/${path}/`) },
                h('summary', {}, name),
                branch(node.folders.get(name), `${path}/`, catalog, selected)));
    });
    const files = node.files
        .slice()
        .sort((a, b) => (KIND_ORDER[a.kind] - KIND_ORDER[b.kind]) || label(a).localeCompare(label(b)))
        .map(doc => h('li', {},
            h('a', {
                href: browseHash(catalog, doc.path),
                class: doc.path === selected ? 'tree-link active' : 'tree-link',
                title: doc.path,
                dataset: { path: doc.path },
            }, label(doc))));
    return h('ul', {}, folders, files);
}
```

`src/http/ui/js/pages/browse.js`:

```js
// Browse: the catalog tree beside one document, read-only.
// Routes (each segment encoded; the router decodes them):
//   #/browse                                    the selected catalog's root index.md
//   #/browse/<catalog>[/<folder>...]            a folder's index.md
//   #/browse/<catalog>/<path>.md[/<slug>]       a concept or document, at a section
// A path ends at its first segment ending in '.md'; one more segment is a
// heading slug (the outline's, as src/render.rs writes links).
import { $, h, clear, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { go, browseHash } from '../core/router.js';
import { openDrawer } from '../core/drawer.js';
import { ago, when, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { segments } from '../components/segments.js';
import { tree } from '../components/tree.js';

// Read through /documents; everything else is a concept.
const DOCUMENTS = new Set(['index.md', 'log.md', 'AGENTS.md']);

// The shell keeps one #catalog-select for the page's lifetime (it only
// replaces its options), so one listener, added on the first render, serves
// every later render: a flag rather than a listener per navigation.
let switcherHooked = false;

function locate(rest) {
    const catalog = rest[0] ?? store.catalog;
    const parts = rest.slice(1);
    const file = parts.findIndex(part => part.endsWith('.md'));
    if (file === -1) {
        return { catalog, folder: parts.join('/'), path: [...parts, 'index.md'].join('/'), section: null };
    }
    return { catalog, folder: null, path: parts.slice(0, file + 1).join('/'), section: parts[file + 1] ?? null };
}

const encPath = path => path.split('/').map(enc).join('/');
const base = where => `/catalogs/${enc(where.catalog)}`;

export async function render(main, rest) {
    if (!switcherHooked) {
        // Switching catalogs while browsing opens the new catalog's root.
        $('#catalog-select').addEventListener('change', event => {
            if (location.hash.startsWith('#/browse')) go(browseHash(event.target.value));
        });
        switcherHooked = true;
    }
    const where = locate(rest);
    if (!where.catalog) {
        main.append(h('h1', {}, 'Browse'), h('p', { class: 'muted' }, 'This key reaches no catalog.'));
        return;
    }
    const side = h('aside', { class: 'browse-tree' }, h('p', { class: 'muted small' }, 'Loading...'));
    const view = h('section', { class: 'browse-view', id: 'browse-view' });
    main.append(h('div', { class: 'browse' }, side, view));
    let listing;
    try {
        listing = await api.get(`${base(where)}/tree`);
    } catch {
        clear(side);
        return;
    }
    clear(side).append(tree(listing.documents, { catalog: where.catalog, selected: `/${where.path}` }));
    const known = listing.documents.some(doc => doc.path === `/${where.path}`);
    if (where.folder !== null && !known) {
        showFolder(view, where, listing.documents);
        return;
    }
    if (DOCUMENTS.has(where.path.split('/').pop())) await showDocument(view, where);
    // A section route asks for the whole body: over concept_outline_chars the
    // server sends only the outline, and the heading to scroll to would be
    // missing (every navigation, an outline click included, renders afresh).
    else await showConcept(view, where, where.section !== null);
}

// A folder without index.md: its documents, as links.
function showFolder(view, where, documents) {
    const prefix = where.folder ? `/${where.folder}/` : '/';
    const inside = documents.filter(doc => doc.path.startsWith(prefix) && !doc.path.slice(prefix.length).includes('/'));
    clear(view).append(
        h('h1', { id: 'concept-title' }, prefix),
        h('p', { class: 'muted' }, 'This folder has no index.md.'),
        h('ul', {}, inside.map(doc => h('li', {}, h('a', { href: browseHash(where.catalog, doc.path) }, doc.title ?? doc.path)))));
}

// The page frame shared by concepts and documents: a header, segment tabs,
// and a pane each tab fills. A newer tab click wins over a slower older one.
function frame(view, { title, subtitle, extra, tabs }) {
    const pane = h('div', { id: 'concept-pane' });
    let ticket = 0;
    const show = async value => {
        const mine = ++ticket;
        const node = await tabs.find(tab => tab.value === value).build();
        if (mine === ticket) clear(pane).append(node);
    };
    const switcher = segments(tabs.map(({ value, label }) => ({ value, label })), tabs[0].value, show);
    switcher.id = 'concept-tabs';
    clear(view).append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', { id: 'concept-title' }, title),
                h('p', { class: 'mono small muted' }, subtitle),
                extra)),
        switcher,
        pane);
    show(tabs[0].value);
}

async function showConcept(view, where, full) {
    let concept;
    try {
        concept = await api.get(`${base(where)}/concepts/${encPath(where.path)}${full ? '?full=true' : ''}`);
    } catch {
        clear(view).append(h('p', { class: 'muted' }, 'This concept could not be loaded.'));
        return;
    }
    frame(view, {
        title: concept.title ?? concept.path,
        subtitle: `${concept.catalog} ${concept.path}`,
        extra: provenance(concept.frontmatter),
        tabs: [
            { value: 'content', label: 'Content', build: () => content(concept, where, () => showConcept(view, where, true)) },
            { value: 'links', label: 'Links', build: () => linksTab(where) },
            { value: 'history', label: 'History', build: () => historyTab(view, where) },
            { value: 'raw', label: 'Raw', build: () => rawTab(concept.raw, concept.note) },
        ],
    });
}

async function showDocument(view, where) {
    let documentRead;
    try {
        documentRead = await api.get(`${base(where)}/documents/${encPath(where.path)}`);
    } catch {
        clear(view).append(h('p', { class: 'muted' }, 'This document could not be loaded.'));
        return;
    }
    frame(view, {
        title: documentRead.path,
        subtitle: documentRead.catalog,
        extra: null,
        tabs: [
            { value: 'content', label: 'Content', build: () => content({ body_html: documentRead.body_html, outline: [] }, where, null) },
            { value: 'history', label: 'History', build: () => historyTab(view, where) },
            { value: 'raw', label: 'Raw', build: () => rawTab(documentRead.text, null) },
        ],
    });
}

// "Generated 3 d ago by x - Reviewed ... " plus source and question counts,
// from the frontmatter keys generated, reviewed, sources, open_questions.
function provenance(frontmatter) {
    const fm = frontmatter ?? {};
    const stamps = [];
    for (const [key, label] of [['generated', 'Generated'], ['reviewed', 'Reviewed']]) {
        const stamp = fm[key];
        if (stamp && typeof stamp === 'object' && stamp.at) {
            stamps.push(`${label} ${ago(String(stamp.at))}${stamp.by ? ` by ${stamp.by}` : ''}`);
        }
    }
    const sources = Array.isArray(fm.sources) ? fm.sources.length : 0;
    const questions = Array.isArray(fm.open_questions) ? fm.open_questions.length : 0;
    return h('p', { id: 'provenance', class: 'small muted' },
        stamps.join(' - ') || 'Never stamped', ' ',
        pill(count(sources, 'source'), sources ? 'info' : 'muted'), ' ',
        pill(count(questions, 'open question'), questions ? 'warn' : 'muted'));
}

function outlineNav(outline, where) {
    return h('div', { class: 'outline', id: 'outline', 'aria-label': 'Outline' },
        h('ul', {}, outline.map(entry => h('li', { class: `level-${entry.level}` },
            h('a', { href: browseHash(where.catalog, where.path, entry.slug) }, entry.heading)))));
}

function content(item, where, loadFull) {
    const wrap = h('div', {});
    const outline = item.outline ?? [];
    if (item.body_html == null) {
        wrap.append(
            h('p', { class: 'muted', id: 'concept-truncated' }, item.note ?? 'The body is over the server limit.'),
            outlineNav(outline, where),
            h('button', { type: 'button', id: 'concept-full', onclick: loadFull }, 'Load the whole body'));
        return wrap;
    }
    if (outline.length > 1) wrap.append(outlineNav(outline, where));
    const body = h('article', { class: 'markdown', id: 'markdown-body' });
    // The console's only innerHTML. body_html is produced by src/render.rs,
    // which turns all raw HTML in the source into escaped text, drops style
    // attributes, and keeps only http(s)/mailto links and console routes
    // (javascript:, data: and the like become plain text). Every other
    // value on every page goes through textContent.
    body.innerHTML = item.body_html;
    wrap.append(body);
    if (where.section) {
        // After the pane is attached, so scrolling has a layout to scroll.
        requestAnimationFrame(() => body.querySelector(`[data-slug="${CSS.escape(where.section)}"]`)?.scrollIntoView({ block: 'start' }));
    }
    return wrap;
}

async function linksTab(where) {
    let data;
    try {
        data = await api.get(`${base(where)}/links/${encPath(where.path)}`);
    } catch {
        return h('p', { class: 'muted' }, 'Links could not be loaded.');
    }
    const open = path => go(browseHash(where.catalog, path));
    const out = listTable([
        { key: 'target', label: 'Target', class: 'mono', render: row => row.fragment ? `${row.target}#${row.fragment}` : row.target, sort: row => row.target },
        { key: 'title', label: 'Title' },
        { key: 'line', label: 'Line', sort: row => row.line },
        { key: 'broken', label: 'State', render: row => row.broken ? pill('broken', 'error') : pill('ok', 'ok') },
    ], data.links, { onRow: row => { if (!row.broken) open(row.target); }, empty: 'No links out.' });
    out.id = 'links-out';
    const back = listTable([
        { key: 'path', label: 'From', class: 'mono', sort: row => row.path },
        { key: 'title', label: 'Title' },
        { key: 'line', label: 'Line', sort: row => row.line },
    ], data.backlinks, { onRow: row => open(row.path), empty: 'Nothing links here.' });
    back.id = 'backlinks';
    return h('div', {}, h('h2', {}, 'Links out'), out, h('h2', {}, 'Backlinks'), back);
}

async function historyTab(view, where) {
    let data;
    try {
        data = await api.get(`${base(where)}/history/${encPath(where.path)}`);
    } catch {
        return h('p', { class: 'muted' }, 'History could not be loaded.');
    }
    const table = listTable([
        { key: 'revision', label: 'Rev', sort: row => row.revision },
        { key: 'op', label: 'Op', render: row => pill(row.op, row.op === 'delete' ? 'error' : 'muted') },
        { key: 'actor', label: 'Actor', class: 'mono' },
        { key: 'message', label: 'Message' },
        { key: 'at', label: 'When', render: row => ago(row.at), sort: row => row.at },
    ], data.revisions, { onRow: row => openRevision(view, where, row), empty: 'No revisions.' });
    table.id = 'history';
    return table;
}

// One revision in the drawer: its diff from the revision before, and its text.
// The drawer is outside this page's container, so it opens only while the
// page is still shown (view is inside it, and detached with it).
async function openRevision(view, where, row) {
    let revision;
    try {
        revision = await api.get(`${base(where)}/revisions/${row.revision}/${encPath(where.path)}`);
    } catch {
        return;
    }
    if (!view.isConnected) return;
    const lines = revision.diff.split('\n').map(line => h('span', {
        class: line.startsWith('+') && !line.startsWith('+++') ? 'add'
            : line.startsWith('-') && !line.startsWith('---') ? 'del' : null,
    }, `${line}\n`));
    openDrawer(`${revision.path} @ ${revision.revision}`, `${revision.op} by ${row.actor}, ${when(row.at)}: ${row.message}`,
        h('h3', {}, 'Changes from the previous revision'),
        h('pre', { class: 'diff', id: 'revision-diff' }, lines),
        revision.text === null
            ? h('p', { class: 'muted' }, 'No text at this revision (deleted, or not text).')
            : h('details', {}, h('summary', {}, 'Text at this revision'), h('pre', { class: 'raw', id: 'revision-text' }, revision.text)));
}

function rawTab(raw, note) {
    if (raw == null) return h('p', { class: 'muted' }, note ?? 'The raw text is only returned with the whole body.');
    return h('pre', { class: 'raw', id: 'concept-raw' }, raw);
}
```

> **Corrected (issue #219, 2026-11-10).** Drafted before Task 1's per-navigation containers. `historyTab` and `openRevision` now take `view` (the page's `#browse-view`, inside `main`), and `openRevision` returns without opening the drawer when `view.isConnected` is false after its request, per "Front-end shape". The `#catalog-select` listener stays behind the module-level `switcherHooked` flag: the shell keeps one `<select>` for the page's lifetime (`refreshIdentity` replaces only its options), so one listener serves every render, and the journey `switching_catalogs_while_browsing_opens_the_new_root_once` fails if a listener is added per render. The code above is corrected inline.

`src/http/ui/js/pages/search.js`:

```js
// Search: ranked concepts with snippets, across every catalog the key
// reaches. Route: #/search/<query>, so a search can be linked and reloaded.
import { h, clear } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { go, browseHash } from '../core/router.js';
import { count } from '../core/format.js';

export async function render(main, rest) {
    const query = rest.join('/');
    const catalogs = store.whoami?.catalogs ?? [];
    const input = h('input', {
        type: 'search', id: 'search-query', name: 'q', value: query, autocomplete: 'off',
        placeholder: 'Words that must all occur', 'aria-label': 'Search',
    });
    const form = h('form', {
        id: 'search-form',
        role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            go(text ? `#/search/${enc(text)}` : '#/search');
        },
    }, h('fieldset', { role: 'group' }, input, h('button', { type: 'submit' }, 'Search')));
    const results = h('div', { id: 'search-results' });
    main.append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Search'),
                h('p', { class: 'muted small' }, catalogs.length ? `Across ${catalogs.join(', ')}` : 'This key reaches no catalog.'))),
        form,
        results);
    input.focus();
    if (!query) return;
    results.append(h('p', { class: 'muted' }, 'Searching...'));
    let found;
    try {
        found = await api.get(`/search?q=${enc(query)}&catalogs=${catalogs.map(enc).join(',')}&limit=50`);
    } catch {
        clear(results);
        return;
    }
    clear(results).append(
        h('p', { class: 'muted small', id: 'search-count' }, count(found.hits.length, 'hit')),
        h('ol', { class: 'hits' }, found.hits.map(hit => h('li', { class: 'hit' },
            h('a', { class: 'hit-title', href: browseHash(hit.catalog, hit.path), dataset: { catalog: hit.catalog, path: hit.path } },
                hit.title ?? hit.path),
            h('span', { class: 'mono small muted' }, ` ${hit.catalog} ${hit.path}`),
            hit.description ? h('p', { class: 'small' }, hit.description) : null,
            hit.snippet ? h('p', { class: 'snippet small muted' }, hit.snippet) : null))));
}
```

Append to `src/http/ui/styles.css`. Do not use `<nav>` for the tree or the outline: Pico lays `nav ul` out as a flex row.

```css
/* browse and search (M9 task 4) */
.browse { display: grid; grid-template-columns: 260px minmax(0, 1fr); gap: 1.5rem; align-items: start; }
.browse-tree { position: sticky; top: 1rem; max-height: calc(100vh - 2rem); overflow-y: auto; background: var(--surface);
    border: 1px solid var(--border); border-radius: var(--radius); padding: .75rem; font-size: .85rem; }
.tree ul { list-style: none; margin: 0; padding: 0 0 0 .75rem; }
.tree > ul { padding-left: 0; }
.tree li { list-style: none; margin: 0; padding: 0; }
.tree details { margin: 0; }
.tree summary { cursor: pointer; color: var(--fg-muted); padding: .15rem .4rem; }
.tree-link { display: block; padding: .15rem .4rem; border-radius: var(--radius-sm); color: var(--fg); text-decoration: none;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tree-link:hover { background: var(--surface-2); }
.tree-link.active { background: var(--accent-soft); color: var(--accent); font-weight: 600; }
.outline { border-left: 2px solid var(--border); padding-left: .75rem; margin-bottom: 1rem; font-size: .85rem; }
.outline ul { list-style: none; margin: 0; padding: 0; }
.outline li { list-style: none; margin: 0; }
.outline .level-3 { padding-left: .75rem; }
.outline .level-4, .outline .level-5, .outline .level-6 { padding-left: 1.5rem; }
.markdown { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); padding: 1.25rem 1.5rem; }
.markdown pre { background: var(--surface-2); padding: .75rem; border-radius: var(--radius-sm); overflow-x: auto; }
.markdown table { width: auto; }
.markdown a[title="Not in this catalog"] { color: var(--error); text-decoration: line-through; }
pre.raw { white-space: pre-wrap; word-break: break-word; background: var(--surface-2); padding: .75rem;
    border-radius: var(--radius-sm); font-size: .8rem; }
pre.diff .add { color: var(--ok); }
pre.diff .del { color: var(--error); }
.hits { padding-left: 1.25rem; }
.hit { margin-bottom: .9rem; }
.hit-title { font-weight: 600; }
.hit p { margin: .15rem 0 0; }
.snippet { font-family: var(--font-mono); }
@media (max-width: 960px) {
    .browse { grid-template-columns: 1fr; }
    .browse-tree { position: static; max-height: none; }
}
```

- [ ] **Step 8: Browser journeys (failing first)**

`tests/ui_e2e/browse.rs`:

```rust
//! Browse and Search (M9 Task 4): the tree, a rendered concept and a link
//! in its body, raw HTML shown as text, the Links and History tabs with a
//! revision's diff, and search.

use chrono::Utc;
use okf_mcp::archive::Files;
use okf_mcp::config::Config;
use okf_mcp::store::import::{import, ImportOptions, ImportScope};
use okf_mcp::store::keys::Access;
use serde_json::json;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

/// `systems/alpha.md` as first imported.
const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\n\
                     Alpha runs [Beta](beta.md).\n\n## Config\n\nNothing yet.\n";

/// A concept that tries to run script three ways.
const HOSTILE: &str = "---\ntype: note\ntitle: Hostile\n---\n# Hostile\n\n\
                       <script>window.owned = true</script>\n\n\
                       Inline <img src=x onerror=\"window.owned = true\"> and \
                       [a link](javascript:window.owned=true).\n";

/// The synthetic `main` catalog, with `alpha` as given.
fn main_catalog(alpha: &str) -> Files {
    files(&[
        (
            "index.md",
            "# Main\n\n- [Alpha](systems/alpha.md): the first system\n",
        ),
        ("log.md", "# Log\n"),
        ("AGENTS.md", "# Agents\n"),
        (
            "systems/index.md",
            "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n",
        ),
        ("systems/alpha.md", alpha),
        (
            "systems/beta.md",
            "---\ntype: system\ntitle: Beta\n---\n# Beta\n\nBeta.\n",
        ),
        ("notes/hostile.md", HOSTILE),
    ])
}

/// A server holding `main` and a read key on it.
async fn server() -> (TestServer, String) {
    let server = TestServer::start(vec![("main", main_catalog(ALPHA))], Config::default()).await;
    let key = server
        .key("browse-reader", Access::Read, &["main"], Some("main"))
        .await;
    (server, key)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tree_opens_a_concept_and_a_link_in_its_body_opens_the_next() {
    let (server, key) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/browse/main");
        ui.wait_text("#concept-title", "/index.md");
        ui.wait_visible("#markdown-body");
        let folder_open = |ui: &crate::harness::Ui| {
            ui.eval(
                "() => document.querySelector(\"[data-folder='systems']\").open",
                json!(null),
            )
        };
        assert_eq!(folder_open(ui), json!(false), "folders start closed");
        ui.click("[data-folder='systems'] > summary");
        assert_eq!(folder_open(ui), json!(true));
        ui.click("[data-path='/systems/alpha.md']");
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_visible("#markdown-body h2[data-slug='config']");
        assert_eq!(ui.text("#markdown-body h1"), "Alpha");
        assert!(ui.text("#provenance").contains("0 sources"));
        assert!(
            !ui.text("#markdown-body").contains("type: system"),
            "no frontmatter"
        );
        ui.click("#markdown-body a[href='#/browse/main/systems/beta.md']");
        ui.wait_text("#concept-title", "Beta");
        assert_eq!(
            ui.eval("() => location.hash", json!(null)),
            json!("#/browse/main/systems/beta.md")
        );
        // A section route opens the concept at that heading.
        ui.open("/browse/main/systems/alpha.md/config");
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_visible("#outline a[href='#/browse/main/systems/alpha.md/config']");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raw_html_and_script_links_in_a_concept_render_as_text() {
    let (server, key) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/browse/main/notes/hostile.md");
        ui.wait_text("#markdown-body", "<script>window.owned = true</script>");
        assert!(ui.text("#markdown-body").contains("<img src=x"));
        let injected = ui.eval(
            "() => document.querySelectorAll('#markdown-body script, #markdown-body img, \
             #markdown-body [onerror], #markdown-body a[href^=\"javascript\"]').length",
            json!(null),
        );
        assert_eq!(injected, json!(0));
        assert_eq!(
            ui.eval("() => window.owned === undefined", json!(null)),
            json!(true)
        );
        // drive() then fails the journey on any console error, CSP
        // violations included.
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_links_tab_shows_backlinks_and_a_revision_opens_its_diff() {
    let (server, key) = server().await;
    // A second revision of alpha, as a replacing import writes it.
    let options = ImportOptions {
        replace: true,
        scope: ImportScope::Everything,
        expect_generation: None,
        actor: "editor",
        now: Utc::now(),
    };
    let edited = ALPHA.replace("Nothing yet.", "Two settings.");
    import(&server.db.pool, "main", main_catalog(&edited), &options)
        .await
        .expect("import the edit");
    server.app.poll().await.expect("load the edit");
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/browse/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        ui.click("#concept-tabs [data-value='links']");
        ui.wait_text("#backlinks", "/systems/alpha.md");
        // The row from alpha (CSS cannot select by text, so a script clicks it).
        ui.eval(
            "() => [...document.querySelectorAll('#backlinks tbody tr')]\
             .find(row => row.textContent.includes('/systems/alpha.md')).click()",
            json!(null),
        );
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_text("#markdown-body", "Two settings.");
        ui.click("#concept-tabs [data-value='history']");
        ui.wait_text("#history", "editor");
        ui.click("#history tbody tr:first-child");
        ui.wait_visible("#drawer");
        let diff = ui.wait_text("#revision-diff", "+Two settings.");
        assert!(diff.contains("-Nothing yet."), "{diff}");
        ui.click("#drawer-close");
        ui.wait_hidden("#drawer");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_finds_alpha_and_opens_it() {
    let (server, key) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/search");
        ui.fill("#search-query", "alpha");
        ui.click("#search-form button[type=submit]");
        ui.wait_text("#search-count", "hit");
        assert!(ui
            .text("#search-results")
            .contains("main /systems/alpha.md"));
        ui.click("#search-results a[data-path='/systems/alpha.md']");
        ui.wait_text("#concept-title", "Alpha");
    })
    .await;
}
```

`tests/ui_e2e/main.rs`: add `mod browse;` (keep the other tasks' lines).

Run, before Step 7's files exist: `cargo test --test ui_e2e browse::`
Expected: FAIL. Without `js/pages/browse.js` in the manifest, the router shows "Not available yet", so `#concept-title` never appears and each journey times out on its first wait.

- [ ] **Step 9: Run the journeys**

Run: `just test-ui`
Expected: the four browse journeys pass with no skip line: `the_tree_opens_a_concept_and_a_link_in_its_body_opens_the_next`, `raw_html_and_script_links_in_a_concept_render_as_text`, `the_links_tab_shows_backlinks_and_a_revision_opens_its_diff` and `search_finds_alpha_and_opens_it`.

> **Corrected (issue #219, 2026-11-10).** Five browse journeys: `tests/ui_e2e/browse.rs` also has `switching_catalogs_while_browsing_opens_the_new_root_once` (a key on `main` and `other`; after three Browse renders, one switch to `other` opens `#/browse/other` and fetches its tree exactly once). Tasks 2 and 3 merged first and already removed the `#[allow(dead_code)]` attributes (see the issue #216 notes under Task 1), so this task changes none. `docs/operations.md` also says that switching catalogs while browsing opens the new catalog's root.

> **Corrected (issue #219, 2026-11-10; review of PR #227).** Two navigation fixes:
> - **Section routes:** `render()` calls `showConcept(view, where, where.section !== null)`, corrected inline above. A section route asks for the whole body, so an outline link on a concept over `concept_outline_chars` reaches its heading. The journey `a_section_link_on_a_long_concept_loads_the_body_and_reaches_the_heading` covers it.
> - **Fragments:** in `src/render.rs`, a link's `#fragment` goes through `section_slug` (`py_unquote`, then lowercase, as `src/okf/analysis/links.rs` reads fragments) before `browse_route`, so an encoded fragment is not encoded twice. The path stays literal, as `resolve_link` and the catalog's link checks read it. The unit test is `encoded_fragments_are_decoded_once_and_paths_stay_literal`. Read `src/render.rs` for the code, since the renderer listing above predates it. The other tasks' journeys pass too. `drive()` fails a journey on any console error, so the hostile concept also proves that no CSP violation fired.

- [ ] **Step 10: Docs and changelog**

`docs/operations.md`, section "Web console", add a subsection:

```markdown
### Browse and search

Browse (`#/browse/<catalog>/<path>`) shows a catalog's tree beside one
document, with tabs Content, Links, History and Raw. A history row opens
the revision's diff in a drawer. Search (`#/search/<query>`) searches every
catalog the key reaches.

Concepts are rendered to HTML on the server (`src/render.rs`):

- raw HTML in a document is shown as text, never run;
- links may use only `http`, `https` and `mailto`, or point at a document
  of the same catalog, which opens in the console; every other link
  (`javascript:`, `data:`, a path leaving the catalog, a link to a file
  that is not markdown) is shown as plain text;
- an outside image is shown as a link to it, because the console's CSP
  loads no outside images;
- a link to a missing document is struck through.

A body over `concept_outline_chars` is shown as its outline until you
choose "Load the whole body".

| Method | Path | Access | Notes |
|---|---|---|---|
| GET | `/api/v1/catalogs/{catalog}/tree` | read | Every concept, `index.md` and `log.md`, and the root `AGENTS.md`: `path`, `kind`, `title`. |
| GET | `/api/v1/catalogs/{catalog}/concepts/{path}?full=` | read | `get_concept` with `raw`, plus `body_html` and the whole `outline`. Over `concept_outline_chars` without `full=true`: no `body`, `raw` or `body_html`. |
| GET | `/api/v1/catalogs/{catalog}/links/{path}` | read | Outbound `links` and `backlinks`, as `get_concept` with `links`. |
| GET | `/api/v1/catalogs/{catalog}/history/{path}?limit=` | read | The `history` tool. |
| GET | `/api/v1/catalogs/{catalog}/revisions/{n}/{path}` | read | `text` at revision `n` (null for a deletion or non-text bytes) and a unified `diff` from revision `n-1`. |
| GET | `/api/v1/catalogs/{catalog}/documents/{path}` | read | `read_document` (any `index.md` or `log.md`, the root `AGENTS.md`) plus `body_html`. |
| GET | `/api/v1/search?q=&catalogs=&limit=` | read | The `search` tool. `catalogs` is comma-separated and defaults to every catalog the key reaches; a named catalog it does not reach is a 404. |

The per-document routes put their kind before the path (`/links/{path}`,
not `/concepts/{path}/links`), because the path is a wildcard and must come
last. For example:

    curl -H "Authorization: Bearer $OKF_MCP_KEY" \
        "http://127.0.0.1:8080/api/v1/catalogs/main/revisions/2/systems/alpha.md"
```

`README.md`, in the "Web console" section, add one sentence: "Browse shows a catalog's tree, each concept rendered (raw HTML in a concept is shown as text, never run), its links, history and revision diffs, and Search searches every catalog the key reaches."

`changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m9-task-04-browse-and-search.md`:

```markdown
# M9 Task 4: Browse and search

## Added

- `src/render.rs`: markdown to safe HTML with pulldown-cmark 0.13.6
  (tables, strikethrough, task lists).
  - Raw HTML is shown as text.
  - Links are limited to `http`, `https`, `mailto` and the same catalog's
    documents, which become console routes.
  - Images become links.
  - Heading ids are `section-<slug>`, with the outline's slug in `data-slug`.
- API:
  - `GET /api/v1/catalogs/{catalog}/tree`, `concepts/{path}`,
    `links/{path}`, `history/{path}`, `revisions/{n}/{path}` and
    `documents/{path}`;
  - `GET /api/v1/search`.
- Console pages:
  - Browse: a collapsible tree; a concept view with a provenance line and
    Content, Links, History and Raw tabs; a revision diff in the drawer; a
    folder shows its `index.md`.
  - Search.
- Browser journeys: the tree and links between concepts, raw HTML shown
  as text, backlinks, a history diff, and search.

## Changed

- `unified_diff` in `src/mcp/changesets.rs` is `pub(crate)`, so revision
  diffs reuse it.

## Notes

- An axum wildcard must come last, so the per-document routes are
  `/links/{path}`, `/history/{path}` and `/revisions/{n}/{path}` rather
  than the design's `/concepts/{path}/links` (and so on).
- Without `catalogs`, `/api/v1/search` searches every catalog the key
  reaches.
- Table alignment is dropped and outside images are shown as links,
  because the console's CSP blocks inline styles and outside images.
```

- [ ] **Step 11: Gates and commit**

Run every gate in "Conventions". This task touches `src/mcp/changesets.rs` only to change a visibility, which changes no behaviour, so the parity and roundtrip runs are not needed. Then:

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/render.rs src/mcp/changesets.rs \
    src/http/api/browse.rs src/http/api/search.rs \
    src/http/ui/js/pages/browse.js src/http/ui/js/pages/search.js \
    src/http/ui/js/components/tree.js src/http/ui/styles.css \
    tests/ui_e2e/browse.rs tests/ui_e2e/main.rs README.md docs/operations.md changelog/
git commit -m "feat(console): markdown rendering, Browse and Search pages and their API (M9 task 4)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 5: Catalogs and import/export

Depends on Task 1. Independent of Tasks 2 to 4, 6 and 7.

**Facts this task relies on** (checked at `62e88cd`):
- `src/http/archives.rs:33-49` `routes(app)` builds `get(export_catalog).layer(timeout_layer())` and `post(import_catalog).layer(DefaultBodyLimit::max(limit))` inline, with `limit = body_limit(app.config().max_import_bytes.as_u64())` (:34, :63). The handlers take `Path(name): Path<String>` (:90, :145), so they work under any single-parameter path, whatever the parameter is called.
- `src/http/archives.rs:107-112` `ImportQuery` has only `replace`. `import_catalog` hard-codes `scope: ImportScope::Everything, expect_generation: None` (:177-183), and rewrites *every* `OkfError::Refused` to "add ?replace=true" (:189-196). The test at :441-445 pins that message exactly.
- `src/store/import.rs:146-157`: a wrong `expect_generation` is `OkfError::Refused { reason: "catalog \"x\" is at generation G, not E: it changed since the export; export it again" }`. That is 409 (`src/http/mod.rs:79-101`). The non-empty refusal is "catalog \"x\" is not empty; pass --replace to replace its contents" (:159-163). Once `expect_generation` is passed through, the blanket rewrite at archives.rs:189-196 would mislabel a generation refusal, so it must rewrite only the `pass --replace` text.
- `src/store/import.rs:20-30` `ImportScope { Everything (default), Machinery }`; `:36-48` `ImportOptions { replace, scope, expect_generation: Option<i64>, actor, now }`; `:52-64` `ImportReport { added, updated, deleted: Vec<String>, unchanged: usize, lint: LintReport }` (Serialize). `LintReport { errors, warnings: Vec<Finding { file, line, rule, message }> }` (`src/okf/analysis/lint.rs:100`, `src/okf/analysis/mod.rs:21-31`).
- `src/cli.rs:66-87`: the CLI's `import` has `--only-machinery` (it `requires = "replace"`) and `--expect-generation N`.
- `OkfError::ImportRefused { problems }` is 400, and its JSON carries `problems` (`src/error.rs:95`, `:336`). `not_an_archive` turns any bytes that are not gzip into `ImportRefused` (`src/archive.rs:338-342`).
- `src/store/catalogs.rs`:
  - `create(pool, name, description, now) -> Catalog` (:60) answers `Refused` for an invalid name (:66-69) or a taken one (:84-88).
  - `update_description(pool, name, description) -> previous` (:114) answers `CatalogNotSelected` for an unknown catalog.
  - `find` (:171).
  - `delete(pool, name, force)` (:207) answers `Refused` "catalog \"x\" has unpublished changes; pass --force to delete them too" (:240-245).
  - No catalog function records an actor, and the CLI does not either (`src/cli.rs:440-469`).
- `Catalog { id, name, description, generation, created_at }` (`src/store/catalogs.rs:18-29`) is not `Serialize`.
- `CatalogState::description() -> Arc<str>` (`src/catalog.rs:167`) and `current() -> Arc<Loaded>` (:187). `Loaded::generation()` (:43), `snapshot()` (:48), `files()` (:59), `last_change() -> Option<DateTime<Utc>>` (:64). `Snapshot::concepts()` (`src/okf/snapshot.rs:317`) is an iterator over `DocKind::Concept` docs (`index.md` and `log.md` are `Index`/`Log`, and `AGENTS.md` is `Machinery`: :70-78).
- `crate::clock::format_timestamp(DateTime<Utc>) -> String` (`src/clock.rs:56`), as `list_catalogs` uses it (`src/mcp/catalogs.rs:62`). `mcp::catalogs::list` returns no generation and no concept count (:15-28), so this task builds its own summary from the same `CatalogState`.
- `Reach { All, Catalogs(BTreeSet<String>) }` and `Principal::reaches` (`src/auth.rs:25-30`, :57). `require` (:75).
- `App::poll()` (`src/app.rs:249`) loads new catalog rows, drops deleted ones and sets each description (:280-283).
- `src/mcp/testing.rs`:
  - `app_with` creates every catalog with description `""` (:76-78).
  - `writer(test, key_id, default) -> ToolContext` (:187), `open(&ctx) -> handle` (:207).
  - `PendingEdit { path, text: Option<&str>, at }` (:268) and `pending_entry(test, handle, &edit)` (:290) store an unpublished entry.
- `documents.content` is `bytea` (`migrations/0001_schema.sql:27`).
- `tests/support/server.rs`:
  - `TestServer.app: Arc<App>` is public (:65).
  - `key(id, access, &[], default)` grants every catalog (`Grants::All`, :186).
- `okf_mcp::archive::{write_tarball, ExportFile}` are public (`src/archive.rs:70`, :281). `base64` 0.23.1 is a normal dependency (`Cargo.toml:26`; `src/keys.rs:5-8` uses `base64::engine::general_purpose` and `base64::Engine`).
- matchit (axum's router) refuses two routes whose parameter at the same position has different names, so every `/api/v1/catalogs/{...}` route in every area must say `{catalog}` (the plan's convention).

**Files:**
- Modify: `src/http/archives.rs` (route builders shared with `/api/v1`, `ImportQuery` options, the `Refused` rewrite, the test tarball helper, two tests)
- Modify: `src/http/api/catalogs.rs` (replaces Task 1's stub)
- Create: `src/http/ui/js/pages/catalogs.js`, `src/http/ui/js/pages/transfer.js`, `src/http/ui/js/components/upload.js`
- Modify: `src/http/ui/styles.css` (append the Task 5 block)
- Create: `tests/ui_e2e/catalogs.rs`; Modify: `tests/ui_e2e/main.rs` (`mod catalogs;`)
- Modify: `README.md`, `docs/operations.md`
- Create: `changelog/<ts>-m9-task-05-catalogs-transfer.md`

- [ ] **Step 1: Write the failing archive-route tests (old paths)**

In `src/http/archives.rs`, add this helper at module level, directly above `#[cfg(test)] mod tests`. It lives outside the test module so `http::api::catalogs`'s tests can use it too:

```rust
/// A valid `.tar.gz` of `entries` (`(path, text)` pairs, none executable),
/// for the tests here and in `http::api`.
///
/// # Panics
/// If the archive cannot be written (it is built in memory, so it can't).
#[cfg(test)]
pub(crate) fn tarball_of(entries: &[(&str, &str)]) -> Vec<u8> {
    use crate::archive::{write_tarball, ExportFile};
    let modified = chrono::Utc::now();
    let files: Vec<ExportFile<'_>> = entries
        .iter()
        // `&(path, text)` destructures each borrowed pair, copying out the
        // two `&str`s (which are `Copy`).
        .map(|&(path, text)| ExportFile {
            path,
            content: text.as_bytes(),
            executable: false,
            modified,
        })
        .collect();
    write_tarball(Vec::new(), &files).expect("an in-memory tarball")
}
```

In the test module:
- Delete the line `use crate::archive::{write_tarball, ExportFile};`.
- Replace the body of `fn tarball()` with `tarball_of(&[("index.md", "# Index\n")])`.
- Add:

```rust
    /// An admin's import into `test`'s `pg`, with `query` ("" or "?...").
    async fn import_with(
        test: &TestApp,
        query: &str,
        archive: Vec<u8>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::post(format!("/catalogs/pg/import{query}"))
            .body(Body::from(archive))
            .expect("request");
        status_and_json(router(test, admin()).oneshot(request).await.unwrap()).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_machinery_replaces_machinery_and_keeps_the_concepts() {
        let test = app_with(
            &[(
                "pg",
                &[("index.md", "# Index\n"), ("AGENTS.md", "# Old\n"), ("a.md", "# A\n")],
            )],
            Config::default(),
        )
        .await;
        let archive = tarball_of(&[("AGENTS.md", "# New\n"), ("a.md", "# Overwritten\n")]);

        let (status, report) =
            import_with(&test, "?replace=true&only_machinery=true", archive).await;
        let kept: Vec<u8> = sqlx::query_scalar(
            "SELECT content FROM documents WHERE path = 'a.md' \
             AND catalog_id = (SELECT id FROM catalogs WHERE name = 'pg')",
        )
        .fetch_one(test.app.pool())
        .await
        .unwrap();

        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["updated"], serde_json::json!(["AGENTS.md"]));
        assert_eq!(report["added"], serde_json::json!([]));
        assert_eq!(report["deleted"], serde_json::json!([]));
        assert_eq!(kept, b"# A\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_expecting_another_generation_is_409_and_imports_nothing() {
        let test = app_with(&[("pg", &[("index.md", "# Index\n")])], Config::default()).await;
        let before = stored(&test).await;
        let stale = before.0 + 1;
        let archive = || tarball_of(&[("index.md", "# Changed\n")]);

        let (status, refused) = import_with(
            &test,
            &format!("?replace=true&expect_generation={stale}"),
            archive(),
        )
        .await;
        let after_refusal = stored(&test).await;
        let (current_status, report) = import_with(
            &test,
            &format!("?replace=true&expect_generation={}", before.0),
            archive(),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(refused["error"], "Refused");
        let message = refused["message"].as_str().unwrap();
        // The generation refusal keeps its own words; only the "not empty"
        // refusal is reworded for HTTP.
        assert!(message.contains(&format!("generation {}, not {stale}", before.0)), "{message}");
        assert!(message.contains("export it again"), "{message}");
        assert_eq!(after_refusal, before);
        assert_eq!(current_status, StatusCode::OK);
        assert_eq!(report["updated"], serde_json::json!(["index.md"]));
    }
```

- [ ] **Step 2: Write the failing `/api/v1` catalog tests**

These go at the bottom of `src/http/api/catalogs.rs`. The implementation from Step 4 goes above them.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use chrono::Utc;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::http::archives::tarball_of;
    use crate::mcp::testing::{open, pending_entry, writer, PendingEdit, TestApp};
    use crate::store::keys::{self as key_rows, Grants, NewKey};

    /// An admin key that reaches every catalog, the only kind that may
    /// create one. It is polled in, like `testing::keys`.
    async fn every_catalog_admin(test: &TestApp) -> String {
        let new = NewKey {
            name: "root",
            access: Access::Admin,
            maintenance_report: false,
            grants: Grants::All,
            default_catalog: Some("main"),
            actor: "root",
        };
        let key = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
            .await
            .expect("create the key")
            .key;
        test.app.poll().await.expect("poll the key in");
        key
    }

    /// One request with a raw body through the whole router; the status,
    /// the headers and the body bytes.
    async fn raw(
        test: &TestApp,
        method: &str,
        uri: &str,
        key: &str,
        body: Vec<u8>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {key}"))
            .body(Body::from(body))
            .expect("a valid request");
        let response = crate::http::router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the body");
        (status, headers, bytes.to_vec())
    }

    /// The names a `GET /catalogs` lists for `key`.
    async fn names(test: &TestApp, key: &str) -> Vec<String> {
        let (status, body) = send(test, "GET", "/api/v1/catalogs", Some(key), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["catalogs"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|catalog| catalog["name"].as_str().expect("a name").to_string())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_catalog_routes_follow_the_access_matrix() {
        let test = app().await;
        // The delete runs last: it removes `main`. Every key in the matrix
        // reaches one catalog only, so none may create one (all 403).
        assert_matrix(
            &test,
            &[
                Row { method: "GET", uri: "/api/v1/catalogs".into(), body: None,
                      read: 200, write: 200, admin: 200, stranger: 200 },
                Row { method: "GET", uri: "/api/v1/catalogs/main/export".into(), body: None,
                      read: 200, write: 200, admin: 200, stranger: 404 },
                // `{}` is not gzip: an admin gets past the key check to 400.
                Row { method: "POST", uri: "/api/v1/catalogs/main/import".into(),
                      body: Some(json!({})),
                      read: 403, write: 403, admin: 400, stranger: 404 },
                Row { method: "POST", uri: "/api/v1/catalogs".into(),
                      body: Some(json!({ "name": "made-in-matrix" })),
                      read: 403, write: 403, admin: 403, stranger: 403 },
                Row { method: "PATCH", uri: "/api/v1/catalogs/main".into(),
                      body: Some(json!({ "description": "patched" })),
                      read: 403, write: 403, admin: 200, stranger: 404 },
                Row { method: "DELETE", uri: "/api/v1/catalogs/main?force=true".into(), body: None,
                      read: 403, write: 403, admin: 200, stranger: 404 },
            ],
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_list_shows_reachable_catalogs_with_counts_and_generation() {
        let test = app().await;
        let keys = keys(&test).await;
        let generation = catalog_rows::find(test.app.pool(), "main").await.expect("main").generation;

        let (status, body) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;

        assert_eq!(status, StatusCode::OK);
        let main = &body["catalogs"][0];
        assert_eq!(body["catalogs"].as_array().map(Vec::len), Some(1));
        assert_eq!(main["name"], "main");
        assert_eq!(main["description"], "");
        assert_eq!(main["files"], 6);
        // systems/alpha.md and systems/beta.md; index, log and AGENTS.md are not concepts.
        assert_eq!(main["concepts"], 2);
        assert_eq!(main["generation"], generation);
        assert!(main["last_change"].is_string());
        assert_eq!(names(&test, &keys.stranger).await, ["other"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_every_catalog_admin_creates_a_catalog_that_is_served_at_once() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        let new = json!({ "name": "scratch", "description": "Scratch space" });

        let (status, created) =
            send(&test, "POST", "/api/v1/catalogs", Some(&root), Some(new.clone())).await;
        let (again, taken) = send(&test, "POST", "/api/v1/catalogs", Some(&root), Some(new)).await;
        let (invalid, _) = send(&test, "POST", "/api/v1/catalogs", Some(&root),
                                Some(json!({ "name": "Not Valid" }))).await;
        let (scoped, refused) = send(&test, "POST", "/api/v1/catalogs", Some(&keys.admin),
                                     Some(json!({ "name": "mine" }))).await;

        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["name"], "scratch");
        assert_eq!(created["description"], "Scratch space");
        assert_eq!(created["files"], 0);
        // Polled at once: loaded, and listed for a key that reaches it.
        assert!(test.app.catalog("scratch").is_some());
        assert_eq!(names(&test, &root).await, ["main", "other", "scratch"]);
        assert_eq!(names(&test, &keys.read).await, ["main"]);
        assert_eq!(again, StatusCode::CONFLICT);
        assert!(taken["message"].as_str().unwrap().contains("already exists"));
        assert_eq!(invalid, StatusCode::CONFLICT);
        assert_eq!(scoped, StatusCode::FORBIDDEN);
        assert!(refused["message"].as_str().unwrap().contains("every catalog"), "{refused}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_changes_a_description_and_the_list_shows_it() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, changed) = send(&test, "PATCH", "/api/v1/catalogs/main", Some(&keys.admin),
                                     Some(json!({ "description": "The main catalog" }))).await;
        let (_, listed) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(changed, json!({ "name": "main", "previous": "",
                                    "description": "The main catalog" }));
        assert_eq!(listed["catalogs"][0]["description"], "The main catalog");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_unpublished_work_unless_forced() {
        let test = app().await;
        let keys = keys(&test).await;
        let ctx = writer(&test, "drafter", "main").await;
        let handle = open(&ctx).await;
        let edit = PendingEdit { path: "systems/gamma.md", text: Some("# Gamma\n"), at: Utc::now() };
        pending_entry(&test, &handle, &edit).await;

        let (refused, body) =
            send(&test, "DELETE", "/api/v1/catalogs/main", Some(&keys.admin), None).await;
        let still_there = test.app.catalog("main").is_some();
        let (forced, deleted) =
            send(&test, "DELETE", "/api/v1/catalogs/main?force=true", Some(&keys.admin), None).await;

        assert_eq!(refused, StatusCode::CONFLICT);
        assert!(body["message"].as_str().unwrap().contains("add ?force=true"), "{body}");
        assert!(still_there);
        assert_eq!(forced, StatusCode::OK);
        assert_eq!(deleted, json!({ "name": "main" }));
        assert!(test.app.catalog("main").is_none());
        assert!(names(&test, &keys.read).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn export_and_import_under_api_v1_share_the_archive_routes() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, headers, bytes) =
            raw(&test, "GET", "/api/v1/catalogs/main/export", &keys.read, Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "application/gzip");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(bytes[..2], [0x1f, 0x8b], "gzip magic");

        // Machinery only: AGENTS.md changes, the concepts stay.
        let machinery = tarball_of(&[("AGENTS.md", "# Agents, new\n")]);
        let (status, _, body) = raw(&test, "POST",
            "/api/v1/catalogs/main/import?replace=true&only_machinery=true",
            &keys.admin, machinery).await;
        let report: serde_json::Value = serde_json::from_slice(&body).expect("a report");
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["updated"], json!(["AGENTS.md"]));
        assert_eq!(report["deleted"], json!([]));

        let generation = catalog_rows::find(test.app.pool(), "main").await.expect("main").generation;
        let archive = || tarball_of(&[("index.md", "# Main\n\nImported.\n")]);
        let stale = format!("/api/v1/catalogs/main/import?replace=true&expect_generation={}",
                            generation + 1);
        let (status, _, body) = raw(&test, "POST", &stale, &keys.admin, archive()).await;
        let refused: serde_json::Value = serde_json::from_slice(&body).expect("an error");
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(refused["message"].as_str().unwrap().contains("export it again"), "{refused}");

        let current = format!("/api/v1/catalogs/main/import?replace=true&expect_generation={generation}");
        let (status, _, body) = raw(&test, "POST", &current, &keys.admin, archive()).await;
        let report: serde_json::Value = serde_json::from_slice(&body).expect("a report");
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["updated"], json!(["index.md"]));
        // The snapshot was swapped in before the answer.
        let (_, listed) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;
        assert_eq!(listed["catalogs"][0]["generation"], generation + 1);
        assert_eq!(listed["catalogs"][0]["files"], 1);
    }
}
```

- [ ] **Step 3: Run the tests to make sure they fail**

Run: `cargo test --lib http:: -- --test-threads=6 2>&1 | tail -20`
Expected: it does not compile. `tarball_of`, `export_route` and `import_route` don't exist yet, and neither do `catalog_rows`, `Access` or the handlers in `api::catalogs`. (If Step 1 alone is applied, the archive tests compile but fail: `only_machinery` is ignored, so `deleted` lists `index.md` and `a.md`. `expect_generation` is ignored too, so the stale import answers 200.)

- [ ] **Step 4: Implement**

`src/http/archives.rs`. Replace `routes` with these three functions, keeping `body_limit` as it is:

```rust
/// The two routes, for the keyed part of the router.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// A router over `Arc<App>` state: the export behind the request timeout,
/// the import with its body cap (and its own deadline, see
/// [`import_catalog`]).
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{name}/export", export_route())
        .route("/catalogs/{name}/import", import_route(app))
}

/// The export route's method router: [`export_catalog`] behind the request
/// timeout (503 after [`REQUEST_TIMEOUT`]). Shared with `/api/v1`, so both
/// paths behave the same; mount it on a path with one parameter, the catalog.
///
/// # Returns
/// The `GET` method router.
pub(crate) fn export_route() -> MethodRouter<Arc<App>> {
    // A layer on one method router applies to that route only.
    get(export_catalog).layer(timeout_layer())
}

/// The import route's method router: [`import_catalog`] with the body cap
/// [`body_limit`] computes from `max_import_bytes`, and no timeout layer.
/// One that fired after the import committed would answer 503 for an import
/// that happened (and a retry would get 409), so the handler bounds the
/// upload itself. Shared with `/api/v1`; mount it on a path with one
/// parameter, the catalog.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// The `POST` method router.
pub(crate) fn import_route(app: &App) -> MethodRouter<Arc<App>> {
    let limit = body_limit(app.config().max_import_bytes.as_u64());
    post(import_catalog).layer(DefaultBodyLimit::max(limit))
}
```

Change the routing import to `use axum::routing::{get, post, MethodRouter};`.

Replace `ImportQuery`:

```rust
/// An import's query: `?replace=true&only_machinery=true&expect_generation=N`,
/// the HTTP spelling of the CLI's `--replace`, `--only-machinery` and
/// `--expect-generation N`. Booleans are `true` or `false`; anything else is
/// axum's 400.
#[derive(Debug, Default, Deserialize)]
struct ImportQuery {
    /// Replace a non-empty catalog's documents.
    #[serde(default)]
    replace: bool,
    /// Replace only repository machinery ([`ImportScope::Machinery`]); the
    /// archive's concepts are ignored and the catalog's own are kept. A
    /// non-empty catalog also needs `replace`, as with the CLI.
    #[serde(default)]
    only_machinery: bool,
    /// Refuse (409) unless the catalog is still at this generation, the one
    /// its export had. A missing `Option` field deserializes as `None`.
    expect_generation: Option<i64>,
}
```

In `import_catalog`:
- In the doc comment, change `` `?replace=true`, if given `` to `` the import options ``.
- In its Errors paragraph, change "409 ([`OkfError::Refused`]) for a non-empty catalog without `?replace=true`" to "409 ([`OkfError::Refused`]) for a non-empty catalog without `?replace=true`, or one no longer at `expect_generation`".
- Replace the options and the error mapping:

```rust
    let options = ImportOptions {
        replace: query.replace,
        scope: if query.only_machinery {
            ImportScope::Machinery
        } else {
            ImportScope::Everything
        },
        expect_generation: query.expect_generation,
        actor: &principal.actor,
        now: app.clock().now(),
    };
    let report = import(app.pool(), &name, files, &options)
        .await
        // `store::import`'s "not empty" refusal names the CLI flag: say what
        // HTTP needs instead. Its generation refusal ("... export it again")
        // already fits and passes through unchanged.
        .map_err(|error| match error {
            OkfError::Refused { reason } => OkfError::Refused {
                reason: reason.replace("pass --replace", "add ?replace=true"),
            },
            other => other,
        })?;
```

The existing `a_refresh_failing_after_a_committed_import_still_reports_the_import` still expects "catalog \"pg\" is not empty; add ?replace=true to replace its contents". The `replace` call produces exactly that.

`src/http/api/catalogs.rs` (whole file above the tests from Step 2):

```rust
//! `/api/v1/catalogs` (web console design, "API": Catalogs, Import /
//! export): the catalogs a key reaches, their administration as
//! `okf-mcp catalog create|update|delete` does it, and the archive routes
//! mounted again under `/api/v1` with the same handlers, body cap and
//! deadline as `/catalogs/{name}/export|import`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use super::require_admin;
use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::catalog::CatalogState;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::{archives, AppError};
use crate::store::catalogs as catalog_rows;
use crate::store::keys::Access;

/// This area's routes.
///
/// # Arguments
/// * `app` - the application (the import route's body cap comes from its config).
///
/// # Returns
/// The router. Every path parameter is `{catalog}`, as in the other areas:
/// axum refuses two routes that name the same position differently.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    Router::new()
        // `get(a).post(b)` is one method router: one path, a handler per method.
        .route("/catalogs", get(list_catalogs).post(create_catalog))
        .route("/catalogs/{catalog}", patch(update_catalog).delete(delete_catalog))
        .route("/catalogs/{catalog}/export", archives::export_route())
        .route("/catalogs/{catalog}/import", archives::import_route(app))
}

/// One catalog as the console lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSummary {
    /// Its name.
    pub name: String,
    /// Its description.
    pub description: String,
    /// How many files it holds (every stored file).
    pub files: usize,
    /// How many of them are concepts.
    pub concepts: usize,
    /// The generation of the loaded snapshot: what an import's
    /// `expect_generation` compares against.
    pub generation: i64,
    /// Its newest change (`2026-11-02T21:00:00Z`), or null when empty.
    pub last_change: Option<String>,
}

impl CatalogSummary {
    /// The summary of one loaded catalog.
    ///
    /// # Arguments
    /// * `name` - its name.
    /// * `state` - its state.
    fn of(name: &str, state: &CatalogState) -> Self {
        // `current()` is an `Arc` of the loaded snapshot: one consistent
        // generation, file count and concept count.
        let loaded = state.current();
        Self {
            name: name.to_string(),
            description: state.description().to_string(),
            files: loaded.files(),
            concepts: loaded.snapshot().concepts().count(),
            generation: loaded.generation(),
            last_change: loaded.last_change().map(format_timestamp),
        }
    }
}

/// `GET /catalogs`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogListing {
    /// The catalogs the key reaches, by name.
    pub catalogs: Vec<CatalogSummary>,
}

/// `POST /catalogs`' body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewCatalog {
    /// `[a-z0-9-]+`.
    name: String,
    /// Free text; empty when left out, as with the CLI.
    #[serde(default)]
    description: String,
}

/// `PATCH /catalogs/{catalog}`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescriptionChange {
    /// The new description (empty is allowed).
    description: String,
}

/// `PATCH /catalogs/{catalog}`'s answer, as `okf-mcp catalog update` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DescriptionChanged {
    /// The catalog.
    pub name: String,
    /// The description it had.
    pub previous: String,
    /// The description it has now.
    pub description: String,
}

/// `DELETE /catalogs/{catalog}`'s query.
#[derive(Debug, Default, Deserialize)]
struct DeleteQuery {
    /// Delete even with unpublished changes in an open changeset.
    #[serde(default)]
    force: bool,
}

/// `DELETE /catalogs/{catalog}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogDeleted {
    /// The catalog that was deleted.
    pub name: String,
}

/// `GET /catalogs`: every loaded catalog the key reaches, any access level.
///
/// # Returns
/// The listing, by name (`Catalogs` is a `BTreeMap`, so it iterates sorted).
async fn list_catalogs(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<CatalogListing>, AppError> {
    // Held in a local so the map outlives the iteration that borrows it.
    let loaded = app.catalogs();
    let catalogs = loaded
        .iter()
        .filter(|(name, _)| principal.reaches(name))
        .map(|(name, state)| CatalogSummary::of(name, state))
        .collect();
    Ok(Json(CatalogListing { catalogs }))
}

/// `POST /catalogs`: creates an empty catalog, then polls so it is served at
/// once. It needs an admin key that reaches every catalog: a key granted
/// named catalogs would not reach the new one, and grants are made with the
/// keys routes.
///
/// # Returns
/// 201 with the new catalog's summary.
///
/// # Errors
/// 403 for any other key; 409 ([`OkfError::Refused`]) for an invalid or
/// taken name; 500 for a database failure.
async fn create_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    // `Json` reads the body, so it must be the last extractor.
    Json(new): Json<NewCatalog>,
) -> Result<(StatusCode, Json<CatalogSummary>), AppError> {
    require_every_catalog(&principal)?;
    let row = catalog_rows::create(app.pool(), &new.name, &new.description, app.clock().now())
        .await?;
    // Catalog rows record no actor (nor does the CLI); the log line does.
    tracing::info!(catalog = %row.name, key = %principal.key_id, actor = %principal.actor,
                   "catalog created");
    settle(&app).await;
    // Built from the row, not the loaded state: a new catalog is empty, and
    // the answer must not depend on the poll having loaded it.
    let summary = CatalogSummary {
        name: row.name,
        description: row.description,
        files: 0,
        concepts: 0,
        generation: row.generation,
        last_change: None,
    };
    Ok((StatusCode::CREATED, Json(summary)))
}

/// `PATCH /catalogs/{catalog}`: replaces the description (`admin` on the
/// catalog), then polls so the listing shows it at once.
///
/// # Errors
/// 404 for a catalog the key does not reach or that does not exist; 403
/// below `admin`; 500 for a database failure.
async fn update_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Json(change): Json<DescriptionChange>,
) -> Result<Json<DescriptionChanged>, AppError> {
    principal.require(Access::Admin, &catalog)?;
    let previous =
        catalog_rows::update_description(app.pool(), &catalog, &change.description).await?;
    tracing::info!(%catalog, key = %principal.key_id, actor = %principal.actor,
                   "catalog description changed");
    settle(&app).await;
    Ok(Json(DescriptionChanged {
        name: catalog,
        previous,
        description: change.description,
    }))
}

/// `DELETE /catalogs/{catalog}[?force=true]`: deletes the catalog with its
/// documents, history, changesets and grants (`admin` on the catalog), then
/// polls so it is gone at once.
///
/// # Errors
/// 404 for a catalog the key does not reach or that does not exist; 403
/// below `admin`; 409 ([`OkfError::Refused`]) with unpublished changes and
/// no `force`; 500 for a database failure.
async fn delete_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<CatalogDeleted>, AppError> {
    principal.require(Access::Admin, &catalog)?;
    catalog_rows::delete(app.pool(), &catalog, query.force)
        .await
        // The store's hint names the CLI flag; say what HTTP needs.
        .map_err(|error| match error {
            OkfError::Refused { reason } => OkfError::Refused {
                reason: reason.replace("pass --force", "add ?force=true"),
            },
            other => other,
        })?;
    tracing::info!(%catalog, key = %principal.key_id, actor = %principal.actor,
                   force = query.force, "catalog deleted");
    settle(&app).await;
    Ok(Json(CatalogDeleted { name: catalog }))
}

/// Refuses all but an admin key that reaches every catalog.
///
/// # Errors
/// [`OkfError::Forbidden`] below `admin`, or for a key granted named catalogs.
fn require_every_catalog(principal: &Principal) -> Result<(), OkfError> {
    require_admin(principal)?;
    match &principal.reach {
        Reach::All => Ok(()),
        Reach::Catalogs(_) => Err(OkfError::Forbidden {
            reason: "creating a catalog needs an admin key that reaches every catalog \
                     (okf-mcp key create --all)"
                .to_string(),
        }),
    }
}

/// Polls after an administrative change, so the key cache and the catalog
/// list reflect it for the next request. The change has committed, so a
/// failed poll is logged, not returned: the poll loop retries it.
///
/// # Arguments
/// * `app` - the application. An `&Arc<App>` argument coerces to `&App`
///   automatically (deref coercion).
async fn settle(app: &App) {
    if let Err(error) = app.poll().await {
        tracing::error!(%error, "poll after a catalog change failed; the poll loop retries");
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib http:: -- --test-threads=6`
Expected: PASS. That includes the new `archives::tests::only_machinery_replaces_machinery_and_keeps_the_concepts`, `archives::tests::an_import_expecting_another_generation_is_409_and_imports_nothing`, and every existing archive test (the `a_refresh_failing_...` message among them). It also includes `api::catalogs::tests::the_catalog_routes_follow_the_access_matrix`, `the_list_shows_reachable_catalogs_with_counts_and_generation`, `an_every_catalog_admin_creates_a_catalog_that_is_served_at_once`, `an_admin_changes_a_description_and_the_list_shows_it`, `delete_refuses_unpublished_work_unless_forced` and `export_and_import_under_api_v1_share_the_archive_routes`.

- [ ] **Step 6: The pages**

Element ids and `data-test` hooks the journeys use:
- **Catalogs:**
  - `[data-catalog=<name>]` (the name cell of each row; clicking it opens the row);
  - `#catalog-create` and `#catalog-create-form` (`input[name=name]`, `textarea[name=description]`, `button[type=submit]`);
  - `#catalog-edit-form` (`#catalog-description`, `#catalog-save`);
  - `#catalog-force` and `#catalog-delete`;
  - Task 1's `#confirm-typed` and `#confirm-ok`.
- **Import & export:**
  - `#transfer-catalog` and `#transfer-generation`;
  - `#export-btn` and `#export-link` (`href` is a `blob:` URL, `download`, `data-size`);
  - `#upload-file` and `#upload-drop`;
  - `#import-replace`, `#import-machinery`, `#import-generation`, `#import-btn` and `#import-progress`;
  - `#import-result`, holding `#import-report` (`#import-added`, `#import-updated`, `#import-deleted`, `#import-unchanged`, `#import-lint`) or `#import-error` (`#import-problems`).

`src/http/ui/js/components/upload.js`:

```js
// File choice by picker or drag and drop, and an upload with progress.
// fetch() cannot report upload progress, so the upload uses XMLHttpRequest
// with the same bearer header api.js sends.
import { h } from '../core/dom.js';
import { store } from '../core/store.js';
import { api, ApiError } from '../core/api.js';

// A drop zone around a file input. onFile(file) runs on every choice.
export function fileDrop({ accept, label, onFile }) {
    const input = h('input', { type: 'file', id: 'upload-file', accept });
    const name = h('span', { id: 'upload-name', class: 'mono small muted' }, 'No file chosen');
    const zone = h('div', { id: 'upload-drop', class: 'upload-drop' }, h('p', {}, label), input, name);
    const choose = file => {
        if (!file) return;
        name.textContent = `${file.name} (${file.size} bytes)`;
        onFile(file);
    };
    input.addEventListener('change', () => choose(input.files[0]));
    zone.addEventListener('dragover', event => { event.preventDefault(); zone.classList.add('over'); });
    zone.addEventListener('dragleave', () => zone.classList.remove('over'));
    zone.addEventListener('drop', event => {
        event.preventDefault();
        zone.classList.remove('over');
        choose(event.dataTransfer.files[0]);
    });
    return zone;
}

// POSTs `file` as application/gzip to /api/v1<path>. onProgress(fraction)
// follows the upload. Resolves with the JSON answer of a 2xx; rejects with
// an ApiError otherwise. The caller shows it: nothing is toasted here.
export function upload(path, file, onProgress = () => {}) {
    return new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', `/api/v1${path}`);
        xhr.setRequestHeader('Authorization', `Bearer ${store.key ?? ''}`);
        xhr.setRequestHeader('Content-Type', 'application/gzip');
        xhr.responseType = 'json';   // a non-JSON body (413) arrives as null
        xhr.upload.addEventListener('progress', event => {
            if (event.lengthComputable) onProgress(event.loaded / event.total);
        });
        xhr.addEventListener('load', () => {
            if (xhr.status >= 200 && xhr.status < 300) {
                onProgress(1);
                resolve(xhr.response);
                return;
            }
            // A refused key: let api.js's shared 401 path sign the browser out.
            if (xhr.status === 401) api.get('/whoami').catch(() => {});
            reject(new ApiError(xhr.status, xhr.response));
        });
        xhr.addEventListener('error', () =>
            reject(new ApiError(0, { message: 'The upload failed: the server could not be reached.' })));
        xhr.send(file);
    });
}
```

`src/http/ui/js/pages/catalogs.js`:

> **Corrected (issue #220, 2026-11-10).** The pages follow the per-navigation convention ("Front-end shape"), which this code predates. In `catalogs.js`, create and delete end in a `settled()` helper: it runs `refreshIdentity()` unconditionally (the switcher is shell state and must follow a change that committed), then returns if `!main.isConnected`, and only then closes the drawer and reloads the table. Saving a description returns before `closeDrawer()` when `!main.isConnected` (the drawer would be another page's). In `transfer.js`, the export returns after `response.blob()` when `!main.isConnected`, so a detached page hands over no download and leaves the module's object URL to the newer render. After review (PR #228), `transfer.js` also runs one export or import at a time: a `busy` flag and `setBusy()` disable the catalog picker, Export and Import while one runs, a file chosen meanwhile does not re-enable Import, and each operation keeps its catalog (and file) in a local for its URL, file name and labels. A fourth journey, `an_export_or_import_in_flight_locks_the_catalog_and_admits_no_second_import`, delays the export `fetch` and the upload `XMLHttpRequest` and checks this. Read `src/http/ui/js/pages/catalogs.js` and `transfer.js`.


```js
// Catalogs: the catalogs this key reaches. Admins edit descriptions and
// delete; an admin key that reaches every catalog also creates them.
import { $, h } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { refreshIdentity } from '../core/shell.js';

export async function render(main) {
    const admin = store.can('admin');
    const creator = admin && store.whoami.all_catalogs;
    const columns = [
        { key: 'name', label: 'Name', sort: row => row.name,
          render: row => h('code', { dataset: { catalog: row.name } }, row.name) },
        { key: 'description', label: 'Description',
          render: row => row.description || h('span', { class: 'muted' }, '-') },
        { key: 'concepts', label: 'Concepts', sort: row => row.concepts, class: 'num' },
        { key: 'files', label: 'Files', sort: row => row.files, class: 'num' },
        { key: 'generation', label: 'Generation', sort: row => row.generation, class: 'num' },
        { key: 'last_change', label: 'Last change', sort: row => row.last_change ?? '',
          render: row => ago(row.last_change) },
    ];
    const table = listTable(columns, [], { onRow: admin ? openEdit : null, empty: 'This key reaches no catalog.' });

    async function load() {
        try { table.redraw((await api.get('/catalogs')).catalogs); } catch { /* toasted */ }
    }

    function openCreate() {
        const form = h('form', { id: 'catalog-create-form' },
            h('label', {}, 'Name',
                h('input', { name: 'name', required: true, pattern: '[a-z0-9-]+', autocomplete: 'off',
                             placeholder: 'lowercase letters, digits and -' })),
            h('label', {}, 'Description', h('textarea', { name: 'description', rows: '3' })),
            h('button', { type: 'submit' }, 'Create catalog'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const name = String(data.get('name') ?? '').trim();
            try {
                await api.post('/catalogs', { name, description: String(data.get('description') ?? '') });
            } catch { return; /* toasted: invalid or taken name */ }
            closeDrawer();
            toast(`Created catalog ${name}.`, 'ok');
            await refreshIdentity().catch(() => {});
            await load();
        });
        openDrawer('New catalog', 'An empty catalog; import a .tar.gz into it next.', form);
    }

    function openEdit(row) {
        const description = h('textarea', { id: 'catalog-description', name: 'description', rows: '3' }, row.description);
        const edit = h('form', { id: 'catalog-edit-form' },
            h('label', {}, 'Description', description),
            h('button', { type: 'submit', id: 'catalog-save' }, 'Save description'));
        edit.addEventListener('submit', async event => {
            event.preventDefault();
            try {
                await api.patch(`/catalogs/${enc(row.name)}`, { description: description.value });
            } catch { return; }
            toast(`Saved ${row.name}'s description.`, 'ok');
            closeDrawer();
            await load();
        });
        const force = h('input', { type: 'checkbox', id: 'catalog-force' });
        const remove = h('button', { type: 'button', id: 'catalog-delete', class: 'danger' }, 'Delete catalog');
        remove.addEventListener('click', async () => {
            const ok = await confirmDialog({
                title: `Delete ${row.name}?`,
                message: 'Its documents, history, changesets and key grants are deleted. This cannot be undone.',
                action: 'Delete',
                typed: row.name,
            });
            if (!ok) return;
            try {
                await api.del(`/catalogs/${enc(row.name)}${force.checked ? '?force=true' : ''}`);
            } catch { return; /* toasted: 409 names ?force=true */ }
            closeDrawer();
            toast(`Deleted catalog ${row.name}.`, 'ok');
            await refreshIdentity().catch(() => {});
            await load();
        });
        openDrawer(row.name, `generation ${row.generation} - ${count(row.concepts, 'concept')}, ${count(row.files, 'file')}`,
            edit,
            h('section', { class: 'danger-zone' },
                h('h3', {}, 'Delete'),
                h('label', {}, force, ' Also discard unpublished changes (force)'),
                remove));
    }

    main.append(
        h('div', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Catalogs'), h('p', { class: 'muted' }, 'The catalogs this key reaches.')),
            h('div', { class: 'actions' },
                creator ? h('button', { type: 'button', id: 'catalog-create', onclick: openCreate }, 'New catalog') : null)),
        table,
        admin && !creator
            ? h('p', { class: 'muted small' }, 'Creating a catalog needs an admin key that reaches every catalog.')
            : null);
    await load();
}
```

`src/http/ui/js/pages/transfer.js`:

```js
// Import & export: download a catalog's .tar.gz, or upload one with the
// CLI's options. An <a href> cannot carry the bearer header, so the export
// is fetched and handed to the browser as a blob: object URL.
import { h, clear } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { count } from '../core/format.js';
import { fileDrop, upload } from '../components/upload.js';

let exportUrl = null;   // the last export's object URL, released when replaced

function forget() {
    if (exportUrl) URL.revokeObjectURL(exportUrl);
    exportUrl = null;
}

function tile(label, n, id) {
    return h('div', { class: 'tile' }, h('div', { class: 'n', id }, String(n)), h('div', { class: 'l' }, label));
}

function paths(label, list) {
    return list.length
        ? h('details', {}, h('summary', {}, `${label} (${list.length})`), h('ul', { class: 'mono small' }, list.map(p => h('li', {}, p))))
        : null;
}

function findings(list) {
    return list.map(f => h('li', {}, h('code', {}, `${f.file}:${f.line}`), ` ${f.rule}: ${f.message}`));
}

function reportPanel(report) {
    const { errors, warnings } = report.lint;
    return h('div', { id: 'import-report', class: 'panel panel-ok' },
        h('h3', {}, 'Imported'),
        h('div', { class: 'tiles' },
            tile('added', report.added.length, 'import-added'),
            tile('updated', report.updated.length, 'import-updated'),
            tile('deleted', report.deleted.length, 'import-deleted'),
            tile('unchanged', report.unchanged, 'import-unchanged')),
        paths('Added', report.added), paths('Updated', report.updated), paths('Deleted', report.deleted),
        h('p', { id: 'import-lint' }, `Lint: ${count(errors.length, 'error')}, ${count(warnings.length, 'warning')}`),
        errors.length || warnings.length ? h('ul', { class: 'small' }, findings(errors), findings(warnings)) : null);
}

function errorPanel(error) {
    const message = error.status === 413
        ? 'The file is larger than the server accepts (max_import_bytes).'
        : error.message;
    const problems = error.body?.problems ?? [];
    return h('div', { id: 'import-error', class: 'panel panel-error' },
        h('h3', {}, error.status ? `Refused (HTTP ${error.status})` : 'Failed'),
        h('p', {}, message),
        problems.length ? h('ul', { id: 'import-problems', class: 'mono small' }, problems.map(p => h('li', {}, p))) : null);
}

export async function render(main, rest) {
    forget();
    let catalogs;
    try { catalogs = (await api.get('/catalogs')).catalogs; } catch { return; }
    const known = name => catalogs.some(c => c.name === name);
    let chosen = known(rest[0]) ? rest[0] : known(store.catalog) ? store.catalog : catalogs[0]?.name;
    if (!chosen) {
        main.append(h('h1', {}, 'Import & export'), h('p', { class: 'muted' }, 'This key reaches no catalog.'));
        return;
    }
    let file = null;
    const current = () => catalogs.find(c => c.name === chosen);

    const picker = h('select', { id: 'transfer-catalog' }, catalogs.map(c => h('option', { value: c.name }, c.name)));
    picker.value = chosen;
    const generationLabel = h('span', { id: 'transfer-generation', class: 'mono small muted' });
    const generation = h('input', { type: 'number', id: 'import-generation', min: '0', step: '1' });
    const showGeneration = () => {
        generationLabel.textContent = `generation ${current().generation}`;
        generation.value = String(current().generation);
    };

    // Export
    const exportOut = h('span', { id: 'export-out' });
    const exportButton = h('button', { type: 'button', id: 'export-btn' }, 'Export .tar.gz');
    exportButton.addEventListener('click', async () => {
        exportButton.disabled = true;
        try {
            const response = await api.get(`/catalogs/${enc(chosen)}/export`, { raw: true });
            const blob = await response.blob();
            forget();
            exportUrl = URL.createObjectURL(blob);
            const link = h('a', { id: 'export-link', href: exportUrl, download: `${chosen}.tar.gz`,
                                  dataset: { size: String(blob.size) } }, `Save ${chosen}.tar.gz again`);
            exportOut.replaceChildren(link, h('span', { class: 'muted small' }, ` ${blob.size} bytes`));
            link.click();
        } catch { /* toasted */ } finally {
            exportButton.disabled = false;
        }
    });

    // Import (admin only; the server checks anyway)
    const result = h('div', { id: 'import-result' });
    const progress = h('progress', { id: 'import-progress', max: '100', value: '0', hidden: true });
    const replace = h('input', { type: 'checkbox', id: 'import-replace' });
    const machinery = h('input', { type: 'checkbox', id: 'import-machinery' });
    machinery.addEventListener('change', () => { if (machinery.checked) replace.checked = true; });
    const importButton = h('button', { type: 'button', id: 'import-btn', disabled: true }, 'Import');
    const drop = fileDrop({
        accept: '.tar.gz,.tgz,application/gzip',
        label: 'Drop a .tar.gz here, or choose one.',
        onFile: chosenFile => { file = chosenFile; importButton.disabled = false; clear(result); },
    });
    importButton.addEventListener('click', async () => {
        if (!file) return;
        const query = new URLSearchParams();
        if (replace.checked) query.set('replace', 'true');
        if (machinery.checked) query.set('only_machinery', 'true');
        if (generation.value.trim() !== '') query.set('expect_generation', generation.value.trim());
        importButton.disabled = true;
        progress.hidden = false;
        progress.value = 0;
        clear(result);
        try {
            const report = await upload(`/catalogs/${enc(chosen)}/import?${query}`, file,
                fraction => { progress.value = Math.round(fraction * 100); });
            result.append(reportPanel(report));
            toast(`Imported into ${chosen}.`, 'ok');
            try { catalogs = (await api.get('/catalogs')).catalogs; showGeneration(); } catch { /* toasted */ }
        } catch (error) {
            result.append(errorPanel(error));
            toast(error.status === 0 ? error.message : 'The import was refused.', 'error');
        } finally {
            importButton.disabled = false;
            progress.hidden = true;
        }
    });

    picker.addEventListener('change', () => {
        chosen = picker.value;
        showGeneration();
        clear(result);
        exportOut.replaceChildren();
        forget();
    });
    showGeneration();

    main.append(
        h('div', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Import & export'),
                h('p', { class: 'muted' }, 'Move a catalog in and out as a .tar.gz, as okf-mcp import and export do.'))),
        h('label', { class: 'catalog-pick' }, 'Catalog ', picker, ' ', generationLabel),
        h('section', { class: 'panel', id: 'export-panel' },
            h('h2', {}, 'Export'),
            h('p', { class: 'muted small' }, 'The published documents, as the catalog stands now.'),
            exportButton, ' ', exportOut),
        store.can('admin')
            ? h('section', { class: 'panel', id: 'import-panel' },
                h('h2', {}, 'Import'),
                drop,
                h('div', { class: 'options' },
                    h('label', {}, replace, ' Replace the catalog\'s contents'),
                    h('label', {}, machinery, ' Only machinery (AGENTS.md, tools/, ...)'),
                    h('label', {}, 'Expected generation ', generation)),
                h('p', { class: 'muted small' }, 'The import is refused if the catalog has moved past the expected generation; clear the field to skip the check.'),
                importButton, progress, result)
            : null);
}
```

Append to `src/http/ui/styles.css`:

```css
/* catalogs and import/export (M9 task 5) */
.panel { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius);
    padding: 1rem 1.25rem; margin-bottom: 1rem; }
.panel h2, .panel h3 { margin-top: 0; }
.panel-ok { border-left: 4px solid var(--ok); }
.panel-error { border-left: 4px solid var(--error); }
.panel button { width: auto; }
.catalog-pick { display: flex; align-items: center; gap: .5rem; margin-bottom: 1rem; }
.catalog-pick select { width: auto; margin: 0; font-family: var(--font-mono); }
.upload-drop { border: 2px dashed var(--border-strong); border-radius: var(--radius); padding: 1rem;
    text-align: center; margin-bottom: .75rem; }
.upload-drop.over { border-color: var(--accent); background: var(--accent-soft); }
.options { display: flex; flex-wrap: wrap; gap: .5rem 1.5rem; align-items: center; margin-bottom: .5rem; }
.options input[type=number] { width: 8rem; margin: 0; }
.list-table td.num { text-align: right; font-variant-numeric: tabular-nums; }
.danger-zone { margin-top: 1.5rem; padding-top: 1rem; border-top: 1px solid var(--border); }
#import-progress { margin: .75rem 0; }
```

- [ ] **Step 7: Browser journeys (failing first)**

`tests/ui_e2e/main.rs`: add `mod catalogs;` (keep the other tasks' lines).

> **Corrected (issue #220, 2026-11-10).** The merged `main.rs` keeps its `mod` lines in alphabetical order (Task 3 put `mod health;` before `mod login;`), so `mod catalogs;` goes first, before `mod harness;`. This task is the first caller of `api::require_admin` and `Ui::wait_hidden` and removes their `#[allow(dead_code)]` (Task 1's note); if Task 2 lands first with the same removal, keep the deletion.

`tests/ui_e2e/catalogs.rs`:

```rust
//! Catalog administration and import/export in the console (M9 task 5).

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use okf_mcp::archive::{write_tarball, ExportFile};
use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::json;

use crate::harness::{drive, Ui};
use crate::support::server::{files, TestServer};

const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha.\n";

/// Puts a file into the import page's `#upload-file`, as a user choosing it
/// would: rustwright has no set_input_files, so the page builds the File from
/// base64 and assigns it through a DataTransfer, then fires `change`.
const CHOOSE_FILE: &str = "(arg) => {
    const bytes = Uint8Array.from(atob(arg.data), char => char.charCodeAt(0));
    const transfer = new DataTransfer();
    transfer.items.add(new File([bytes], arg.name, { type: 'application/gzip' }));
    const input = document.querySelector('#upload-file');
    input.files = transfer.files;
    input.dispatchEvent(new Event('change', { bubbles: true }));
    return input.files.length;
}";

async fn server() -> TestServer {
    TestServer::start(
        vec![("main", files(&[("index.md", "# Main\n"), ("systems/alpha.md", ALPHA)]))],
        Config::default(),
    )
    .await
}

/// A `.tar.gz` of `(path, text)` pairs.
fn tarball(entries: &[(&str, &str)]) -> Vec<u8> {
    let modified = chrono::Utc::now();
    let entries: Vec<ExportFile<'_>> = entries
        .iter()
        .map(|&(path, text)| ExportFile { path, content: text.as_bytes(), executable: false, modified })
        .collect();
    write_tarball(Vec::new(), &entries).expect("tarball")
}

/// The catalog switcher's options.
fn switcher(ui: &Ui) -> Vec<String> {
    let values = ui.eval(
        "() => [...document.querySelectorAll('#catalog-select option')].map(option => option.value)",
        json!(null),
    );
    serde_json::from_value(values).expect("a list of names")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_creates_and_deletes_a_catalog_and_the_switcher_follows() {
    let server = server().await;
    // No grants: every catalog, the only kind of key that may create one.
    let key = server.key("console-root", Access::Admin, &[], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/catalogs");
        ui.wait_visible("[data-catalog=main]");
        ui.click("#catalog-create");
        ui.wait_visible("#catalog-create-form");
        ui.fill("#catalog-create-form input[name=name]", "scratch");
        ui.fill("#catalog-create-form textarea[name=description]", "Scratch space");
        ui.click("#catalog-create-form button[type=submit]");
        // The page rebuilds the switcher before it redraws the table.
        ui.wait_visible("[data-catalog=scratch]");
        assert_eq!(switcher(ui), ["main", "scratch"]);

        ui.click("[data-catalog=scratch]");
        ui.wait_visible("#catalog-delete");
        ui.click("#catalog-delete");
        ui.wait_visible("#confirm-typed");
        ui.fill("#confirm-typed", "scratch");
        ui.click("#confirm-ok");
        ui.wait_hidden("[data-catalog=scratch]");
        assert_eq!(switcher(ui), ["main"]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_hands_the_browser_the_catalog_tarball() {
    let server = server().await;
    let key = server.key("console-exporter", Access::Read, &["main"], Some("main")).await;
    let browser_key = key.clone();
    drive(server.base.clone(), move |ui| {
        ui.login(&browser_key);
        ui.open("/transfer");
        ui.wait_visible("#export-btn");
        ui.click("#export-btn");
        ui.wait_visible("#export-link");
        let link = ui.eval(
            "() => { const a = document.querySelector('#export-link'); \
             return { href: a.href, download: a.download, size: Number(a.dataset.size) }; }",
            json!(null),
        );
        assert!(link["href"].as_str().expect("an href").starts_with("blob:"), "{link}");
        assert_eq!(link["download"], "main.tar.gz");
        assert!(link["size"].as_u64().expect("a size") > 0);
        // A read key sees no import form.
        assert!(!ui.page.is_visible("#import-btn").expect("visible"));
    })
    .await;
    // rustwright cannot save downloads (Deviation 5): fetch what the button
    // fetched, with the same bearer key, and check it is gzip.
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/catalogs/main/export", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .expect("export");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/gzip");
    let bytes = response.bytes().await.expect("body");
    assert_eq!(bytes[..2], [0x1f, 0x8b]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_reports_its_counts_and_a_stale_generation_is_refused() {
    let server = server().await;
    let key = server.key("console-importer", Access::Admin, &["main"], Some("main")).await;
    let generation = server.app.catalog("main").expect("main").current().generation();
    let archive = tarball(&[
        ("index.md", "# Main\n\nImported.\n"),
        ("systems/alpha.md", ALPHA),
        ("systems/gamma.md", "---\ntype: system\ntitle: Gamma\n---\n# Gamma\n"),
    ]);
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/transfer");
        ui.wait_text("#transfer-generation", &format!("generation {generation}"));
        let prefilled = ui.eval("() => document.querySelector('#import-generation').value", json!(null));
        assert_eq!(prefilled, json!(generation.to_string()));
        ui.wait_visible("#upload-file");
        let chosen = ui.eval(CHOOSE_FILE, json!({ "name": "main.tar.gz", "data": STANDARD.encode(&archive) }));
        assert_eq!(chosen, json!(1));
        ui.wait_text("#upload-name", "main.tar.gz");
        ui.click("#import-replace");
        ui.click("#import-btn");
        ui.wait_visible("#import-report");
        assert_eq!(ui.text("#import-added"), "1");
        assert_eq!(ui.text("#import-updated"), "1");
        assert_eq!(ui.text("#import-deleted"), "0");
        assert_eq!(ui.text("#import-unchanged"), "1");
        ui.wait_text("#import-lint", "Lint:");
        // The page re-reads the catalog: the field now holds the new generation.
        ui.wait_text("#transfer-generation", &format!("generation {}", generation + 1));

        // The same file against an old generation: refused, and the panel says why.
        ui.fill("#import-generation", &generation.to_string());
        ui.click("#import-btn");
        ui.wait_text("#import-error", "export it again");
        assert!(!ui.page.is_visible("#import-report").expect("visible"));
    })
    .await;
    assert_eq!(server.app.catalog("main").expect("main").current().generation(), generation + 1);
}
```

Run: `cargo test --test ui_e2e catalogs -- --test-threads=2` before Step 6's files exist.
Expected: FAIL. With no `catalogs.js`, the router shows "Not available yet" and `wait_visible("[data-catalog=main]")` times out. With no `transfer.js`, `#export-btn` times out. Without Chromium, each journey prints the skip line (the export journey's `reqwest` half still runs and passes).

- [ ] **Step 8: Run the journeys**

Run: `just test-ui`
Expected: PASS, with no skip lines: Task 1's 4 login journeys, the 3 journeys above, and those of any other task already merged.

- [ ] **Step 9: Docs and changelog**

- **`docs/operations.md`, "Web console":** add the subsection "Catalogs and import/export". Rows for the API reference table:

  > **Corrected (issue #220, 2026-11-10).** As corrected for Task 3 (issue #218), the rows below use the merged three-column shape (`Method and path | Key | Does`) in their own `### Catalogs and import/export` subsection, placed before "Testing the console", with the same content. The 409 row of the troubleshooting table also names a stale `expect_generation`, and the section 9 curl example goes after the `--expect-generation` paragraph.


  | Method | Path | Access | Notes |
  |---|---|---|---|
  | `GET` | `/api/v1/catalogs` | any key | The catalogs the key reaches: `name`, `description`, `files`, `concepts`, `generation` (of the loaded snapshot, what `expect_generation` compares against) and `last_change` |
  | `POST` | `/api/v1/catalogs` | `admin` key that reaches every catalog | `{name, description}`. 201 with the new catalog; 409 for an invalid or taken name; 403 for a key granted named catalogs, since it would not reach the new one |
  | `PATCH` | `/api/v1/catalogs/{catalog}` | `admin` on the catalog | `{description}`. Answers `{name, previous, description}` |
  | `DELETE` | `/api/v1/catalogs/{catalog}[?force=true]` | `admin` on the catalog | 409 while an open changeset holds unpublished changes, unless `force=true` |
  | `GET` | `/api/v1/catalogs/{catalog}/export` | `read` on the catalog | Same handler and 120 s timeout as `/catalogs/{name}/export` |
  | `POST` | `/api/v1/catalogs/{catalog}/import[?replace=true&only_machinery=true&expect_generation=N]` | `admin` on the catalog | Same handler, body cap and upload deadline as `/catalogs/{name}/import` |

  Then a short paragraph:
  - catalog changes take effect at once (the handler polls);
  - catalog rows record no actor, but the server's log line names the key id and actor;
  - `only_machinery` is `--only-machinery`, and on a non-empty catalog it needs `replace=true`, as on the CLI;
  - a stale `expect_generation` is 409 with "export it again";
  - the console's Import & export page pre-fills the expected generation from `GET /api/v1/catalogs` and shows upload progress.

  Change the existing route table row (`docs/operations.md:28`) to `POST /catalogs/{name}/import[?replace=true&only_machinery=true&expect_generation=N]`. In section 9's "post it to the HTTP route" sentence (around :474), mention `?only_machinery=true` and `?expect_generation=N`. Add one `curl` example of a guarded machinery-only import:

  ```sh
  curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" --data-binary @notes.tar.gz \
      "https://okf-mcp.example.com/api/v1/catalogs/notes/import?replace=true&only_machinery=true&expect_generation=12"
  ```

- **`README.md`:**
  - Change the import row (`README.md:244`) to show the three query parameters.
  - In the paragraph at :263-267, add after the 409 sentence: "or when `?expect_generation=N` no longer matches (the message says to export again). `?only_machinery=true` replaces only repository machinery, as `--only-machinery` does."
  - In the "Web console" section, add one line: catalogs can be created, described, deleted, imported and exported from the console's Administer group. No more piping archives through a shell on the server.
- **`changelog/<ts>-m9-task-05-catalogs-transfer.md`:**
  - **Added:** the catalog endpoints, the `/api/v1` archive mounts, the `only_machinery` and `expect_generation` import options, and the Catalogs and Import & export pages.
  - **Changed:** the archive routes' route builders are shared, and the "not empty" refusal is reworded by text replacement.
  - **Notes:** catalog creation needs an all-catalogs admin key; matrix rows.

- [ ] **Step 10: Gates and commit**

Run every gate in "Conventions", including `just test-ui` with Chromium. Then:

```bash
git add src/http/archives.rs src/http/api/catalogs.rs src/http/ui/js/pages/catalogs.js \
    src/http/ui/js/pages/transfer.js src/http/ui/js/components/upload.js src/http/ui/styles.css \
    tests/ui_e2e/catalogs.rs tests/ui_e2e/main.rs README.md docs/operations.md changelog/
git commit -m "feat(console): catalog administration and import/export pages and API (M9 task 5)"
```

### Task 6: Findings and runs

Depends on Task 1.

**Facts this task relies on** (checked at `62e88cd`):
- `src/mcp/maintenance/reports.rs`: the params structs (14-69) derive `Serialize, Deserialize, JsonSchema` with `#[serde(deny_unknown_fields)]` and a required `catalog: String`: `MaintenanceListRunsParams { catalog, after, limit: Option<u32> }`, `MaintenanceGetRunParams { catalog, run_id }`, `MaintenanceListFindingsParams { catalog, after, limit }`, `MaintenanceGetFindingParams { catalog, finding_id }`, `MaintenanceDisposeParams { catalog, finding_id, disposition: Disposition, rationale: String, idempotency_key: String }`. Functions: `maintenance_list_runs` (71), `maintenance_get_run` (87), `maintenance_list_findings` (101), `maintenance_get_finding` (120), `maintenance_dispose` (131). Each does `ctx.one(Some(&params.catalog))?` then `principal.require(Read)`, except `maintenance_dispose`, which requires **Write** (136). The reporting flag plays no part in dispose: a reporter key at read access gets 403 (`tests/maintenance.rs:61`).
- `MaintenanceResult<T> { catalog, #[serde(flatten)] result }`, fields `pub`, `new` private to `mcp::maintenance` (`src/mcp/maintenance/mod.rs:24-37`). `finding_paths` and `pin_path` `/`-prefix evidence and work pins (46-62). `use reports::*;` (19) glob-imports the tool functions into `mod.rs`.
- Store (`src/store/maintenance/`):
  - `runs::Page<T> { items, next_after }` (`runs.rs:21-26`).
  - `runs::page_ids` (85-127): it refuses a limit outside 1..=200 with `Refused` (409), and an unknown `after` cursor with `NotFound`.
  - `list_runs` (137), `get_run` (164).
  - `RunReport { run: RunSummary, work: Vec<WorkReport { id, pin, reasons, outcome, assessment, summary, evidence, finding_ids }> }` (30-56).
  - `RunSummary { id, state, actor, created_at, deadline, ended_at, reason, primary_count, completed_count, failed_count, inconclusive_count, superseded_count }` (215-228).
  - `claim(pool, catalog_id, &Actor, key, limit, review_interval, now) -> ClaimResult { run_id, leases }` (317).
  - `complete_if_terminal` marks a run `completed` when its last work is submitted (230; called from `reports::submit`, `reports.rs:445`).
- Store findings (`src/store/maintenance/reports.rs`):
  - `FindingRecord { id, run_id, work_id, input: FindingInput, actor, created_at, disposition, dispositions: Vec<DispositionRecord { id, disposition, rationale, actor, at }>, outdated }` (19-54). Its `disposition` is the last appended one, or `Open` with none (113-115).
  - `list_findings` (154) has **no disposition filter**.
  - `dispose` (187-242) appends one row. A retry with the same credential, operation and key and the same payload returns the stored original response with no write; the same key with a different payload is `Refused` (409) ("idempotency key reused with a different request", `mod.rs:74-95`). Another key with the same idempotency key is a new request, because the key is credential-scoped. The rationale may be empty, is at most 4096 bytes and is secret-scanned (`validate.rs:224-229`).
  - `submit(pool, catalog_id, &Actor, work_id, token, key, &Submission, now) -> Receipt { run_id, work_id, outcome, finding_ids, outdated }` (294; `types.rs:163-169`).
  - `enum_text` is private (256).
- `maintenance_dispositions` has `disposition text` with a CHECK on the four values and the index `(catalog_id, finding_id, id)` (`migrations/0006_maintenance.sql:104-115`), so "the last disposition of a finding" is one index probe.
- `Disposition` is `Copy`, `snake_case` `open|accepted_for_follow_up|dismissed|needs_input` (`src/maintenance/types.rs:82-89`); `RunState` `running|completed|failed|cancelled` (55-62); `FindingInput { category, assertion, follow_up, uncertainty, relationship, evidence, external }` (108-116); `ExternalObservation { resource, excerpt, attribution }` (100-104). A finding needs at least one catalog evidence entry (`validate.rs:133-135`), so `input.evidence[0].pin.path` is the finding's concept.
- Seeding:
  - The store tests claim and submit directly (`report_tests.rs:10-63`, with `setup`/`actor`/`t0` from `tests.rs:13-30`).
  - The HTTP tests use a reporter key with `maintenance_claim` and `maintenance_submit` (`tests/maintenance.rs:24-45`). Their evidence excerpt is a substring of the pinned revision.
  - `TestServer::reporter_key(name, catalogs)` (`tests/support/server.rs:206`), `client(key, None)` (234), `call` (323), `structured` (370).
- `maintenance_runs.key_id` references `api_keys` but is set to `NULL` when no key has the credential id (`runs.rs:362`), so a store-level seed may use a synthetic credential id.

**Disposition filter: a store filter.** The store cannot filter today, and filtering in the handler would break paging: a page of 20 could hold 0 matches while `next_after` still points on. Instead, `page_ids` gains an optional current-disposition condition, so `limit` and `next_after` count matching rows only. The cursor is still any finding id in the catalog. The MCP tool keeps its schema (no new argument; the plan changes no MCP API): `maintenance_list_findings` delegates to a new `maintenance_list_findings_by` with `None`.

**Badge count: a counts endpoint.** Listing open findings to count them costs three queries per finding. `GET .../maintenance/counts` is one `GROUP BY` over the same "current disposition" expression, which is shared through a macro so the filter and the count cannot drift. `refreshFindingsBadge()` in `pages/findings.js` calls it. `main.js` imports and calls it after sign-in and on a catalog switch, because Health (not Findings) is the landing page. This is the one edit outside the task's own files (see Notes).

**Spec deviations:**
- The disposition body is `{disposition, rationale, idempotency_key}` (the tool's names), not `{disposition, note}`.
- `idempotency_key` is required, as in the tool.

**Files:**
- Modify: `src/store/maintenance/mod.rs` (the `current_disposition!` macro), `src/store/maintenance/runs.rs` (`Listing`, `page_ids`), `src/store/maintenance/reports.rs` (`list_findings_with`, `FindingCounts`, `count_findings`, `enum_text` visibility), `src/store/maintenance/report_tests.rs` (one test)
- Modify: `src/mcp/maintenance/reports.rs` (`maintenance_list_findings_by`, `maintenance_finding_counts`)
- Modify: `src/http/api/maintenance.rs` (replaces Task 1's stub)
- Create: `src/http/ui/js/pages/findings.js`, `src/http/ui/js/pages/runs.js`
- Modify: `src/http/ui/js/main.js` (badge refresh, three lines), `src/http/ui/styles.css` (appended rules)
- Create: `tests/ui_e2e/maintenance.rs`; modify `tests/ui_e2e/main.rs` (`mod maintenance;`)
- Modify: `docs/operations.md` ("Web console": subsection "Findings and runs"), `README.md` (one sentence)
- Create: `changelog/<ts>-m9-task-06-findings-runs.md`

- [ ] **Step 1: Write the failing store test**

Append to `src/store/maintenance/report_tests.rs` (it already imports `reports`, `runs`, `test_db`, `actor`, `setup`, `t0` and `crate::maintenance::types::*`):

```rust
#[tokio::test(flavor = "multi_thread")]
async fn maintenance_findings_filter_and_count_by_current_disposition() {
    let db = test_db().await;
    let catalog = setup(&db.pool, &["a.md", "b.md"]).await;
    let (_, first) = fixture(&db.pool, catalog, "first").await;
    let (_, second) = fixture(&db.pool, catalog, "second").await;
    let ids = |page: &runs::Page<reports::FindingRecord>| {
        page.items.iter().map(|finding| finding.id.clone()).collect::<Vec<_>>()
    };
    let human = actor("human");
    reports::dispose(&db.pool, catalog, &human, &first, Disposition::Dismissed, "Not useful.", "dismiss", t0())
        .await
        .unwrap();
    let open = reports::list_findings_with(&db.pool, catalog, None, 20, Some(Disposition::Open))
        .await
        .unwrap();
    assert_eq!(ids(&open), [second.clone()]);
    assert_eq!(open.next_after, None);
    let dismissed =
        reports::list_findings_with(&db.pool, catalog, None, 20, Some(Disposition::Dismissed))
            .await
            .unwrap();
    assert_eq!(ids(&dismissed), [first.clone()]);
    assert!(reports::list_findings_with(&db.pool, catalog, None, 20, Some(Disposition::NeedsInput))
        .await
        .unwrap()
        .items
        .is_empty());
    assert_eq!(
        reports::count_findings(&db.pool, catalog).await.unwrap(),
        reports::FindingCounts { open: 1, accepted_for_follow_up: 0, dismissed: 1, needs_input: 0 }
    );

    // Only the last decision counts: reopening moves the finding back.
    reports::dispose(&db.pool, catalog, &human, &first, Disposition::Open, "Reopened.", "reopen", t0())
        .await
        .unwrap();
    let all = reports::list_findings(&db.pool, catalog, None, 20).await.unwrap();
    let open = reports::list_findings_with(&db.pool, catalog, None, 20, Some(Disposition::Open))
        .await
        .unwrap();
    assert_eq!(open.items, all.items);
    // A filtered page's cursor continues within the filter.
    let page = reports::list_findings_with(&db.pool, catalog, None, 1, Some(Disposition::Open))
        .await
        .unwrap();
    assert_eq!(page.items, all.items[..1]);
    let rest = reports::list_findings_with(
        &db.pool,
        catalog,
        page.next_after.as_deref(),
        1,
        Some(Disposition::Open),
    )
    .await
    .unwrap();
    assert_eq!(rest.items, all.items[1..]);
    assert_eq!(rest.next_after, None);
    assert_eq!(
        reports::count_findings(&db.pool, catalog).await.unwrap(),
        reports::FindingCounts { open: 2, ..Default::default() }
    );
}
```

Run: `cargo test --lib store::maintenance::report_tests -- --test-threads=6`
Expected: it does not compile: `list_findings_with`, `count_findings` and `FindingCounts` do not exist.

- [ ] **Step 2: Implement the store filter and the counts**

`src/store/maintenance/mod.rs`: insert after the `//!` line, **before** `pub mod candidates;` (a `macro_rules!` macro is visible only to code that comes after it in the file, child modules included):

```rust
/// SQL for a finding's current disposition: its last appended one, else
/// `open`. The finding row must be aliased `f`. A macro rather than a
/// `const`, so `concat!` can splice it into `&'static str` queries at
/// compile time (a `const &str` cannot be concatenated that way).
macro_rules! current_disposition {
    () => {
        "COALESCE((SELECT d.disposition FROM maintenance_dispositions d \
         WHERE d.catalog_id=f.catalog_id AND d.finding_id=f.id \
         ORDER BY d.id DESC LIMIT 1),'open')"
    };
}
```

`src/store/maintenance/runs.rs`: add `Disposition` to the `crate::maintenance::types` import, and replace `page_ids` (85-127) with:

```rust
/// Which rows a page of IDs lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Listing {
    /// Maintenance runs.
    Runs,
    /// Findings; with a disposition, only those whose current one it is.
    Findings(Option<Disposition>),
}

/// Runs, oldest first.
const RUNS_PAGE: &str = "SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND \
    ($2::timestamptz IS NULL OR (created_at,id)>($2,$3)) ORDER BY created_at,id LIMIT $4";

/// Findings, oldest first, narrowed by `$5` (a disposition, or NULL for all).
const FINDINGS_PAGE: &str = concat!(
    "SELECT f.id FROM maintenance_findings f WHERE f.catalog_id=$1 AND \
     ($2::timestamptz IS NULL OR (f.created_at,f.id)>($2,$3)) AND ($5::text IS NULL OR ",
    current_disposition!(),
    "=$5) ORDER BY f.created_at,f.id LIMIT $4"
);

pub(super) async fn page_ids(
    conn: &mut sqlx::PgConnection,
    catalog: i64,
    after: Option<&str>,
    limit: u32,
    listing: Listing,
) -> Result<Page<String>, OkfError> {
    if !(1..=200).contains(&limit) {
        return Err(refused(
            "maintenance page size must be 1..=200 (default 20)",
        ));
    }
    let findings = matches!(listing, Listing::Findings(_));
    let cursor: Option<(DateTime<Utc>, String)> = if let Some(id) = after {
        Some(
            sqlx::query_as(if findings {
                "SELECT created_at,id FROM maintenance_findings WHERE catalog_id=$1 AND id=$2"
            } else {
                "SELECT created_at,id FROM maintenance_runs WHERE catalog_id=$1 AND id=$2"
            })
            .bind(catalog)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(not_found)?,
        )
    } else {
        None
    };
    // The filter's text form. `transpose` turns Option<Result<..>> into
    // Result<Option<..>>, so `?` can raise the (impossible) encoding error.
    let filter = match listing {
        Listing::Runs => None,
        Listing::Findings(disposition) => disposition.map(super::reports::enum_text).transpose()?,
    };
    let query = sqlx::query_scalar::<_, String>(if findings { FINDINGS_PAGE } else { RUNS_PAGE })
        .bind(catalog)
        .bind(cursor.as_ref().map(|c| c.0))
        .bind(cursor.as_ref().map(|c| c.1.as_str()))
        .bind(i64::from(limit) + 1);
    // Only the findings query has a `$5`; binding it for runs would be a
    // parameter-count error from Postgres.
    let query = if findings { query.bind(filter) } else { query };
    let mut items: Vec<String> = query.fetch_all(conn).await?;
    let next_after = if items.len() > limit as usize {
        items.pop();
        items.last().cloned()
    } else {
        None
    };
    Ok(Page { items, next_after })
}
```

In `list_runs`, change the call to `page_ids(&mut tx, catalog_id, after, limit, Listing::Runs)`.

`src/store/maintenance/reports.rs`:
- Make `enum_text` `pub(super)` with a doc comment ("The `snake_case` text of a maintenance enum, as stored.").
- Replace `list_findings` (154-176) with:

```rust
/// Lists findings ascending by `(created_at,id)` in one read-only snapshot.
///
/// `limit` must be 1..=200; callers supply the default 20 when omitted upstream.
/// `after` is an existing finding ID in this explicit catalog. Freshness is
/// computed at read time, not copied from the original submission receipt.
///
/// # Errors
/// `Refused` for invalid limits, `NotFound` for absent/foreign cursors, or database errors.
pub async fn list_findings(
    pool: &PgPool,
    catalog_id: i64,
    after: Option<&str>,
    limit: u32,
) -> Result<runs::Page<FindingRecord>, OkfError> {
    list_findings_with(pool, catalog_id, after, limit, None).await
}

/// [`list_findings`], keeping only findings whose current disposition (the
/// last appended, else `Open`) is `disposition` when it is set. `limit` and
/// `next_after` count matching findings only; `after` may be any finding of
/// the catalog, so a cursor survives its finding changing disposition.
///
/// # Errors
/// As [`list_findings`].
pub async fn list_findings_with(
    pool: &PgPool,
    catalog_id: i64,
    after: Option<&str>,
    limit: u32,
    disposition: Option<Disposition>,
) -> Result<runs::Page<FindingRecord>, OkfError> {
    let mut tx = runs::snapshot(pool).await?;
    let ids =
        runs::page_ids(&mut tx, catalog_id, after, limit, runs::Listing::Findings(disposition))
            .await?;
    let mut items = Vec::with_capacity(ids.items.len());
    for id in ids.items {
        items.push(finding(&mut tx, catalog_id, &id).await?);
    }
    tx.commit().await?;
    Ok(runs::Page {
        items,
        next_after: ids.next_after,
    })
}

/// How many of a catalog's findings are in each current disposition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindingCounts {
    /// Open, including reopened.
    pub open: i64,
    /// Accepted for follow-up.
    pub accepted_for_follow_up: i64,
    /// Dismissed.
    pub dismissed: i64,
    /// Waiting for input.
    pub needs_input: i64,
}

/// Counts a catalog's findings by current disposition, in one query.
///
/// # Errors
/// Database errors; `Refused` for a stored disposition outside the four values.
pub async fn count_findings(pool: &PgPool, catalog_id: i64) -> Result<FindingCounts, OkfError> {
    // The inner select computes each finding's current disposition, the outer
    // one groups them (grouping by a correlated subquery directly is refused).
    let rows: Vec<(String, i64)> = sqlx::query_as(concat!(
        "SELECT disposition, COUNT(*) FROM (SELECT ",
        current_disposition!(),
        " AS disposition FROM maintenance_findings f WHERE f.catalog_id=$1) current \
         GROUP BY disposition"
    ))
    .bind(catalog_id)
    .fetch_all(pool)
    .await?;
    let mut counts = FindingCounts::default();
    for (text, n) in rows {
        // `&mut counts.open` borrows one field for writing; `*slot = n` writes through it.
        let slot = match runs::decode::<Disposition>(text)? {
            Disposition::Open => &mut counts.open,
            Disposition::AcceptedForFollowUp => &mut counts.accepted_for_follow_up,
            Disposition::Dismissed => &mut counts.dismissed,
            Disposition::NeedsInput => &mut counts.needs_input,
        };
        *slot = n;
    }
    Ok(counts)
}
```

Run: `cargo test --lib store::maintenance -- --test-threads=6`
Expected: PASS: the new `maintenance_findings_filter_and_count_by_current_disposition` and every existing maintenance store test (`maintenance_report_pages_are_catalog_scoped` covers the unchanged unfiltered paging).

- [ ] **Step 3: The tool-layer functions**

`src/mcp/maintenance/reports.rs`: replace `maintenance_list_findings` (101-118) with the two functions below, and add the counts function after `maintenance_get_finding`:

```rust
pub(crate) async fn maintenance_list_findings(
    ctx: &ToolContext,
    params: MaintenanceListFindingsParams,
) -> Result<MaintenanceResult<runs::Page<reports::FindingRecord>>, OkfError> {
    maintenance_list_findings_by(ctx, params, None).await
}

/// `maintenance_list_findings`, narrowed to findings whose current
/// disposition is `disposition` (the web console's filter; not an MCP
/// argument, so the tool's schema is unchanged).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog and page.
/// * `disposition` - keep only this current disposition; `None` for all.
///
/// # Returns
/// One page of findings, with `/`-prefixed evidence paths.
///
/// # Errors
/// As `maintenance_list_findings`.
pub(crate) async fn maintenance_list_findings_by(
    ctx: &ToolContext,
    params: MaintenanceListFindingsParams,
    disposition: Option<Disposition>,
) -> Result<MaintenanceResult<runs::Page<reports::FindingRecord>>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let mut result = reports::list_findings_with(
        ctx.app().pool(),
        state.id(),
        params.after.as_deref(),
        params.limit.unwrap_or(20),
        disposition,
    )
    .await?;
    for finding in &mut result.items {
        finding_paths(finding);
    }
    Ok(MaintenanceResult::new(state.name(), result))
}

/// A catalog's findings counted by current disposition (the console's badge;
/// not an MCP tool).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `catalog` - the catalog.
///
/// # Returns
/// The counts, beside the catalog.
///
/// # Errors
/// `CatalogNotSelected` for a catalog the key does not reach; database errors.
pub(crate) async fn maintenance_finding_counts(
    ctx: &ToolContext,
    catalog: &str,
) -> Result<MaintenanceResult<reports::FindingCounts>, OkfError> {
    let state = ctx.one(Some(catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let counts = reports::count_findings(ctx.app().pool(), state.id()).await?;
    Ok(MaintenanceResult::new(state.name(), counts))
}
```

(`reports` here is the file's existing alias for `crate::store::maintenance::reports`; `Disposition` and `Access` are already imported.)

- [ ] **Step 4: Write the failing API tests**

At the bottom of `src/http/api/maintenance.rs`:

```rust
#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::maintenance::types::{
        Actor, Assessment, Category, Evidence, FindingInput, Relationship, Submission, WorkOutcome,
    };
    use crate::mcp::testing::TestApp;
    use crate::store::maintenance::{reports as finding_store, runs as run_store};

    const BASE: &str = "/api/v1/catalogs/main/maintenance";

    /// One completed run on `main` with one open finding, written straight
    /// through the store (tests/maintenance.rs covers the reporter's MCP path).
    ///
    /// # Returns
    /// The run id and the finding id.
    async fn seed(test: &TestApp) -> (String, String) {
        let state = test.app.catalog("main").expect("main is loaded");
        let pool = test.app.pool();
        let now = test.app.clock().now();
        let reporter = Actor { key_id: "reporter".into(), actor: "agent/reporter".into() };
        let claimed =
            run_store::claim(pool, state.id(), &reporter, "seed-claim", 1, Duration::days(30), now)
                .await
                .expect("claim one concept");
        let lease = claimed.leases.first().expect("one lease");
        // Both MAIN concepts contain this line, whichever one is leased.
        let evidence = Evidence { pin: lease.pin.clone(), excerpt: "type: system".into() };
        let submission = Submission {
            outcome: WorkOutcome::Completed,
            assessment: Assessment::SupportedCurrent,
            summary: "Synthetic investigation.".into(),
            evidence: vec![evidence.clone()],
            findings: vec![FindingInput {
                category: Category::UnverifiableClaim,
                assertion: "The owner of this system is not recorded.".into(),
                follow_up: "Ask the owner.".into(),
                uncertainty: "Synthetic only.".into(),
                relationship: Relationship::DirectStatement,
                evidence: vec![evidence],
                external: vec![],
            }],
            reported_tokens: None,
        };
        let receipt = finding_store::submit(
            pool,
            state.id(),
            &reporter,
            &lease.work_id,
            &lease.token,
            "seed-submit",
            &submission,
            now,
        )
        .await
        .expect("submit the work");
        let finding = receipt.finding_ids.first().expect("one finding").clone();
        (claimed.run_id.clone(), finding)
    }

    fn dispose_body(disposition: &str, rationale: &str, key: &str) -> serde_json::Value {
        json!({"disposition": disposition, "rationale": rationale, "idempotency_key": key})
    }

    #[tokio::test]
    async fn maintenance_routes_follow_the_tools_access() {
        let test = app().await;
        let (run, finding) = seed(&test).await;
        let read = |uri: String| Row {
            method: "GET",
            uri,
            body: None,
            read: 200,
            write: 200,
            admin: 200,
            stranger: 404,
        };
        assert_matrix(
            &test,
            &[
                read(format!("{BASE}/runs")),
                read(format!("{BASE}/runs/{run}")),
                read(format!("{BASE}/findings")),
                read(format!("{BASE}/findings?disposition=open")),
                read(format!("{BASE}/findings/{finding}")),
                read(format!("{BASE}/counts")),
                // Write, as maintenance_dispose. Admin is a different
                // credential, so its same idempotency key is a new request.
                Row {
                    method: "POST",
                    uri: format!("{BASE}/findings/{finding}/disposition"),
                    body: Some(dispose_body("needs_input", "Ask the owner.", "matrix")),
                    read: 403,
                    write: 200,
                    admin: 200,
                    stranger: 404,
                },
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn the_filter_and_the_counts_follow_the_current_disposition() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let get = |uri: String| {
            let key = keys.read.clone();
            let test = &test;
            async move { send(test, "GET", &uri, Some(&key), None).await }
        };
        let (status, open) = get(format!("{BASE}/findings?disposition=open")).await;
        assert_eq!(status.as_u16(), 200, "{open}");
        assert_eq!(open["catalog"], "main");
        assert_eq!(open["items"][0]["id"], finding.as_str());
        assert!(open["items"][0]["input"]["evidence"][0]["pin"]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/systems/")));
        let (_, counts) = get(format!("{BASE}/counts")).await;
        assert_eq!(
            counts,
            json!({"catalog": "main", "open": 1, "accepted_for_follow_up": 0,
                   "dismissed": 0, "needs_input": 0})
        );

        let (status, disposed) = send(
            &test,
            "POST",
            &format!("{BASE}/findings/{finding}/disposition"),
            Some(&keys.write),
            Some(dispose_body("dismissed", "Covered elsewhere.", "k1")),
        )
        .await;
        assert_eq!(status.as_u16(), 200, "{disposed}");
        assert_eq!(disposed["disposition"], "dismissed");
        assert_eq!(disposed["dispositions"][0]["actor"], "writer");

        let (_, open) = get(format!("{BASE}/findings?disposition=open")).await;
        assert_eq!(open["items"], json!([]));
        assert_eq!(open["next_after"], serde_json::Value::Null);
        let (_, dismissed) = get(format!("{BASE}/findings?disposition=dismissed")).await;
        assert_eq!(dismissed["items"][0]["id"], finding.as_str());
        let (_, all) = get(format!("{BASE}/findings")).await;
        assert_eq!(all["items"].as_array().expect("items").len(), 1);
        let (_, counts) = get(format!("{BASE}/counts")).await;
        assert_eq!(counts["open"], 0);
        assert_eq!(counts["dismissed"], 1);
    }

    #[tokio::test]
    async fn a_retried_disposition_replays_and_a_changed_one_is_refused() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let uri = format!("{BASE}/findings/{finding}/disposition");
        let body = dispose_body("accepted_for_follow_up", "Worth a look.", "retry-1");
        let (first_status, first) =
            send(&test, "POST", &uri, Some(&keys.write), Some(body.clone())).await;
        let (again_status, again) = send(&test, "POST", &uri, Some(&keys.write), Some(body)).await;
        assert_eq!((first_status.as_u16(), again_status.as_u16()), (200, 200));
        assert_eq!(first, again, "a retry returns the original response");
        assert_eq!(again["dispositions"].as_array().expect("history").len(), 1);
        let (status, refused) = send(
            &test,
            "POST",
            &uri,
            Some(&keys.write),
            Some(dispose_body("accepted_for_follow_up", "Changed my mind.", "retry-1")),
        )
        .await;
        assert_eq!(status.as_u16(), 409);
        assert_eq!(refused["error"], "Refused");
    }

    #[tokio::test]
    async fn bad_maintenance_requests_are_refused() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let get = |uri: String| {
            let key = keys.read.clone();
            let test = &test;
            async move { send(test, "GET", &uri, Some(&key), None).await.0.as_u16() }
        };
        assert_eq!(get(format!("{BASE}/findings?disposition=bogus")).await, 400);
        assert_eq!(get(format!("{BASE}/findings/missing")).await, 404);
        assert_eq!(get(format!("{BASE}/runs/missing")).await, 404);
        assert_eq!(get(format!("{BASE}/findings?after=missing")).await, 404);
        // Out-of-range page sizes are the tool's `Refused`.
        assert_eq!(get(format!("{BASE}/runs?limit=0")).await, 409);
        // The spec's `note` is not a field: the body is the tool's.
        let (status, _) = send(
            &test,
            "POST",
            &format!("{BASE}/findings/{finding}/disposition"),
            Some(&keys.write),
            Some(json!({"disposition": "dismissed", "note": "No.", "idempotency_key": "n"})),
        )
        .await;
        assert_eq!(status.as_u16(), 422);
    }

    #[tokio::test]
    async fn a_run_lists_its_work_and_findings() {
        let test = app().await;
        let (run, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let (_, runs) = send(&test, "GET", &format!("{BASE}/runs"), Some(&keys.read), None).await;
        assert_eq!(runs["items"][0]["id"], run.as_str());
        assert_eq!(runs["items"][0]["state"], "completed");
        let (status, report) =
            send(&test, "GET", &format!("{BASE}/runs/{run}"), Some(&keys.read), None).await;
        assert_eq!(status.as_u16(), 200, "{report}");
        assert_eq!(report["run"]["completed_count"], 1);
        assert_eq!(report["work"][0]["finding_ids"], json!([finding]));
        assert!(report["work"][0]["pin"]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with('/')));
    }
}
```

Run: `cargo test --lib http::api::maintenance -- --test-threads=6`
Expected: FAIL. Every test panics on a 404 from Task 1's stub router, for example `GET /api/v1/catalogs/main/maintenance/runs as read: null` in the matrix.

- [ ] **Step 5: Implement the maintenance area**

`src/http/api/maintenance.rs` (above the tests, replacing the stub):

```rust
//! `/api/v1/catalogs/{catalog}/maintenance/*` (web console design, "API":
//! Maintenance): runs, findings, finding counts and dispositions. Each
//! handler calls the matching `maintenance_*` tool function, so access
//! (read to list, write to dispose), paging and idempotency are the tool's.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::auth::Principal;
use crate::http::api;
use crate::http::AppError;
use crate::maintenance::types::Disposition;
use crate::mcp::maintenance::reports::{
    self as tools, MaintenanceDisposeParams, MaintenanceGetFindingParams,
    MaintenanceGetRunParams, MaintenanceListFindingsParams, MaintenanceListRunsParams,
};
use crate::mcp::maintenance::MaintenanceResult;
use crate::store::maintenance::reports::{FindingCounts, FindingRecord};
use crate::store::maintenance::runs::{Page, RunReport, RunSummary};

/// `?after=&limit=` for runs.
#[derive(Debug, Deserialize)]
struct RunsQuery {
    /// The last run id of the previous page.
    after: Option<String>,
    /// Page size, 1..=200 (default 20).
    limit: Option<u32>,
}

/// `?after=&limit=&disposition=` for findings.
#[derive(Debug, Deserialize)]
struct FindingsQuery {
    /// The last finding id of the previous page.
    after: Option<String>,
    /// Page size, 1..=200 (default 20).
    limit: Option<u32>,
    /// Keep only this current disposition (an unknown value is a 400).
    disposition: Option<Disposition>,
}

/// `POST .../disposition`'s body: the tool's arguments without the ids,
/// which come from the path. Unknown fields are refused (422), as the tool
/// refuses them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisposeBody {
    /// The decision.
    disposition: Disposition,
    /// Why, recorded with the key's actor.
    rationale: String,
    /// The caller's retry key (`crypto.randomUUID()` in the console).
    idempotency_key: String,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{catalog}/maintenance/runs", get(list_runs))
        .route("/catalogs/{catalog}/maintenance/runs/{run}", get(get_run))
        .route("/catalogs/{catalog}/maintenance/findings", get(list_findings))
        .route("/catalogs/{catalog}/maintenance/findings/{finding}", get(get_finding))
        .route("/catalogs/{catalog}/maintenance/findings/{finding}/disposition", post(dispose))
        .route("/catalogs/{catalog}/maintenance/counts", get(counts))
}

/// `GET .../maintenance/runs`: one page of runs, oldest first.
///
/// # Errors
/// As `maintenance_list_runs`: 404 unreached catalog or unknown cursor, 409 bad limit.
async fn list_runs(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<MaintenanceResult<Page<RunSummary>>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceListRunsParams { catalog, after: query.after, limit: query.limit };
    Ok(Json(tools::maintenance_list_runs(&ctx, params).await?))
}

/// `GET .../maintenance/runs/{run}`: a run and its work items.
///
/// # Errors
/// As `maintenance_get_run`: 404 unreached catalog or unknown run.
async fn get_run(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, run_id)): Path<(String, String)>,
) -> Result<Json<MaintenanceResult<RunReport>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceGetRunParams { catalog, run_id };
    Ok(Json(tools::maintenance_get_run(&ctx, params).await?))
}

/// `GET .../maintenance/findings`: one page of findings, oldest first,
/// optionally of one current disposition.
///
/// # Errors
/// As `maintenance_list_findings`; 400 for an unknown disposition.
async fn list_findings(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Query(query): Query<FindingsQuery>,
) -> Result<Json<MaintenanceResult<Page<FindingRecord>>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceListFindingsParams { catalog, after: query.after, limit: query.limit };
    Ok(Json(tools::maintenance_list_findings_by(&ctx, params, query.disposition).await?))
}

/// `GET .../maintenance/findings/{finding}`.
///
/// # Errors
/// As `maintenance_get_finding`: 404 unreached catalog or unknown finding.
async fn get_finding(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, finding_id)): Path<(String, String)>,
) -> Result<Json<MaintenanceResult<FindingRecord>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceGetFindingParams { catalog, finding_id };
    Ok(Json(tools::maintenance_get_finding(&ctx, params).await?))
}

/// `POST .../maintenance/findings/{finding}/disposition`: appends a decision.
///
/// # Errors
/// As `maintenance_dispose`: 403 below write; 404 unreached catalog or
/// unknown finding; 409 for a reused key with a different body; 400 for a
/// secret in the rationale; 422 for a malformed body.
async fn dispose(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, finding_id)): Path<(String, String)>,
    // `Json` reads the body, so it must be the last extractor.
    Json(body): Json<DisposeBody>,
) -> Result<Json<MaintenanceResult<FindingRecord>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceDisposeParams {
        catalog,
        finding_id,
        disposition: body.disposition,
        rationale: body.rationale,
        idempotency_key: body.idempotency_key,
    };
    Ok(Json(tools::maintenance_dispose(&ctx, params).await?))
}

/// `GET .../maintenance/counts`: findings per current disposition.
///
/// # Errors
/// 404 for an unreached catalog.
async fn counts(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<MaintenanceResult<FindingCounts>>, AppError> {
    let ctx = api::context(&app, principal);
    Ok(Json(tools::maintenance_finding_counts(&ctx, &catalog).await?))
}
```

- [ ] **Step 6: Run the tests to make sure they pass**

Run: `cargo test --lib http::api::maintenance -- --test-threads=6 && cargo test --test maintenance -- --test-threads=6`
Expected: PASS: `maintenance_routes_follow_the_tools_access`, `the_filter_and_the_counts_follow_the_current_disposition`, `a_retried_disposition_replays_and_a_changed_one_is_refused`, `bad_maintenance_requests_are_refused`, `a_run_lists_its_work_and_findings`. The MCP maintenance tests are unchanged and still pass (the tool now delegates).

- [ ] **Step 7: The Findings and Runs pages**

Ids and attributes the journeys use:
- **Findings:**
  - `#findings-filter` (segments, `.seg[data-value=open|needs_input|accepted_for_follow_up|dismissed|all]`), `#findings-table`, `#findings-more`, `#findings-refresh`.
  - In the drawer: `#finding-state`, `#finding-claim`, `#finding-evidence`, `#finding-history`, `#dispose-actions` (write keys only), `#dispose-rationale`, `#dispose-<disposition>`.
- **Runs:** `#runs-table`; in the drawer, `#run-state` and `table#run-work`.
- **Nav:** `#findings-badge` (Task 1's shell).

`src/http/ui/js/pages/findings.js`:

```js
// Maintenance findings: a table filtered by current disposition, a drawer
// with the claim, its evidence at the pinned revisions and the decision
// history, and for write keys the four disposition buttons. A decision is
// recorded only; it never edits the catalog. Also owns the nav badge.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { segments } from '../components/segments.js';
import { browseHash } from '../core/router.js';

export const DISPOSITIONS = [
    { value: 'open', label: 'Open', kind: 'warn' },
    { value: 'needs_input', label: 'Needs input', kind: 'info' },
    { value: 'accepted_for_follow_up', label: 'Accepted', kind: 'ok' },
    { value: 'dismissed', label: 'Dismissed', kind: 'muted' },
];
const PAGE_SIZE = 200;

export function maintenancePath(catalog, rest) {
    return `/catalogs/${enc(catalog)}/maintenance${rest}`;
}

export function dispositionPill(value) {
    const known = DISPOSITIONS.find(d => d.value === value);
    return pill(known?.label ?? value, known?.kind ?? 'muted');
}

const words = text => String(text ?? '').replaceAll('_', ' ');
const conceptOf = finding => finding.input.evidence[0]?.pin.path ?? null;

// Sets #findings-badge to the selected catalog's open findings: hidden at 0,
// or when the count cannot be read. main.js calls it after sign-in and on a
// catalog switch; this page calls it on load and after every decision.
export async function refreshFindingsBadge() {
    const badge = document.getElementById('findings-badge');
    const catalog = store.catalog;
    if (!badge) return;
    if (!catalog || !store.whoami) { badge.hidden = true; return; }
    try {
        const counts = await api.get(maintenancePath(catalog, '/counts'), { quiet: true });
        if (catalog !== store.catalog) return;   // switched meanwhile: the newer call wins
        badge.textContent = String(counts.open);
        badge.hidden = counts.open === 0;
    } catch {
        badge.hidden = true;
    }
}

// A retry key. randomUUID needs a secure context (https or localhost);
// getRandomValues works everywhere.
function newKey() {
    if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
    return Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, '0')).join('');
}

function columns() {
    return [
        { key: 'disposition', label: 'State', sort: f => f.disposition,
            render: f => h('span', {}, dispositionPill(f.disposition), f.outdated ? [' ', pill('outdated', 'error')] : null) },
        { key: 'concept', label: 'Concept', class: 'mono', render: f => conceptOf(f) ?? '-', sort: f => conceptOf(f) ?? '' },
        { key: 'claim', label: 'Claim', render: f => f.input.assertion },
        { key: 'category', label: 'Category', render: f => words(f.input.category), sort: f => f.input.category },
        { key: 'run_id', label: 'Run', class: 'mono small' },
        { key: 'created_at', label: 'Created', render: f => ago(f.created_at), sort: f => f.created_at },
    ];
}

function showFinding(catalog, finding, onChange) {
    const body = openDrawer('Finding', finding.id);
    // The decision in flight: retrying the same decision reuses its key, so a
    // lost response cannot append it twice.
    let attempt = null;

    async function decide(disposition, rationale, buttons) {
        if (!attempt || attempt.disposition !== disposition || attempt.rationale !== rationale) {
            attempt = { disposition, rationale, key: newKey() };
        }
        buttons.forEach(button => { button.disabled = true; });
        try {
            const updated = await api.post(maintenancePath(catalog, `/findings/${enc(finding.id)}/disposition`),
                { disposition, rationale, idempotency_key: attempt.key });
            attempt = null;
            toast(`Recorded: ${DISPOSITIONS.find(d => d.value === disposition).label}`, 'ok');
            draw(updated);
            onChange();
            refreshFindingsBadge();
        } catch {
            buttons.forEach(button => { button.disabled = false; });   // the API showed why
        }
    }

    function actions() {
        const rationale = h('textarea', { id: 'dispose-rationale', rows: 3, placeholder: 'Why (recorded with your key\'s actor)' });
        const buttons = DISPOSITIONS.map(({ value, label }) => h('button', {
            type: 'button', id: `dispose-${value}`, class: 'secondary',
            onclick: () => decide(value, rationale.value.trim(), buttons),
        }, label));
        return h('div', { id: 'dispose-actions' },
            h('h3', {}, 'Decide'),
            h('label', {}, 'Rationale', rationale),
            h('div', { class: 'dispose-buttons' }, buttons));
    }

    function draw(current) {
        const input = current.input;
        body.replaceChildren(h('div', { class: 'finding' },
            h('p', { id: 'finding-state' }, dispositionPill(current.disposition),
                current.outdated ? [' ', pill('outdated', 'error')] : null,
                ' ', h('span', { class: 'muted small' }, `${words(input.category)}, ${words(input.relationship)}`)),
            h('h3', {}, 'Claim'),
            h('p', { id: 'finding-claim' }, input.assertion),
            h('dl', {},
                h('dt', {}, 'Follow-up'), h('dd', {}, input.follow_up),
                h('dt', {}, 'Uncertainty'), h('dd', {}, input.uncertainty),
                h('dt', {}, 'Reported by'), h('dd', {}, `${current.actor}, ${when(current.created_at)}`),
                h('dt', {}, 'Run'), h('dd', {}, h('a', { href: `#/runs/${enc(current.run_id)}`, class: 'mono', onclick: closeDrawer }, current.run_id))),
            h('h3', {}, 'Evidence'),
            h('ul', { id: 'finding-evidence' }, input.evidence.map(item => h('li', {},
                h('a', { href: browseHash(catalog, item.pin.path), class: 'mono', onclick: closeDrawer }, item.pin.path),
                h('span', { class: 'muted small' }, ` pinned at revision ${item.pin.revision}`),
                h('pre', { class: 'diff' }, item.excerpt)))),
            input.external.length
                ? [h('h3', {}, 'External observations'),
                    h('ul', {}, input.external.map(o => h('li', {},
                        h('span', { class: 'mono' }, o.resource), ` (${o.attribution})`,
                        h('pre', { class: 'diff' }, o.excerpt))))]
                : null,
            h('h3', {}, 'History'),
            current.dispositions.length
                ? h('ol', { id: 'finding-history' }, current.dispositions.map(d => h('li', {},
                    dispositionPill(d.disposition), ` ${d.actor}, ${when(d.at)}`,
                    d.rationale ? h('p', { class: 'small' }, d.rationale) : null)))
                : h('p', { id: 'finding-history', class: 'muted' }, 'No decisions yet.'),
            store.can('write') ? actions() : h('p', { class: 'muted small' }, 'A write key can record a decision.')));
    }

    draw(finding);
}

async function openFinding(catalog, id, onChange) {
    try {
        showFinding(catalog, await api.get(maintenancePath(catalog, `/findings/${enc(id)}`)), onChange);
    } catch { /* the API showed a toast */ }
}

export async function render(main, rest) {
    const catalog = store.catalog;
    refreshFindingsBadge();
    if (!catalog) {
        main.append(h('h1', {}, 'Findings'), h('p', { class: 'muted' }, 'No catalog selected.'));
        return;
    }
    let filter = 'open';
    let rows = [];
    let next = null;
    let ticket = 0;   // a newer load wins over a slower older one

    async function load(reset) {
        const mine = ++ticket;
        const query = new URLSearchParams({ limit: String(PAGE_SIZE) });
        if (filter !== 'all') query.set('disposition', filter);
        if (!reset && next) query.set('after', next);
        try {
            const page = await api.get(`${maintenancePath(catalog, '/findings')}?${query}`);
            if (mine !== ticket) return;
            rows = reset ? page.items : rows.concat(page.items);
            next = page.next_after ?? null;
            table.redraw(rows);
            more.hidden = !next;
        } catch { /* the API showed a toast */ }
    }

    const table = listTable(columns(), rows, {
        onRow: finding => showFinding(catalog, finding, () => load(true)),
        empty: 'No findings here.',
    });
    table.id = 'findings-table';
    const more = h('button', { type: 'button', id: 'findings-more', class: 'secondary', hidden: true, onclick: () => load(false) }, 'Load more');
    const filterBar = segments([...DISPOSITIONS.map(({ value, label }) => ({ value, label })), { value: 'all', label: 'All' }],
        filter, value => { filter = value; load(true); });
    filterBar.id = 'findings-filter';
    main.append(
        h('header', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Findings'),
                h('p', { class: 'muted' }, `Report-only maintenance findings in ${catalog}, oldest first. A decision never edits the catalog.`)),
            h('div', { class: 'actions' }, h('button', { type: 'button', id: 'findings-refresh', class: 'secondary', onclick: () => load(true) }, 'Refresh'))),
        filterBar, table, more);
    await load(true);
    if (rest[0]) await openFinding(catalog, rest[0], () => load(true));
}
```

`load` is a function declaration, so it can be called above where it is written. It only runs after `table` and `more` exist: the first call is the `await load(true)` near the end.

`src/http/ui/js/pages/runs.js`:

```js
// Maintenance runs, newest first; a run's drawer lists its work items, and
// each item links to its concept and its findings.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { maintenancePath, refreshFindingsBadge } from './findings.js';
import { browseHash } from '../core/router.js';

const STATE_KIND = { running: 'info', completed: 'ok', failed: 'error', cancelled: 'muted' };
const OUTCOME_KIND = { completed: 'ok', inconclusive: 'warn', failed: 'error', superseded: 'muted' };
const PAGE_SIZE = 200;

// Every run, newest first. The API pages oldest first, so read every page:
// runs are few (one per scheduled pass).
async function allRuns(catalog) {
    const runs = [];
    let after = null;
    do {
        const query = new URLSearchParams({ limit: String(PAGE_SIZE) });
        if (after) query.set('after', after);
        const page = await api.get(`${maintenancePath(catalog, '/runs')}?${query}`);
        runs.push(...page.items);
        after = page.next_after;
    } while (after);
    return runs.reverse();
}

function columns() {
    return [
        { key: 'state', label: 'State', render: r => pill(r.state, STATE_KIND[r.state] ?? 'muted'), sort: r => r.state },
        { key: 'id', label: 'Run', class: 'mono small' },
        { key: 'actor', label: 'Actor', class: 'mono' },
        { key: 'work', label: 'Work', render: r => `${r.completed_count}/${r.primary_count} completed`, sort: r => r.primary_count },
        { key: 'other', label: 'Failed / inconclusive / superseded', render: r => `${r.failed_count} / ${r.inconclusive_count} / ${r.superseded_count}` },
        { key: 'created_at', label: 'Created', render: r => ago(r.created_at), sort: r => r.created_at },
        { key: 'ended_at', label: 'Ended', render: r => (r.ended_at ? ago(r.ended_at) : '-'), sort: r => r.ended_at ?? '' },
    ];
}

async function openRun(catalog, id) {
    let report;
    try {
        report = await api.get(maintenancePath(catalog, `/runs/${enc(id)}`));
    } catch {
        return;   // the API showed a toast
    }
    const { run, work } = report;
    const items = listTable([
        { key: 'pin', label: 'Concept', sort: w => w.pin.path,
            render: w => h('a', { href: browseHash(catalog, w.pin.path), class: 'mono', onclick: closeDrawer }, `${w.pin.path} @ ${w.pin.revision}`) },
        { key: 'reasons', label: 'Lanes', render: w => w.reasons.join(', ') },
        { key: 'outcome', label: 'Outcome', render: w => (w.outcome ? pill(w.outcome, OUTCOME_KIND[w.outcome] ?? 'muted') : pill('leased', 'info')) },
        { key: 'assessment', label: 'Assessment', render: w => (w.assessment ? w.assessment.replaceAll('_', ' ') : '-') },
        { key: 'summary', label: 'Summary', render: w => w.summary ?? '-' },
        { key: 'finding_ids', label: 'Findings', render: w => (w.finding_ids.length
            ? w.finding_ids.flatMap((finding, index) => [index ? ' ' : null,
                h('a', { href: `#/findings/${enc(finding)}`, class: 'mono small', onclick: closeDrawer }, finding)])
            : '0') },
    ], work, { empty: 'No work items.' });
    items.id = 'run-work';
    openDrawer('Run', run.id, h('div', { class: 'run' },
        h('p', { id: 'run-state' }, pill(run.state, STATE_KIND[run.state] ?? 'muted'), ` by ${run.actor}`),
        h('dl', {},
            h('dt', {}, 'Created'), h('dd', {}, when(run.created_at)),
            h('dt', {}, 'Deadline'), h('dd', {}, when(run.deadline)),
            h('dt', {}, 'Ended'), h('dd', {}, when(run.ended_at)),
            h('dt', {}, 'Reason'), h('dd', {}, run.reason ?? '-'),
            h('dt', {}, 'Work'), h('dd', {}, `${run.completed_count}/${run.primary_count} completed, ${run.failed_count} failed, `
                + `${run.inconclusive_count} inconclusive, ${run.superseded_count} superseded`)),
        h('h3', {}, 'Work items'),
        items));
}

export async function render(main, rest) {
    const catalog = store.catalog;
    refreshFindingsBadge();
    main.append(h('header', { class: 'page-head' },
        h('div', {}, h('h1', {}, 'Runs'),
            h('p', { class: 'muted' }, catalog ? `Maintenance runs in ${catalog}, newest first.` : 'No catalog selected.'))));
    if (!catalog) return;
    const table = listTable(columns(), [], { onRow: run => openRun(catalog, run.id), empty: 'No maintenance runs yet.' });
    table.id = 'runs-table';
    main.append(table);
    try {
        table.redraw(await allRuns(catalog));
    } catch { /* the API showed a toast */ }
    if (rest[0]) await openRun(catalog, rest[0]);
}
```

`src/http/ui/js/main.js` (Task 1's file; the only shared JS edit):
- add `import { refreshFindingsBadge } from './pages/findings.js';` beside the other imports;
- in `enter()`, after `await start();`, add `refreshFindingsBadge();`;
- in the `#catalog-select` change listener, after `route();`, add `refreshFindingsBadge();`.

Append to `src/http/ui/styles.css`:

```css
/* findings and runs (M9 task 6) */
.finding dl, .run dl { display: grid; grid-template-columns: max-content 1fr; gap: .25rem 1rem; }
.finding dt, .run dt { color: var(--fg-muted); font-size: .8rem; }
.finding dd, .run dd { margin: 0; }
.dispose-buttons { display: flex; flex-wrap: wrap; gap: .5rem; }
.dispose-buttons button, #findings-more { width: auto; margin: .5rem 0 0; }
```

- [ ] **Step 8: The browser journeys**

`tests/ui_e2e/maintenance.rs`:

```rust
//! Findings and Runs: a writer dismisses a seeded finding and watches its
//! state and the badge change; a reader sees the run, its work and the
//! finding, but no decision buttons.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};

use crate::harness::drive;
use crate::support::server::{call, files, structured, TestServer};

const ALPHA: &str = "---\ntype: system\ntitle: Alpha\ndescription: The first system.\n---\n# Alpha\n\nAlpha runs [Beta](/systems/beta.md).\n";
const BETA: &str = "---\ntype: system\ntitle: Beta\ndescription: The second system.\n---\n# Beta\n\nBeta serves [Alpha](/systems/alpha.md).\n";
const CLAIM: &str = "The owner of this system is not recorded.";

/// A server whose `main` has one completed run with one open finding,
/// reported the way the runner reports: a reporter key through MCP.
///
/// # Returns
/// The server, the run id and the finding id.
async fn seeded() -> (TestServer, String, String) {
    let server = TestServer::start(
        vec![(
            "main",
            files(&[("index.md", "# Main\n"), ("systems/alpha.md", ALPHA), ("systems/beta.md", BETA)]),
        )],
        Config::default(),
    )
    .await;
    let reporter = server.reporter_key("console-reporter", &["main"]).await;
    let client = server.client(&reporter, None).await.expect("a reporter client");
    let claimed: Value = structured(
        &call(
            &client,
            "maintenance_claim",
            json!({"catalog": "main", "idempotency_key": "seed-claim", "batch_size": 1}),
        )
        .await,
    )
    .clone();
    let lease = &claimed["leases"][0];
    // Both concepts contain this line, whichever one is leased.
    let evidence = json!([{"pin": lease["pin"], "excerpt": "type: system"}]);
    let submission = json!({
        "outcome": "completed", "assessment": "supported_current",
        "summary": "Synthetic investigation.", "evidence": evidence,
        "findings": [{"category": "unverifiable_claim", "assertion": CLAIM,
                      "follow_up": "Ask the owner.", "uncertainty": "Synthetic only.",
                      "relationship": "direct_statement", "evidence": evidence, "external": []}],
        "reported_tokens": null,
    });
    let receipt: Value = structured(
        &call(
            &client,
            "maintenance_submit",
            json!({"catalog": "main", "work_id": lease["work_id"], "token": lease["token"],
                   "idempotency_key": "seed-submit", "submission": submission}),
        )
        .await,
    )
    .clone();
    let run = claimed["run_id"].as_str().expect("a run id").to_string();
    let finding = receipt["finding_ids"][0].as_str().expect("a finding id").to_string();
    (server, run, finding)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_dismisses_a_finding_and_its_state_changes() {
    let (server, _run, finding) = seeded().await;
    let key = server.key("console-writer", Access::Write, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#findings-badge", "1");
        ui.open("/findings");
        ui.wait_text("#findings-table tbody", CLAIM);
        ui.wait_text("#findings-table tbody", "Open");
        ui.click("#findings-table tbody tr.clickable");
        ui.wait_text("#drawer", &finding);
        ui.wait_text("#finding-evidence", "/systems/");
        ui.wait_text("#finding-history", "No decisions yet.");
        ui.fill("#dispose-rationale", "Covered by the owner list.");
        ui.click("#dispose-dismissed");
        ui.wait_text("#finding-state", "Dismissed");
        ui.wait_text("#finding-history", "console-writer");
        ui.wait_hidden("#findings-badge");
        ui.click("#drawer-close");
        // The Open view no longer lists it; the Dismissed view does.
        ui.wait_text("#findings-table tbody", "No findings here.");
        ui.click("#findings-filter .seg[data-value=dismissed]");
        ui.wait_text("#findings-table tbody", "Dismissed");
        ui.wait_text("#findings-table tbody", CLAIM);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_sees_the_run_and_its_work_but_cannot_decide() {
    let (server, run, finding) = seeded().await;
    let key = server.key("console-reader", Access::Read, &["main"], Some("main")).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/runs");
        ui.wait_text("#runs-table tbody", &run);
        ui.wait_text("#runs-table tbody", "completed");
        ui.click("#runs-table tbody tr.clickable");
        ui.wait_text("#run-state", "completed");
        ui.wait_text("#run-work", "/systems/");
        ui.wait_text("#run-work", &finding);
        // The work item's finding link opens that finding's drawer, read-only.
        ui.click(&format!("#run-work a[href='#/findings/{finding}']"));
        ui.wait_text("#finding-claim", CLAIM);
        assert!(!ui.page.is_visible("#dispose-actions").expect("visible"));
    })
    .await;
}
```

`tests/ui_e2e/main.rs`: add `mod maintenance;` (keep the other tasks' lines).

Run: `just test-ui`
Expected: `a_writer_dismisses_a_finding_and_its_state_changes` and `a_reader_sees_the_run_and_its_work_but_cannot_decide` pass with Task 1's journeys, with no skip lines.

- [ ] **Step 9: Docs and changelog**

- `docs/operations.md`, "Web console", subsection "Findings and runs". Rows for the API reference table:

  | Method | Path | Access | Notes |
  |---|---|---|---|
  | GET | `/api/v1/catalogs/{catalog}/maintenance/runs?after=&limit=` | read | As `maintenance_list_runs`: oldest first; `limit` 1..=200 (default 20; outside is 409); `after` is the previous page's last id (`next_after`). |
  | GET | `/api/v1/catalogs/{catalog}/maintenance/runs/{run}` | read | As `maintenance_get_run`: the run summary and up to 50 work items, with their finding ids. |
  | GET | `/api/v1/catalogs/{catalog}/maintenance/findings?after=&limit=&disposition=` | read | As `maintenance_list_findings`, optionally narrowed to one current disposition (`open`, `accepted_for_follow_up`, `dismissed`, `needs_input`; another value is 400). `limit` and `next_after` count matching findings only. |
  | GET | `/api/v1/catalogs/{catalog}/maintenance/findings/{finding}` | read | As `maintenance_get_finding`: claim, evidence with pinned revisions, decision history, `outdated`. |
  | GET | `/api/v1/catalogs/{catalog}/maintenance/counts` | read | `{catalog, open, accepted_for_follow_up, dismissed, needs_input}` by current disposition (the nav badge). |
  | POST | `/api/v1/catalogs/{catalog}/maintenance/findings/{finding}/disposition` | write | Body `{disposition, rationale, idempotency_key}` (unknown fields are 422). As `maintenance_dispose`: appends a decision and never edits the catalog. A retry with the same key and body returns the original response; the same key with a different body is 409. The reporting flag grants nothing here: a read-level reporter key gets 403. |

  Then one paragraph: the Findings page lists open findings first, with filters for the other dispositions. Its drawer records a decision with a rationale; the console makes the retry key. The Runs page lists runs newest first, and the nav badge shows the selected catalog's open findings.
- `README.md`, "Web console": add "Maintenance findings can be triaged (accepted, dismissed, needs input, reopened) and runs inspected from the browser."
- `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m9-task-06-findings-runs.md`:
  - Added: six routes, Findings and Runs pages, nav badge, `list_findings_with`, `count_findings`.
  - Changed: `page_ids` takes a `Listing`; `maintenance_list_findings` delegates to `maintenance_list_findings_by` (MCP schema unchanged).
  - Notes: `rationale` instead of the spec's `note`; the counts route is an addition.

- [ ] **Step 10: Gates and commit**

Run every gate in "Conventions". This task changes `src/mcp/maintenance/reports.rs` by delegation only (no behaviour change), so the parity and roundtrip runs are not needed; `cargo test --test maintenance` covers the MCP path. Then:

```bash
git add src/store/maintenance/mod.rs src/store/maintenance/runs.rs src/store/maintenance/reports.rs \
    src/store/maintenance/report_tests.rs src/mcp/maintenance/reports.rs src/http/api/maintenance.rs \
    src/http/ui/js/pages/findings.js src/http/ui/js/pages/runs.js src/http/ui/js/main.js \
    src/http/ui/styles.css tests/ui_e2e/maintenance.rs tests/ui_e2e/main.rs docs/operations.md \
    README.md changelog/
git commit -m "feat(console): maintenance findings and runs, disposition filter and badge (M9 task 6)"
```

---

### Task 7: Changesets

Depends on Task 1. Parallel with Tasks 2 to 6.

**Facts this task relies on** (verified at `62e88cd`):
- `list_changesets(ctx, ListParams { catalog: Option<String> }) -> Result<Listed { changesets: Vec<ListedChangeset> }>` (`src/mcp/changesets.rs:104`; params 38, row 45). `ListedChangeset { changeset, catalog, key: Option<String>, key_name: Option<String>, actor, created, last_change, idle, paths }`, timestamps already formatted. With `catalog`, `ctx.one(Some(name))` refuses an unreached catalog with `CatalogNotSelected` (404) (`src/mcp/context.rs:205`, `src/selection.rs:156`); without it, every catalog the key reaches. A non-admin key gets its own changesets only (`key_filter`, :121-124), an admin every key's. A read key owns none, so it gets an empty list.
- `pending_changes(ctx, PendingParams { changeset }) -> PendingChanges { catalog, changeset, files: Vec<PendingFile { path, change, base_revision, current_revision, stale, replayable, diff }> }` (:612; params 395; file 376) resolves through `ctx.changeset(handle, Access::Read)`.
- `discard(ctx, DiscardParams { paths: Option<Vec<String>>, changeset }) -> Discarded { catalog, changeset, removed, closed }` (:456; params 343; result 352) resolves through `ctx.changeset(handle, Access::Write)`; `paths: None` drops every entry and closes the changeset for good. Discarding a closed one is `Refused` (409) (`src/mcp/context.rs:354-364`).
- `ToolContext::changeset(handle, access)` (`src/mcp/context.rs:325`): `UnknownChangeset` (404) for an unknown handle, a catalog the key does not reach, or another key's changeset when the caller is not admin (checked **before** the access level, :340-350); then `require(access)` (403); `Refused` (409) for a published or discarded one.
- `open_changeset(ctx, OpenParams { catalog: Option<String> }) -> Opened { changeset, catalog }` (:204; params 177) needs write. A changeset's `key_id` is the opening principal's `key_id`, a foreign key to `api_keys.id`.
- `api::testing::keys` creates real keys through `create_with_id` and returns only their secrets; `App::keys().find(secret) -> Option<Arc<Principal>>` (`src/auth.rs:228`, `src/app.rs:152`) yields the real principal (its `key_id` is the generated id), so tests can open changesets as those keys without the ids.
- `crate::mcp::testing::{pending_entry, PendingEdit { path, text, at }}` (`src/mcp/testing.rs:268-321`) store an entry in the lock order, without the write rules.
- `tests/support/server.rs`: `TestServer::client(key, catalogs) -> Result<Client, String>` (234), `call(client, tool, args) -> CallToolResult` (323), `open_changeset(client, catalog) -> String` (346), `structured(result) -> &Value` (370). `edit_document` takes `{path, old_str, new_str, replace_all?, changeset}` (`src/mcp/writes.rs:43-55`).

**Access, as the tools enforce it** (reflected in the matrix):

| Route | read | write | admin | stranger |
|---|---|---|---|---|
| `GET /changesets` | 200 (empty: owns none) | 200 (own) | 200 (every key's in reach) | 200 (its reach only) |
| `GET /changesets?catalog=main` | 200 | 200 | 200 | 404 (unreached catalog) |
| `GET /changesets/{writer's}/diff` | 404 (not its own) | 200 (owner) | 200 | 404 (unreached) |
| `DELETE /changesets/{admin's}` | 404 | 404 (not its own) | 200 | 404 |

The spec's "read" for the diff is in practice "the owner or an admin", and a read key owns none (Decision 3 of M8: another key's handle is 404, never 403). Task 8 records this in the spec.

**Files:**
- Modify: `src/http/api/changesets.rs` (replaces Task 1's stub)
- Create: `src/http/ui/js/pages/changesets.js`
- Modify: `src/http/ui/styles.css` (append the "changesets" block)
- Create: `tests/ui_e2e/changesets.rs`; Modify: `tests/ui_e2e/main.rs` (`mod changesets;`)
- Modify: `README.md`, `docs/operations.md` ("Web console" subsection "Changesets")
- Create: `changelog/<ts>-m9-task-07-changesets.md`

- [ ] **Step 1: (none)** Task 1's `api::testing::assert_matrix_with` already lets a matrix run with keys made beforehand; this task uses it.

- [ ] **Step 2: Write the failing API tests**

At the bottom of `src/http/api/changesets.rs` (below the stub for now; explicit imports, so it compiles against the stub):

```rust
#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::http::api::context;
    use crate::http::api::testing::{app, assert_matrix_with, keys, send, Row};
    use crate::mcp::changesets::{open_changeset, OpenParams};
    use crate::mcp::testing::{pending_entry, PendingEdit, TestApp};

    /// Opens a changeset in `main` as the key `secret` and stores one new
    /// file in it; returns the handle.
    async fn open_as(test: &TestApp, secret: &str, path: &str) -> String {
        let principal = test.app.keys().find(secret).expect("a live key");
        let ctx = context(&test.app, principal);
        let handle = open_changeset(&ctx, OpenParams { catalog: Some("main".to_string()) })
            .await
            .expect("open a changeset")
            .changeset;
        let edit = PendingEdit { path, text: Some("# Gamma\n\nDraft.\n"), at: Utc::now() };
        pending_entry(test, &handle, &edit).await;
        handle
    }

    fn handles(listed: &serde_json::Value) -> Vec<String> {
        listed["changesets"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|row| row["changeset"].as_str().map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn changeset_routes_follow_the_tools_access_rules() {
        let test = app().await;
        let keys = keys(&test).await;
        let writers = open_as(&test, &keys.write, "systems/gamma.md").await;
        let admins = open_as(&test, &keys.admin, "systems/delta.md").await;
        assert_matrix_with(
            &test,
            &keys,
            &[
                Row { method: "GET", uri: "/api/v1/changesets".into(), body: None,
                      read: 200, write: 200, admin: 200, stranger: 200 },
                Row { method: "GET", uri: "/api/v1/changesets?catalog=main".into(), body: None,
                      read: 200, write: 200, admin: 200, stranger: 404 },
                Row { method: "GET", uri: format!("/api/v1/changesets/{writers}/diff"), body: None,
                      read: 404, write: 200, admin: 200, stranger: 404 },
                Row { method: "DELETE", uri: format!("/api/v1/changesets/{admins}"), body: None,
                      read: 404, write: 404, admin: 200, stranger: 404 },
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn a_key_lists_its_own_changesets_and_an_admin_every_keys() {
        let test = app().await;
        let keys = keys(&test).await;
        let writers = open_as(&test, &keys.write, "systems/gamma.md").await;
        let admins = open_as(&test, &keys.admin, "systems/delta.md").await;
        let uri = "/api/v1/changesets?catalog=main";

        let (_, as_writer) = send(&test, "GET", uri, Some(&keys.write), None).await;
        assert_eq!(handles(&as_writer), vec![writers.clone()]);
        assert_eq!(as_writer["changesets"][0]["key_name"], "writer");
        assert_eq!(as_writer["changesets"][0]["paths"], json!(["/systems/gamma.md"]));
        assert_eq!(as_writer["changesets"][0]["idle"], false);

        let (_, as_admin) = send(&test, "GET", uri, Some(&keys.admin), None).await;
        let mut all = handles(&as_admin);
        all.sort();
        let mut want = vec![writers, admins];
        want.sort();
        assert_eq!(all, want);

        let (_, as_reader) = send(&test, "GET", uri, Some(&keys.read), None).await;
        assert!(handles(&as_reader).is_empty());
    }

    #[tokio::test]
    async fn the_diff_shows_each_pending_file() {
        let test = app().await;
        let keys = keys(&test).await;
        let handle = open_as(&test, &keys.write, "systems/gamma.md").await;
        let uri = format!("/api/v1/changesets/{handle}/diff");
        let (status, body) = send(&test, "GET", &uri, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 200, "{body}");
        assert_eq!(body["catalog"], "main");
        assert_eq!(body["files"][0]["path"], "/systems/gamma.md");
        assert_eq!(body["files"][0]["change"], "create");
        assert!(body["files"][0]["diff"].as_str().expect("a diff").contains("+# Gamma"));
        let (status, _) = send(&test, "GET", "/api/v1/changesets/no-such-handle/diff",
                               Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 404);
    }

    #[tokio::test]
    async fn the_owner_discards_its_changeset_for_good() {
        let test = app().await;
        let keys = keys(&test).await;
        let handle = open_as(&test, &keys.write, "systems/gamma.md").await;
        let uri = format!("/api/v1/changesets/{handle}");
        let (status, body) = send(&test, "DELETE", &uri, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 200, "{body}");
        assert_eq!(body["removed"], 1);
        assert_eq!(body["closed"], true);
        let (_, listed) = send(&test, "GET", "/api/v1/changesets", Some(&keys.write), None).await;
        assert!(handles(&listed).is_empty());
        // The handle is closed: its diff and a second discard are refused.
        let (status, _) = send(&test, "GET", &format!("{uri}/diff"), Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 409);
        let (status, _) = send(&test, "DELETE", &uri, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 409);
    }
}
```

- [ ] **Step 3: Run the tests to make sure they fail**

Run: `cargo test --lib http::api::changesets -- --test-threads=6 2>&1 | tail -20`
Expected: FAIL. The stub has no routes: `GET /api/v1/changesets as read: ... left: 404, right: 200`, and the other tests fail on their first request's status the same way.

- [ ] **Step 4: Implement the changeset endpoints**

`src/http/api/changesets.rs`, above the test module (replacing the stub):

```rust
//! `/api/v1/changesets` (web console design, "API": Changesets): open
//! changesets, their pending diffs, and discarding one. Thin adapters over
//! the `list_changesets`, `pending_changes` and `discard` tools, so the
//! access rules are the tools' own (M8 Decisions 3 and 8):
//! - list: the key's own changesets, an admin's every key's, in the
//!   catalogs it reaches (a read key owns none);
//! - diff: the owning key or an admin;
//! - discard: the owning write key or an admin.
//!
//! Another key's handle is 404 (`UnknownChangeset`), never 403, so a key
//! cannot tell a real handle from a made-up one.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::auth::Principal;
use crate::http::api::context;
use crate::http::AppError;
use crate::mcp::changesets::{
    discard, list_changesets, pending_changes, DiscardParams, Discarded, ListParams, Listed,
    PendingChanges, PendingParams,
};

/// `GET /changesets`' query.
#[derive(Debug, Deserialize)]
struct ListQuery {
    /// Only this catalog (absent or empty: every catalog the key reaches).
    catalog: Option<String>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/changesets", get(list))
        .route("/changesets/{handle}", delete(remove))
        .route("/changesets/{handle}/diff", get(diff))
}

/// `GET /changesets?catalog=`: as the `list_changesets` tool.
///
/// # Errors
/// 404 for a catalog the key does not reach; 500 if the store fails.
async fn list(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Listed>, AppError> {
    let ctx = context(&app, principal);
    // `filter` turns `?catalog=` (an empty select) into "every catalog".
    let catalog = query.catalog.filter(|name| !name.is_empty());
    Ok(Json(list_changesets(&ctx, ListParams { catalog }).await?))
}

/// `GET /changesets/{handle}/diff`: as the `pending_changes` tool.
///
/// # Errors
/// 404 for a handle the key may not use; 409 for a published or discarded one.
async fn diff(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(handle): Path<String>,
) -> Result<Json<PendingChanges>, AppError> {
    let ctx = context(&app, principal);
    Ok(Json(pending_changes(&ctx, PendingParams { changeset: handle }).await?))
}

/// `DELETE /changesets/{handle}`: drops every entry and closes the
/// changeset for good, as `discard` without `paths`.
///
/// # Errors
/// 404 for a handle the key may not use; 403 for an owner below write
/// (none exists: opening needs write); 409 if it was published or
/// discarded meanwhile.
async fn remove(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(handle): Path<String>,
) -> Result<Json<Discarded>, AppError> {
    let ctx = context(&app, principal);
    let params = DiscardParams { paths: None, changeset: handle };
    Ok(Json(discard(&ctx, params).await?))
}
```

- [ ] **Step 5: Run the tests to make sure they pass**

Run: `cargo test --lib http::api -- --test-threads=6`
Expected: PASS: `changeset_routes_follow_the_tools_access_rules`, `a_key_lists_its_own_changesets_and_an_admin_every_keys`, `the_diff_shows_each_pending_file`, `the_owner_discards_its_changeset_for_good`.

- [ ] **Step 6: The Changesets page**

`src/http/ui/js/pages/changesets.js`:

```js
// Changesets: the open changesets of the selected catalog (the key's own;
// an admin sees every key's), with owner, idle state and file count. The
// drawer shows the pending diff, computed by the server, and discards.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, when, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const COLUMNS = [
    { key: 'changeset', label: 'Handle', class: 'mono',
      render: row => h('span', { class: 'changeset-handle', 'data-test': 'changeset-handle' }, row.changeset) },
    { key: 'catalog', label: 'Catalog', class: 'mono', sort: row => row.catalog },
    { key: 'owner', label: 'Owner', sort: row => row.key_name ?? '',
      render: row => [row.key_name ?? '(deleted key)', ' ', h('span', { class: 'muted small mono' }, row.actor)] },
    { key: 'created', label: 'Opened', sort: row => row.created, render: row => when(row.created) },
    { key: 'last_change', label: 'Last change', sort: row => row.last_change, render: row => ago(row.last_change) },
    { key: 'files', label: 'Files', sort: row => row.paths.length, render: row => count(row.paths.length, 'file') },
    { key: 'idle', label: 'State', sort: row => (row.idle ? 1 : 0),
      render: row => (row.idle ? pill('idle', 'warn') : pill('active', 'ok')) },
];

export async function render(main) {
    const catalog = store.catalog;
    const scope = store.can('admin') ? "Every key's open changesets" : "Your key's open changesets";
    const table = listTable(COLUMNS, [], { onRow: row => show(main, row, load), empty: 'No open changesets.' });
    table.id = 'changesets-table';
    main.append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Changesets'),
                h('p', { class: 'muted' }, `${scope} in ${catalog ?? 'every catalog'}. Idle ones have had no change within the idle timeout.`)),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'changesets-refresh', class: 'secondary', onclick: () => load() }, 'Refresh'))),
        table);

    async function load() {
        try {
            const listed = await api.get(`/changesets?catalog=${enc(catalog ?? '')}`);
            table.redraw(listed.changesets);
        } catch { /* toasted */ }
    }
    await load();
}

// One changeset's drawer: its diff (loaded on open) and Discard. `main` is
// the page's view: once it is detached, the user has moved on and the
// drawer is no longer this page's to fill.
async function show(main, row, reload) {
    const diff = h('div', { id: 'changeset-diff' }, h('p', { class: 'muted' }, 'Loading the diff...'));
    const discardButton = store.can('write')
        ? h('button', { type: 'button', id: 'changeset-discard', class: 'danger', onclick: () => discardOne(main, row, reload) }, 'Discard')
        : null;
    openDrawer(row.changeset,
        `${row.catalog} - ${row.key_name ?? 'deleted key'} (${row.actor}) - last change ${ago(row.last_change)}`,
        row.idle ? h('p', {}, pill('idle', 'warn'), ' No change within the idle timeout.') : null,
        diff,
        h('div', { class: 'drawer-actions' }, discardButton));
    let content;
    try {
        const pending = await api.get(`/changesets/${enc(row.changeset)}/diff`);
        content = pending.files.length
            ? pending.files.map(file => h('section', { class: 'diff-file' },
                h('h3', { class: 'mono small' }, `${file.change} ${file.path}`, ' ', file.stale ? pill('stale', 'warn') : null),
                // The diff is a text child (a text node, like textContent), never markup.
                h('pre', { class: 'diff' }, file.diff)))
            : [h('p', { class: 'muted' }, 'No pending files: the changeset is empty.')];
    } catch (error) {
        content = [h('p', { class: 'error' }, `The diff could not be loaded: ${error.message}`)];
    }
    if (!main.isConnected) return;
    diff.replaceChildren(...content);
}

async function discardOne(main, row, reload) {
    const ok = await confirmDialog({
        title: 'Discard this changeset?',
        message: `${count(row.paths.length, 'pending file')} in ${row.catalog} will be dropped, and the handle ${row.changeset} closed for good. This cannot be undone.`,
        action: 'Discard',
    });
    if (!ok) return;
    try {
        const result = await api.del(`/changesets/${enc(row.changeset)}`);
        if (!main.isConnected) return;
        closeDrawer();
        toast(`Discarded ${count(result.removed, 'file')} from ${result.changeset}.`, 'ok');
        await reload();
    } catch { /* toasted */ }
}
```

Element ids and `data-test` attributes the journey uses: `#changesets-table`, `[data-test=changeset-handle]`, `#changeset-diff`, `#changeset-discard`, `#changesets-refresh`, and Task 1's `#confirm-ok`.

Append to `src/http/ui/styles.css` (`.drawer-actions` is already in Task 1's block, so this one does not repeat it; corrected in issue #217):

```css
/* Changesets (Task 7) */
.diff-file { margin-bottom: 1rem; }
.diff-file h3 { margin: 0 0 .35rem; }
/* A block box, so a handle wrapped at its hyphens is one click target. */
.changeset-handle { display: inline-block; }
```

> **Corrected (issue #222, 2026-11-10).** Two changes to this step as first planned, both corrected inline above. (1) Drafted before Task 1's per-navigation containers: `show` and `discardOne` now take the page's `main`, and return without touching the drawer when `main.isConnected` is false after their request, per "Front-end shape". (2) The handle cell's span has `class: 'changeset-handle'` and `.changeset-handle { display: inline-block; }`. A UUID handle wraps at its hyphens in the seven-column table at the journeys' 800 px viewport; the centre of the inline span's bounding box then fell on the bare cell, and rustwright refused the click with `Actionability(NotReceivingEvents)` (as Task 2's `key-name`, issue #217). An inline block keeps wrapping but is one rectangle. The Rust tests in Step 2 build their matrix rows through a small `row(method, uri, [read, write, admin, stranger])` helper, for rustfmt; the rows are as listed. The journey in Step 7 also waits for `#drawer` to close after the discard.

- [ ] **Step 7: Browser journey (failing first)**

`tests/ui_e2e/changesets.rs`:

```rust
//! The Changesets page: an admin finds a changeset an agent left open,
//! reads its diff and discards it.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::json;

use crate::harness::drive;
use crate::support::server::{call, files, open_changeset, structured, TestServer};

const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha runs.\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_discards_a_changeset_from_the_console() {
    let server = TestServer::start(
        vec![("main", files(&[("index.md", "# Main\n\n- [Alpha](systems/alpha.md)\n"),
                               ("systems/alpha.md", ALPHA)]))],
        Config::default(),
    )
    .await;
    let key = server.key("console-admin", Access::Admin, &["main"], Some("main")).await;
    // An agent's unfinished work: one edit in an open changeset.
    let client = server.client(&key, None).await.expect("connect");
    let handle = open_changeset(&client, Some("main")).await;
    let edited = call(&client, "edit_document", json!({
        "path": "/systems/alpha.md", "old_str": "Alpha runs.", "new_str": "Alpha runs well.",
        "changeset": handle,
    }))
    .await;
    structured(&edited);

    let shown = handle.clone();
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/changesets");
        ui.wait_text("#changesets-table", &shown);
        ui.click("[data-test=changeset-handle]");
        ui.wait_text("#changeset-diff", "+Alpha runs well.");
        ui.click("#changeset-discard");
        ui.click("#confirm-ok");
        ui.wait_text("#changesets-table", "No open changesets");
        ui.wait_hidden("#drawer");
    })
    .await;

    // The server agrees: nothing is open any more.
    let listed = call(&client, "list_changesets", json!({})).await;
    assert_eq!(structured(&listed)["changesets"], json!([]));
}
```

`tests/ui_e2e/main.rs`: add `mod changesets;`.

Run: `cargo test --test ui_e2e changesets` before `changesets.js` exists.
Expected: FAIL. The route shows "Not available yet", so the journey times out on `#changesets-table`.

- [ ] **Step 8: Run the journey**

Run: `just test-ui`
Expected: PASS, including `an_admin_discards_a_changeset_from_the_console`, with no skip lines.

- [ ] **Step 9: Docs and changelog**

> **Corrected (issue #222, 2026-11-10).** As issue #218's note under Task 3 says, the rows below went into the `### Changesets` subsection's own three-column table (`Method and path | Key | Does`), placed before "Testing the console", with a `curl` example on `okf-mcp.example.com`.

`docs/operations.md`, section "Web console": add subsection "Changesets", saying that the page lists the selected catalog's open changesets (a key sees its own, an admin every key's), highlights idle ones (no change within `session_idle_timeout`), shows a changeset's diff, and discards it; that these are the `list_changesets`, `pending_changes` and `discard` tools over HTTP, with the same rules; and that another key's handle answers 404. API reference rows:

| Method | Path | Access | Notes |
|---|---|---|---|
| GET | `/api/v1/changesets?catalog=` | read | The key's own open changesets; an admin's: every key's. `catalog` optional (every reached catalog). As `list_changesets`. |
| GET | `/api/v1/changesets/{handle}/diff` | owner or admin | As `pending_changes`. 404 for another key's handle; 409 once published or discarded. |
| DELETE | `/api/v1/changesets/{handle}` | write (owner) or admin | As `discard` without `paths`: drops every entry and closes the handle for good. |

`README.md`, section "Web console": one sentence that stale agent changesets can be inspected and discarded at `/ui/#/changesets`.

`changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m9-task-07-changesets.md`: Added (the three endpoints, the page, the journey), Changed (none), Notes (the diff is owner-or-admin in practice; another key's handle is 404).

- [ ] **Step 10: Gates and commit**

Run every gate in "Conventions". Then:

```bash
git add src/http/api/changesets.rs src/http/ui/js/pages/changesets.js \
    src/http/ui/styles.css tests/ui_e2e/changesets.rs tests/ui_e2e/main.rs README.md \
    docs/operations.md changelog/
git commit -m "feat(console): changeset endpoints and page (M9 task 7)"
```

---

### Task 8: Release v0.12.0 and wrap-up

Depends on Tasks 1 to 7. The rollout steps wait for pgrace's go-ahead.

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock` (version, through `just release-prepare`)
- Modify: `README.md` (status line and the "Web console" section, checked against what shipped)
- Modify: `docs/operations.md` (the complete "Web console" API table; the upgrade note)
- Modify: `docs/superpowers/specs/2026-11-09-okf-webui-design.md` (deviations 1 to 5; a closing "What M9 shipped differently" section)

  > **Corrected (issue #218, 2026-11-10).** Also reconcile the spec's "Delivery" phase 3 sentence ("Until phase 4 lands, report rows link to the raw document endpoint"): Health's rows link to Browse through `browseHash` from the start, and the router shows "Not available yet" until Task 4 lands, as "Task dependencies" decided. No raw document endpoint exists under `/api/v1` (review of PR #225).
- Create: `changelog/<ts>-m9-web-console.md` (milestone changelog) and the release entry `just release-prepare` writes

- [ ] **Step 1: Check the docs against the code**

For every route in `src/http/api/*.rs`, `rg '\.route\("' src/http/api` must have a matching row in `docs/operations.md`'s API table, with the right method, path and access, and a row in that area's `assert_matrix` test. Add anything missing.

- [ ] **Step 2: Whole-console journey**

Add `tests/ui_e2e/smoke.rs` (and `mod smoke;` in `main.rs`) with one journey that an admin runs across every page in turn:
- **Health:** the tiles render.
- **Browse:** open `systems/alpha.md`.
- **Search:** `alpha`.
- **Findings and Runs:** the empty states render.
- **Changesets:** the empty state renders.
- **Catalogs:** `main` is listed.
- **Import & export:** the export link is present.
- **API keys:** the admin's own key is listed.

There must be no console errors. Run `just test-ui`, expecting every journey to pass.

- [ ] **Step 3: Spec, milestone changelog**

- **Spec:** record deviations 1 to 13 from this plan in place, and add "What M9 shipped differently", listing every `> **Corrected (issue #N).**` note in this plan.
- **`changelog/<ts>-m9-web-console.md`:** summarize Tasks 1 to 7 with their PR numbers.

- [ ] **Step 4: Prepare the release**

Run `just release-prepare minor` (0.11.2 to 0.12.0). Follow `docs/operations.md` "Preparing and shipping a release": open the release PR and get it reviewed. **Stop and ask pgrace** before merging it, before `just release-ship` (tag and image), and before any rollout.

- [ ] **Step 5: Gates and commit**

Run every gate in "Conventions", plus `cargo test --release --test smoke -- --ignored`. Then:

```bash
git add README.md docs/ changelog/ tests/ui_e2e/
git commit -m "docs: M9 wrap-up, web console reference and whole-console journey (M9 task 8)"
```

## Self-review (2026-11-09)

**Spec coverage.** Every spec section is covered:

| Spec section | Task |
|---|---|
| Identity | 1 |
| Keys | 2 |
| Health and reports | 3 |
| Tree, concept, links, history, revisions, documents, search, markdown rendering | 4 |
| Catalogs, import, export | 5 |
| Runs, findings, dispositions | 6 |
| Changesets | 7 |
| Security headers and CSP | 1 |
| Access matrix | 1 (helper) and every area |
| Browser journeys | 1 to 7, with a whole-console journey in 8 |
| Docs and release | each task and 8 |

**Placeholders.** None. Every code step has complete code. The only deliberately deferred work is in Scope's table.

**Consistency checks across tasks:**
- `browseHash` and `refreshIdentity` are defined once in Task 1 (`router.js`, `shell.js`) and imported by Tasks 3 to 6.
- `assert_matrix_with` is in Task 1, for Tasks 6 and 7.
- `.drawer-actions` is in Task 1's `styles.css`.
- `require_admin` is in Task 1, used by Tasks 2 and 5.
- Every area uses `{catalog}`.

**Compiled so far.** While drafting Task 4, its author built Task 1 and Task 4 from this plan's code blocks in a scratch copy of the repository and ran them. Pico was an empty file and the fonts link was removed, for offline use. The results:
- clippy (`-D warnings`), fmt and doc were clean, after the `require_admin` fix now in Task 1;
- the renderer, API and access-matrix tests passed;
- the 4 browse journeys and Task 1's harness ran in headless Chromium.

Tasks 2, 3, 5, 6 and 7 have not been compiled. Their facts are cited against the source, and each task's Corrected-note rule applies.

**Open checks for implementers** (from the drafts):
- that rustwright's console capture ignores or includes "Failed to load resource" (the harness filters it either way);
- `Option<Disposition>` in `Query` (Task 6);
- `clippy::large_enum_variant` on Task 3's `Report`;
- that an `app_with` catalog can be claimed for maintenance (Task 6);
- `is_visible` inside a closed `<details>` (Task 4 checks `details.open` instead);
- that rustwright's `evaluate` awaits a returned promise (Task 1's router journey).
