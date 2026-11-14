# okf-mcp team suspension: design

Status: delivered in milestone 13 (epic #362; sub-issues #363-#370; PRs
#374, #376-#378, #380, #382, #384 and #387), 2026-11-14 (plan
`docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md`). What
differs from this design is under "What shipped differently" at the end;
the operator's view is `docs/operations.md`, "Suspending a team".

## Goal

An operator can **suspend a team**: every catalog the team owns stops
answering its members at once, through MCP, the REST API and the console,
until the operator lifts the suspension. Nothing is deleted or changed, and
members can still sign in and **export** the team's catalogs while it is
suspended.

It's a general operator control. Examples: a team that broke the server's
terms of use, a team whose access is under review, or a hosted operator
pausing a team at the end of an agreement. okf-mcp doesn't know or record
why; the operator can attach a short **notice** that members see.

## Non-goals

- Deleting anything. Suspension and deletion are separate; deletion stays
  `catalog delete`, `team delete` and `user delete`.
- Suspending a single user or a single catalog. Users keep working in their
  other teams' catalogs; a catalog is suspended through its owner team.
- Scheduling (suspend at a future time) or automatic suspension.
- Multiple replicas (the server is single-replica, as before).

## Decisions

| Question | Decision |
|---|---|
| What is suspended | A team. Its effect is on the catalogs it owns (`catalogs.owner_team_id`). A personal team can be suspended like a shared one. |
| Stored as | `teams.suspended_at` (null when active) and `teams.suspension_notice` (optional text, at most 2000 characters). Migration `0011`. |
| Who may suspend | An admin key that reaches every catalog, or an admin console session: the same rule as the other team writes. |
| Who is affected | Every non-admin credential: API keys, OAuth grants and console sessions, whoever issued them. An `admin` credential is not affected, so the operator can still inspect and export. |
| What is refused | Every read and write of a suspended team's catalog: MCP tools (selection, changesets, publish), `/api/v1` browse, search, history, health and reports, maintenance tools. |
| What stays open | **Export** (`GET /catalogs/{name}/export` and `GET /api/v1/catalogs/{name}/export`) for any credential that reaches the catalog. Console sign-in. Listing: `list_catalogs` and `GET /api/v1/catalogs` still show the catalog, marked suspended (`GET /api/v1/me`'s ceiling is unchanged). |
| The error | A new `Suspended` error kind: HTTP **403**, `{"error": "Suspended", "message", "catalog", "team", "notice"}`. The message names the catalog and team and ends with the notice. It is only given for a catalog the credential reaches, so it reveals nothing new. |
| Order of checks | Not reached: `CatalogNotSelected` (404) as today. Reached but level too low: `Forbidden`. Otherwise suspended: `Suspended`. |
| Multi-catalog calls | A call that names catalogs (argument, or the key's default) and includes a suspended one is refused with `Suspended`. A call that falls back to the connection's list (`X-OKF-Catalogs`) drops the suspended ones and reads the rest, so one suspended team doesn't break a connection that also reads other teams' catalogs; only when every listed catalog is suspended is it refused. The REST search over "every catalog the key reaches" leaves suspended ones out. |
| How fast | Suspending and lifting bump `admin_generation`, and the route polls before answering, as the other team writes do: when that poll succeeds, the next request sees it. If the poll or the key-cache reload fails, the change is still committed and the route still answers success; the poll loop retries, so it takes effect within `poll_interval` of the database recovering (`settle` and `reload_keys_if_changed` log rather than return failures). |
| Open changesets | Kept. Writing to them and publishing them are refused while suspended; lifting the suspension makes them usable again. |
| Idempotent | Suspending a suspended team keeps its original `suspended_at` and replaces the notice. Lifting a suspension that isn't there changes nothing and doesn't bump the generation. |

## Surfaces

| CLI | REST | Console |
|---|---|---|
| `okf-mcp team suspend NAME [--notice TEXT]` | `PUT /api/v1/teams/{name}/suspension` with `{"suspended": true, "notice": "..."}` | Teams, then a team, then Suspension: Suspend |
| `okf-mcp team unsuspend NAME` | the same route with `{"suspended": false}` | Teams, then a team, then Suspension: Lift suspension |

- The team list and detail (`team list`, `GET /api/v1/teams`,
  `GET /api/v1/teams/{name}`) show `suspended_at` and `suspension_notice`.
- `list_catalogs` (MCP) and `GET /api/v1/catalogs` give each catalog a
  `suspended` field: null, or `{"team", "notice"}`.
- The console shows a "suspended" pill on the team and on its catalogs,
  and a notice on Browse with a link to Import & export. A 403 whose error
  is `Suspended` shows its message rather than the generic "cannot do
  this".

## Data model

```sql
ALTER TABLE teams
    ADD COLUMN suspended_at timestamptz,
    ADD COLUMN suspension_notice text
        CHECK (char_length(suspension_notice) <= 2000),
    ADD CONSTRAINT teams_notice_needs_suspension
        CHECK (suspension_notice IS NULL OR suspended_at IS NOT NULL);
```

The key cache loads one more map, catalog name to `{team, notice}`, for
catalogs whose owner team is suspended, and every principal it builds
shares it (`Arc`).

## Testing

- Store: suspend, re-suspend, lift, lift-when-active, notice length,
  unknown team, the generation bump.
- Principal: `require` refuses a suspended catalog with `Suspended` after
  the reach and level checks; admin is exempt; `require_export` ignores
  suspension.
- Selection: argument, default and connection-list cases.
- REST: the suspension route's access matrix and bodies; a member's
  browse and search refused, export allowed; listing marks the catalog.
- MCP: a member's `search` and `open_changeset` refused; `list_catalogs`
  marks it; another team's catalog on the same connection still answers.
- CLI parity table rows; console journey: suspend, see the pill, lift.

## What shipped differently

Read the plan's "Decisions taken while planning" and its dated `Corrected
(issue #N)` and `Qualified` notes for the detail; the per-task changelogs
(`changelog/*-m13-task-*`) give the reasons. The design above held; these
are the places where what shipped is narrower, wider or more exact.

### Behaviour

- **`Forbidden` or `Suspended` for a key below the needed level.**
  `Principal::require` checks reach, then level, then suspension, as
  "Order of checks" says. Tools that pick their catalog through the
  selection before checking their own level (`open_changeset`,
  `maintenance_dispose` and the maintenance work tools) refuse a suspended
  catalog at the selection, so a key that reaches it below the level the
  tool needs gets 403 `Suspended` rather than 403 `Forbidden`. Reach is
  still checked first, so a credential that doesn't reach the catalog never
  learns it is suspended (#366).
- **A connection of only suspended catalogs is refused everywhere.**
  `ToolContext::check_connection` goes through the selection, so
  `list_changesets` without `catalog`, and a changeset handle on such a
  connection, are refused with `Suspended` too; a mixed list passes (#366).
- **Search over every reached catalog, all suspended.** `GET /api/v1/search`
  without `catalogs` leaves suspended catalogs out; when that leaves
  nothing, it falls back to the key's default, as for a key that reaches
  nothing, and that default is then refused as named (403 `Suspended`)
  (#366).
- **A new catalog is marked at once.** `POST /api/v1/catalogs`' answer reads
  `suspended` from the key cache after its poll, so a catalog created for a
  suspended team is marked (null if that poll failed, until the next)
  (#366). More generally the suspension follows ownership: a catalog moved
  to or from a suspended team changes with it.
- **The notice is cleaned.** A blank notice (after trimming) is no notice,
  so re-suspending without one clears the old one; a notice holding a NUL is
  refused (409 `Refused`) like one over 2000 characters, since Postgres text
  can't store it (#363).
- **Malformed team names are unknown.** `store::teams::find`, `lock_team`
  and `id_of` refuse a name that is not `[a-z0-9-]+` with `UnknownName`
  before SQL, so a NUL in a team path is a 404, never a 500, on every team
  route, the suspension route and the CLI included (the team part of #339;
  #367).
- **The route's errors.** `PUT /api/v1/teams/{name}/suspension` takes
  `{suspended, notice?}` with unknown fields refused: 422 without
  `suspended` or with an unknown field, 409 for a bad notice, 404 for an
  unknown team (#367).

### Surfaces

- **`okf-mcp team list`** gained a `suspended TIME` cell (`-` when active)
  after `catalogs`, before the display name, so the display name moved from
  field 5 to field 6. `team suspend` prints `suspended team NAME`, `team
  unsuspend` prints `lifted the suspension of team NAME` (#368).
- **Console.** The "suspended" pills use the `error` kind (the console's
  `pill` has no `danger`). The team drawer's Suspension section follows the
  console rules in `tools/agentic/lessons.md`: its writes are guarded by a
  pending-write key, a refused write keeps its reason in the drawer, only
  the newest read draws, a settled write redraws the drawer that shows its
  team now, and a failed users read never discards a write's answer. The
  Browse notice uses a `.notice` style with `role="status"`. The console
  can suspend and lift but not replace a suspended team's notice; the CLI
  and REST can (#361, follow-up; #369).

### Internals

- `suspended_message` takes `Option<&str>`; the MCP tool-error mapping
  needed no change, since it builds its body from `to_json` (#364).
- `CatalogSummary::of(name, state, principal)` takes the caller and fills
  `suspended` itself, through `From<&SuspendedCatalog> for SuspensionInfo`
  (#366).
- Shared test helpers: `mcp::testing::{suspend_catalogs, lift_suspension,
  stored_secret, cached_principal}` and `http::api::testing::{Expect,
  assert_answers}` (#365).

### Not built here

- A narrower "provisioner" admin scope for automation: milestone 14's
  design (PR #375), not a separate issue.
- Follow-ups filed during the milestone: #361 (edit a suspended team's
  notice in the console), #381 (the team drawer's membership and delete
  controls are not guarded by a pending-write key), #385 (the users and My
  keys drawers number their redraw after the list refresh).
