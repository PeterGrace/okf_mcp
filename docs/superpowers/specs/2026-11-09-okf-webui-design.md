# okf-mcp web console: design

Status: approved in brainstorming, 2026-11-09; delivered in milestone 9
(epic #215, plan `docs/superpowers/plans/2026-11-09-okf-mcp-m9-web-console.md`).
Where the plan deviated, this document says so in place ("Plan deviation N",
numbered as in the plan's "Simplifications and deviations"), and "What M9
shipped differently" at the end lists the rest.

## Goal

A browser console for `okf-mcp` that covers three needs the MCP tools and
the CLI serve badly today:

1. **API key management**: what `okf-mcp key ...` does, without a shell on the
   server.
2. **Catalog maintenance**: health dashboards (lint, sprawl, links, oldest,
   open questions), catalog administration with `.tar.gz` import and export
   from the browser (no more piping archives through `kubectl exec`),
   maintenance findings triage, and changeset housekeeping.
3. **Browsing**: read-only navigation of catalogs, concepts, links, history
   and search.

Every operation the console performs is also a documented JSON API call, so
the same work can be scripted with `curl`.

## Non-goals (this round)

- Editing concepts (body edits, frontmatter, stamp, review, questions,
  create, move, split, delete, publish). Browsing is read-only; the only
  writes are administrative (keys, catalogs, import) plus finding
  dispositions and changeset discard.
- Any identity system beyond API keys (no OIDC, no passwords, no users table).
- A JavaScript build step, npm, or a component framework.

## Decisions

| Question | Decision |
|---|---|
| Sign-in | Paste an existing API key. The console acts with exactly that key's access. |
| Key storage in the browser | `localStorage` (persists across tabs and restarts until sign-out or revocation). |
| Architecture | JSON REST API under `/api/v1` plus a vanilla-JS single-page app under `/ui`, both served by the existing `okf-mcp` binary on its existing port. |
| Rendering and analysis | In Rust on the server: markdown to HTML, diffs, every report. JS only fetches, displays, filters and sorts. |
| Look and feel | Sidebar app in the visual language of the pr-review-bot admin UI (see "Visual design"). |
| JS tests | None in Node. Behaviour is covered by browser end-to-end tests driven by `rustwright` from `cargo test`. |
| Phase order | Foundation, API keys, health, browse and search, catalogs and import/export, findings and runs, changesets. |

## Architecture

```
browser ── GET /ui/* (no key) ──────────────► embedded static assets
        ── /api/v1/* (Authorization: Bearer) ─► require_key ─► api handlers
                                                            │
                                   ┌────────────────────────┼─────────────────────┐
                                   ▼                        ▼                     ▼
                           mcp::* tool fns          store::keys / catalogs   http::archives
                           (via ToolContext)        (as the CLI uses them)   (import / export)
```

- **`/api/v1/*`** is nested beside the existing keyed router and applies the
  same `require_key` middleware itself (inside the layer that adds its
  headers, so a 401 carries them too), and gets a `Principal` like every
  other keyed route. An unknown `/api/v1` path is 404, key or not. Handlers are thin adapters: they build the same
  `ToolContext` the MCP tools build from the `Principal`, call the existing
  `pub(crate)` tool functions (`lint_report`, `sprawl_report`, `links`,
  `oldest_report`, `open_questions`, `history`, `list_changesets`,
  `pending_changes`, `discard`, the maintenance tools, search, concept
  reads), and return their result structs as JSON. Key and catalog
  administration calls the `store::keys` and `store::catalogs` functions the
  CLI already uses. The behaviour of an endpoint therefore cannot drift from
  its MCP tool or CLI counterpart.
- **Errors** reuse the existing `http::AppError` (`OkfError` mapped to a
  status, body `{error, message}`): 400 bad input, 401 missing or invalid
  key (middleware), 403 insufficient access, 404 unknown or unreachable
  catalog, path or changeset, 409 refused or conflicting, 422 lint failure,
  503 retryable database abort, 500 internal (logged with `tracing::error!`,
  never echoed in detail).
- **`/ui/*`** serves `index.html`, `styles.css`, vendored Pico CSS and
  `js/**`, all embedded in the binary with `include_bytes!` (`build.rs`
  walks `src/http/ui/` and generates the list, so a new file needs no Rust
  edit), with no key required (they contain no data). `/ui` redirects to
  `/ui/`, and `/` redirects to `/ui/` (it was 404). `GET /ui/manifest.json`
  lists the embedded pages; the client router loads a page on demand and
  hides nav links for pages the build lacks. Client routing uses the URL hash
  (`#/browse/<catalog>/<path>`).
- **New module layout**: `src/http/api/` (one file per area: `identity`,
  `catalogs`, `browse`, `search`, `health`, `changesets`, `maintenance`,
  `keys`, plus `mod.rs` with the router), `src/http/ui.rs` (static asset
  serving and security headers), `src/render.rs` (markdown to safe HTML),
  `src/http/ui/` (the front-end files).

## API

All routes are under `/api/v1` and need a bearer key. "read", "write" and
"admin" are the existing `Access` levels; a catalog the key does not reach
is a 404, as in the MCP tools.

| Area | Method and path | Access |
|---|---|---|
| Identity | `GET /whoami`: key id, name, actor, access, reach, default catalog, maintenance-report flag | any |
| Catalogs | `GET /catalogs`: name, description, file and concept counts, generation, last change | read (reachable catalogs only) |
| | `POST /catalogs` `{name, description}`: 201; an invalid or taken name is 409 | admin that reaches every catalog (plan deviation 9) |
| | `PATCH /catalogs/{c}` `{description}` | admin |
| | `DELETE /catalogs/{c}?force=bool` | admin |
| Import / export | `GET /catalogs/{c}/export`: `.tar.gz` | read |
| | `POST /catalogs/{c}/import?replace=&only_machinery=&expect_generation=`: `.tar.gz` body | admin |
| Browse | `GET /catalogs/{c}/tree`: folders and concepts with titles | read |
| | `GET /catalogs/{c}/concepts/{*path}`: frontmatter, raw body, `body_html`, outline, sources, open questions | read |
| | `GET /catalogs/{c}/links/{*path}`: inbound and outbound | read |
| | `GET /catalogs/{c}/history/{*path}`: revisions | read |
| | `GET /catalogs/{c}/revisions/{n}/{*path}`: text at revision `n` and a unified diff against `n-1` | read |
| | `GET /catalogs/{c}/documents/{*path}`: non-concept documents (`index.md`, `log.md`, `AGENTS.md`), raw and `body_html` | read |
| Search | `GET /search?q=&catalogs=&limit=`: without `catalogs`, every catalog the key reaches | read |
| Health | `GET /catalogs/{c}/health`: counts only (lint errors and warnings, sprawl, orphans, broken links, oldest age, open questions) | read |
| | `GET /catalogs/{c}/reports/{lint,sprawl,links,oldest,questions}`: `?n=` defaults to 20; another name is 404 | read |
| Changesets | `GET /changesets?catalog=`: the key's own; every key's for admin | read |
| | `GET /changesets/{handle}/diff` | owner or admin (plan deviation 12) |
| | `DELETE /changesets/{handle}` | write (owner) or admin |
| Maintenance | `GET /catalogs/{c}/maintenance/runs`, `GET .../runs/{id}` | read |
| | `GET /catalogs/{c}/maintenance/findings?disposition=`, `GET .../findings/{id}` | read |
| | `GET /catalogs/{c}/maintenance/counts`: findings by current disposition, for the nav badge | read |
| | `POST /catalogs/{c}/maintenance/findings/{id}/disposition` `{disposition, rationale, idempotency_key}` | as `maintenance_dispose` (write) |
| Keys | `GET /keys` | admin |
| | `POST /keys` `{name, access, catalogs \| all, default_catalog, actor, maintenance_report}`: returns the secret once | admin |
| | `POST /keys/{id}/revoke` | admin |
| | `PUT /keys/{id}/catalogs/{c}`, `DELETE /keys/{id}/catalogs/{c}`: grant, ungrant | admin |
| | `PATCH /keys/{id}` `{maintenance_report}` | admin |

Plan deviation 6: the per-document browse routes put the sub-resource
before the path (`/links/{*path}`, not `/concepts/{*path}/links`), because an
axum wildcard must be the last segment.

Conventions:

- Every `/api/v1` response carries `Cache-Control: no-store` and
  `X-Content-Type-Options: nosniff`, not only `POST /keys`.
- Lists page with `?limit=&after=` cursors, as the maintenance tools do.
- Dispositions are the existing values: `open`, `accepted_for_follow_up`,
  `dismissed`, `needs_input`.
- The existing archive routes keep their current paths; the `/api/v1`
  import and export routes share their handlers, body limit and upload
  deadline (plan deviation 10: `archives::export_route()` and
  `import_route(app)` are mounted at both). Plan deviation 4: the import
  options `only_machinery` and `expect_generation` are query parameters on
  both paths, beside `replace`. A stale `expect_generation` is 409 with a
  message that says to export again.
- Admin mutations record no audit actor, as with the CLI (`store::keys` and
  the catalog rows record none); the server's log line for a catalog change
  names the key id and its actor.
- Key administration stays within the caller's reach (plan deviation 8). A
  catalog-scoped admin lists and manages only keys whose catalogs it all
  reaches; any other key is 404. A grant or a new key may name only catalogs
  the caller reaches, and `all: true` needs an all-catalogs caller (403
  otherwise). A bad `access` is 400.
- Admin mutations (keys, catalogs) poll at once, so the change holds from the
  next request without waiting for `poll_interval`.
- Findings (plan deviation 11): the store gained a disposition filter, so
  paging counts matching findings only; the MCP tool's schema is unchanged.
- No response other than `POST /keys` ever contains a key secret, and no log
  line ever does.

## Markdown rendering (`src/render.rs`)

- `pulldown-cmark` with tables, strikethrough and task lists.
- Raw HTML in the source is escaped, never passed through.
- Link and image URLs are restricted to `http`, `https`, `mailto` and
  relative links. A relative link that resolves to a concept or document in
  the same catalog is rewritten to the console route
  (`#/browse/<catalog>/<path>`). Any other scheme (`javascript:`, `data:`,
  ...) is rendered as plain text.
- Headings get stable ids matching the outline the API returns, so the
  outline links to sections.
- For bodies longer than `concept_outline_chars`, the concept endpoint
  returns the outline and the body is fetched with `?full=true`, mirroring
  `get_concept`.

Plan deviation 7, as shipped:

- Heading ids are `section-<slug>`, with the slug itself in `data-slug`. A
  bare slug could take a shell element's id (`main`, `app`, `drawer`).
- Table column alignment is dropped: pulldown-cmark writes it as a `style`
  attribute, which the CSP blocks.
- Images become links (`http`/`https`) or alt text: the CSP blocks outside
  images, and the console serves no catalog files.
- The concept endpoint always returns the whole outline. Links come only
  from `/links/{*path}`.
- A link to a missing document is struck through.

## Front end

```
src/http/ui/
  index.html          login card, app shell, drawer, toast host, <template>s
  styles.css          tokens (light/dark), layout, components
  vendor/pico.min.css
  js/main.js          boot: theme, auth check, router start
  js/core/            api.js, router.js, dom.js, store.js, toast.js,
                      drawer.js, format.js
  js/components/      list-table.js, segments.js, tree.js, upload.js,
                      confirm.js
  js/pages/           keys, health, browse, search, catalogs, transfer,
                      findings, runs, changesets
```

- Plan deviation 2: there is no `tabs.js`. Concept tabs use the same
  `segments` component as the report switcher.
- Native ES modules, loaded with `<script type="module">`; no inline script
  or style attributes, so the CSP needs no `unsafe-inline`.
- Data reaches the DOM only via `textContent` and the `h()` element
  builder. The one exception is server-produced `body_html`.
- `api.js` adds the bearer header from `localStorage`, and turns every
  non-2xx into a toast with the server's message. On 401 it clears the
  stored key and shows the login card (this handles revoked keys). On 403 it
  says the key lacks access.
- `store.js` holds the `whoami` result and the selected catalog (remembered
  in `localStorage`). Nav entries and buttons the key cannot use are hidden;
  a `read` key sees no Administer group. The server enforces every rule
  regardless.

### Pages

Sidebar groups: **Health**, **Browse**, **Search**; **Maintenance**:
Findings (badge: open count), Runs, Changesets; **Administer**: Catalogs,
Import & export, API keys. The catalog switcher sits under the brand; the
key's name and access level, the theme toggle and sign-out sit in the footer.

- **Health** (landing): five tiles from `/health`; a segmented control
  switches between the report tables; a row opens its concept in Browse.
  The `oldest` tile is the stalest concept's newest stamp (`generated`,
  `verified` or `reviewed`), which is what `oldest` measures (plan
  deviation 13).
- **Browse**: collapsible tree; concept header with a provenance line
  (generated, reviewed, sources, open questions); tabs Content, Links,
  History, Raw. A revision opens a drawer with its diff. A folder shows its
  `index.md`. Switching catalogs while browsing opens the new catalog's
  root.
- **Search**: query box, ranked hits with snippets across the key's catalogs.
- **Findings**: table filtered by disposition; the drawer shows the claim,
  evidence and pinned revision, with disposition buttons and a rationale
  field (the console makes the idempotency key). The nav badge refreshes
  after sign-in, a catalog switch and a decision.
- **Runs**: table of runs; the drawer lists the run's work items.
- **Changesets**: open changesets with owner, catalog, idle time and file
  count; ones idle longer than `session_idle_timeout` are highlighted; the
  drawer shows the diff and a Discard button behind a confirm.
- **Catalogs**: table; admins create, edit descriptions and delete (delete
  requires typing the catalog name, with an optional force).
- **Import & export**: per catalog, download the export, or drop a `.tar.gz`
  with options replace, only machinery and expected generation (prefilled
  with the current generation). Upload shows progress; the result shows file
  counts or every validation problem.
- **API keys**: table of keys with name, actor, access, catalogs, default,
  reporting flag, created and revoked. Create opens a form; the new secret is
  shown once in a modal with a copy button. Row actions: grant, ungrant,
  toggle reporting, revoke (with a confirm).

### Visual design

Following the pr-review-bot admin UI:

- Fonts: Space Grotesk (headings), Inter (body), JetBrains Mono (paths,
  handles, ids, code), from Google Fonts.
- Pico CSS as the base, overridden by `styles.css` tokens: warm neutral
  background, white surfaces, violet accent (`#6d28d9` light, `#a78bfa`
  dark), soft success, warning and error pills, 10px radius, subtle shadows.
- Theme follows `prefers-color-scheme` until toggled; the choice is stored
  in `localStorage` and applied via `data-theme` on `<html>`.
- Components: sidebar with uppercase group labels and an active pill;
  stat tiles; segmented controls; bordered list tables with uppercase
  column headers; right-side entity drawer with a scrim; toasts.

## Security

- `/api/v1/*` sits behind `require_key`. Only `/healthz` and `/ui/*` assets
  are unauthenticated.
- Headers on `/ui/*`: `Content-Security-Policy: default-src 'self';
  script-src 'self'; style-src 'self' https://fonts.googleapis.com;
  font-src https://fonts.gstatic.com; img-src 'self' data:;
  connect-src 'self'; frame-ancestors 'none'; base-uri 'none';
  form-action 'self'`, plus `X-Content-Type-Options: nosniff` and
  `Referrer-Policy: no-referrer`. API responses also carry `nosniff`.
- The key lives in `localStorage`, so the defence is keeping script
  injection out: no third-party scripts, no inline scripts, escaped
  markdown, scheme-restricted links, and DOM writes through `textContent`.
- No cookies are used, so cross-site requests carry no credentials and
  there is no CSRF surface; DNS rebinding cannot borrow a credential either.
- Every `/api/v1` response, `POST /keys` included, is `Cache-Control:
  no-store`.

## Testing

- **API tests** (`cargo test`): every endpoint tested with
  `tower::ServiceExt::oneshot` against testcontainers Postgres, following
  `src/http/archives.rs`.
- **Access matrix**: every API route is driven with no key, a read key, a
  write key, an admin key and an admin key that does not reach the catalog,
  asserting each status. Plan deviation 1: axum cannot enumerate a router's
  routes, so coverage is a convention rather than generated from the route
  table: each area's test module calls the shared
  `api::testing::assert_matrix` with one row per endpoint, and review checks
  that a new endpoint has a row.
- **Unit tests**: the markdown renderer (raw HTML escaped, link rewriting,
  rejected schemes, heading ids), security headers, and the asset handler.
- **Browser end to end with `rustwright`** (dev-dependency, `tests/ui_e2e/`):
  a harness starts testcontainers Postgres and the real router on an
  ephemeral port, seeds a synthetic catalog (example names only) and keys
  at each access level, and launches headless Chromium (rustwright's own
  discovery: `RUSTWRIGHT_CHROMIUM`, `CHROME`, `CHROMIUM`, the Playwright
  cache, then `PATH`). The synchronous rustwright API runs inside
  `spawn_blocking`. Any browser console error or CSP violation fails the
  test. Journeys: login, bad key, sign-out; theme persistence; key create,
  secret shown once, new key works, revoke returns the browser to login;
  read key sees no Administer group; health tiles match the API and a row
  opens its concept; tree navigation, tabs and a history diff; search;
  import with progress and result, export download; finding disposition;
  changeset discard; and a whole-console journey that visits every page as
  an admin (`tests/ui_e2e/smoke.rs`). Run with `just test-ui` and in the local
  gates (plan deviation 3: the repository's CI builds images only and has no
  test job); skipped with a clear message when Chromium is missing, as the
  Docker-backed tests are without Docker. rustwright 0.3.0 has no page-error
  event, so `main.js` turns script errors, unhandled rejections and CSP
  violations into `console.error`, which fails the journey. Plan deviation
  5: rustwright cannot save a download, so the export journey checks the
  link's URL and fetches it with `reqwest`, and the API test checks the
  bytes.

## Delivery

One epic, one sub-issue and PR per phase. Phases 2 to 7 depend only on
phase 1.

1. **Foundation**: `/api/v1` router and conventions, `GET /whoami`, static
   asset serving with security headers, app shell, login, sign-out, theme
   toggle, `store.js`/`api.js`/`router.js`, the access-matrix harness and
   the rustwright harness with the login journeys.
2. **API keys**: key endpoints and page (first, so per-access-level keys
   for testing every later phase can be made from the console).
3. **Health dashboards**: health and report endpoints, Health page. Report
   rows link to Browse from the start; until phase 4 lands, the router shows
   "Not available yet" there. No raw document endpoint exists under
   `/api/v1`.
4. **Browse and search**: `src/render.rs`, tree, concept, links, history,
   revision diff and search endpoints; Browse and Search pages.
5. **Catalogs and import/export**: catalog endpoints, `/api/v1` mounts of
   the archive handlers, Catalogs and Import & export pages.
6. **Findings and runs**: maintenance endpoints, Findings and Runs pages,
   nav badge.
7. **Changesets**: changeset endpoints, Changesets page.

Each phase updates `README.md` and `docs/operations.md` (new routes,
access rules, `just test-ui`), and adds its changelog entry under
`changelog/`.

## What M9 shipped differently

The plan's deviations are recorded in place above. These are the plan's
dated "Corrected (issue #N)" notes: places where the merged code differs
from the plan as first written. The code is the reference; each item names
the file to read.

**Task 1, foundation (issue #216, PR #224):**

- The API is nested beside the keyed router, not inside it: `api::routes`
  applies `require_key` as a `route_layer`, adds its own 404 fallback, and
  wraps both in the header layer, so a 401 from `/api/v1` carries `no-store`
  and `nosniff` (`src/http/api/mod.rs`).
- Boot calls `enterOrRecover()`: a stored key whose first `/whoami` fails
  with anything but 401 shows the login card with the reason instead of a
  blank page, and keeps the key (`src/http/ui/js/main.js`).
- rustwright 0.3.0's `evaluate` awaits a returned promise, and the harness's
  `assert_no_console_errors` fails on both a `console.error` and a CSP
  violation (checked with a probe journey).
- Task 1 helpers no task had called yet (`api::context`, `require_admin`,
  `Ui::wait_hidden`) carried `#[allow(dead_code)]` until their first caller
  removed it.

**Task 2, API keys (issue #217, PR #226):**

- Key changes use new by-id store functions (`set_maintenance_report_by_id`,
  `revoke_by_id`, `grant_by_id`, `ungrant_by_id`) that lock exactly the row
  named by id and never fall back to a name, so a legacy row (`id == name`)
  cannot reach another live key of that name, such as a replacement created
  meanwhile (`src/store/keys.rs`).
- The key name cell does not wrap (`.key-name`), so a click lands on it at
  the journeys' 800 px viewport.
- Each area's rows go into its own subsection's three-column table in
  `docs/operations.md`, not one shared table.

**Task 3, health (issue #218, PR #225):**

- A row-limit test checks the report default (20) and the clamp (1 to 200)
  on a catalog of 201 concepts.
- Report rows link to Browse from the start (see "Delivery").

**Task 4, browse and search (issue #219, PR #227):**

- The pages follow the per-navigation container convention: `historyTab` and
  `openRevision` take the page's view and touch nothing once it is detached;
  one `#catalog-select` listener serves every render.
- Switching catalogs while browsing opens the new catalog's root once.
- A section route loads the whole body, so an outline link on a long concept
  reaches its heading.
- A link's `#fragment` is decoded once (`section_slug`) before routing; the
  path stays literal, as `resolve_link` and the link checks read it
  (`src/render.rs`).

**Task 5, catalogs and import/export (issue #220, PR #228):**

- Create and delete refresh the catalog switcher even when the user has
  navigated away, and touch the drawer only while the page is attached
  (`catalogs.js`).
- Import & export runs one operation at a time: a busy flag locks the
  catalog picker, Export and Import, and each operation keeps its own
  catalog and file (`transfer.js`).

**Task 6, findings and runs (issue #221, PR #230):**

- The findings filter test seeds three findings and proves the filter
  applies before the limit; the store's `page_ids` picks its queries with
  one match (`src/store/maintenance/runs.rs`).
- `Option<Disposition>` deserializes from the query string (an unknown value
  is 400), and a test backs the docs: a read-level reporter key gets 403
  from the disposition route, and an unknown catalog is 404.
- The drawers check that their page is still attached after each request,
  and reuse `dl.facts`.
- `refreshFindingsBadge` ignores answers older than the newest call, and a
  filter change drops the old cursor at once (`findings.js`), each with a
  delayed-response journey.
- On a machine whose `chromium-browser` is a snap stub that cannot launch,
  leave `RUSTWRIGHT_CHROMIUM` unset: rustwright then finds a Playwright
  Chromium.

**Task 7, changesets (issue #222, PR #229):**

- `show` and `discardOne` take the page's `main` and leave the drawer alone
  once it is detached; the handle cell is an inline block so a click lands
  on it (`changesets.js`).
- The harness's `drive` returns whether the journey ran, so a test that
  checks server state afterwards skips, not fails, without Chromium
  (`tests/ui_e2e/harness.rs`).
