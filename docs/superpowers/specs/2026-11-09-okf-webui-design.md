# okf-mcp web console: design

Status: approved in brainstorming, 2026-11-09. Next step: implementation plan.

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

- **`/api/v1/*`** is mounted inside the existing keyed router, so it passes
  through the existing `require_key` middleware and gets a `Principal` like
  every other keyed route. Handlers are thin adapters: they build the same
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
  `js/**`, all embedded in the binary with `include_str!`/`include_bytes!`,
  with no key required (they contain no data). `/ui` redirects to `/ui/`.
  Client routing uses the URL hash (`#/browse/<catalog>/<path>`).
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
| Catalogs | `GET /catalogs`: name, description, concept count, generation | read (reachable catalogs only) |
| | `POST /catalogs` `{name, description}` | admin |
| | `PATCH /catalogs/{c}` `{description}` | admin |
| | `DELETE /catalogs/{c}?force=bool` | admin |
| Import / export | `GET /catalogs/{c}/export`: `.tar.gz` | read |
| | `POST /catalogs/{c}/import?replace=&only_machinery=&expect_generation=`: `.tar.gz` body | admin |
| Browse | `GET /catalogs/{c}/tree`: folders and concepts with titles | read |
| | `GET /catalogs/{c}/concepts/{*path}`: frontmatter, raw body, `body_html`, outline, sources, open questions | read |
| | `GET /catalogs/{c}/concepts/{*path}/links`: inbound and outbound | read |
| | `GET /catalogs/{c}/concepts/{*path}/history`: revisions | read |
| | `GET /catalogs/{c}/concepts/{*path}/revisions/{n}`: text at revision `n` and a unified diff against `n-1` | read |
| | `GET /catalogs/{c}/documents/{*path}`: non-concept documents (`index.md`, `log.md`, `AGENTS.md`), raw and `body_html` | read |
| Search | `GET /search?q=&catalogs=&limit=` | read |
| Health | `GET /catalogs/{c}/health`: counts only (lint errors and warnings, sprawl, orphans, broken links, oldest age, open questions) | read |
| | `GET /catalogs/{c}/reports/{lint,sprawl,links,oldest,questions}` | read |
| Changesets | `GET /changesets?catalog=`: the key's own; every key's for admin | read |
| | `GET /changesets/{handle}/diff` | read |
| | `DELETE /changesets/{handle}` | write (owner) or admin |
| Maintenance | `GET /catalogs/{c}/maintenance/runs`, `GET .../runs/{id}` | read |
| | `GET /catalogs/{c}/maintenance/findings?disposition=`, `GET .../findings/{id}` | read |
| | `POST /catalogs/{c}/maintenance/findings/{id}/disposition` `{disposition, note}` | as `maintenance_dispose` |
| Keys | `GET /keys` | admin |
| | `POST /keys` `{name, access, catalogs \| all, default, actor, maintenance_report}`: returns the secret once, `Cache-Control: no-store` | admin |
| | `POST /keys/{id}/revoke` | admin |
| | `PUT /keys/{id}/catalogs/{c}`, `DELETE /keys/{id}/catalogs/{c}`: grant, ungrant | admin |
| | `PATCH /keys/{id}` `{maintenance_report}` | admin |

Conventions:

- Lists page with `?limit=&after=` cursors, as the maintenance tools do.
- Dispositions are the existing values: `open`, `accepted_for_follow_up`,
  `dismissed`, `needs_input`.
- The existing archive routes keep their current paths; the `/api/v1`
  import and export routes share their handlers, body limit and upload
  deadline.
- Admin mutations are attributed to the key's actor, as CLI changes are.
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

## Front end

```
src/http/ui/
  index.html          login card, app shell, drawer, toast host, <template>s
  styles.css          tokens (light/dark), layout, components
  vendor/pico.min.css
  js/main.js          boot: theme, auth check, router start
  js/core/            api.js, router.js, dom.js, store.js, toast.js,
                      drawer.js, format.js
  js/components/      list-table.js, segments.js, tabs.js, tree.js,
                      upload.js, confirm.js
  js/pages/           keys, health, browse, search, catalogs, transfer,
                      findings, runs, changesets
```

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
- **Browse**: collapsible tree; concept header with a provenance line
  (generated, reviewed, sources, open questions); tabs Content, Links,
  History, Raw. A revision opens a drawer with its diff. A folder shows its
  `index.md`.
- **Search**: query box, ranked hits with snippets across the key's catalogs.
- **Findings**: table filtered by disposition; the drawer shows the claim,
  evidence and pinned revision, with disposition buttons and a note field.
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
- `POST /keys` responses are `Cache-Control: no-store`.

## Testing

- **API tests** (`cargo test`): every endpoint tested with
  `tower::ServiceExt::oneshot` against testcontainers Postgres, following
  `src/http/archives.rs`.
- **Access matrix**: one test drives every route in the API route table
  with no key, a read key, a write key, an admin key and a key that does
  not reach the catalog, asserting each status. The route table is the
  test's input, so an added route without an expected-status row fails.
- **Unit tests**: the markdown renderer (raw HTML escaped, link rewriting,
  rejected schemes, heading ids), security headers, and the asset handler.
- **Browser end to end with `rustwright`** (dev-dependency, `tests/ui_e2e/`):
  a harness starts testcontainers Postgres and the real router on an
  ephemeral port, seeds a synthetic catalog (example names only) and keys
  at each access level, and launches headless Chromium (found through
  `RUSTWRIGHT_CHROMIUM`). The synchronous rustwright API runs inside
  `spawn_blocking`. Any browser console error or CSP violation fails the
  test. Journeys: login, bad key, sign-out; theme persistence; key create,
  secret shown once, new key works, revoke returns the browser to login;
  read key sees no Administer group; health tiles match the API and a row
  opens its concept; tree navigation, tabs and a history diff; search;
  import with progress and result, export download; finding disposition;
  changeset discard. Run with `just test-ui` and in CI; skipped with a clear
  message when Chromium is missing, as the Docker-backed tests are without
  Docker.

## Delivery

One epic, one sub-issue and PR per phase. Phases 2 to 7 depend only on
phase 1.

1. **Foundation**: `/api/v1` router and conventions, `GET /whoami`, static
   asset serving with security headers, app shell, login, sign-out, theme
   toggle, `store.js`/`api.js`/`router.js`, the access-matrix harness and
   the rustwright harness with the login journeys.
2. **API keys**: key endpoints and page (first, so per-access-level keys
   for testing every later phase can be made from the console).
3. **Health dashboards**: health and report endpoints, Health page. Until
   phase 4 lands, report rows link to the raw document endpoint.
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
