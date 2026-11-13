# okf-mcp team suspension: design

Status: approved, 2026-11-13. Delivered by milestone 13 (plan
`docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md`).

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
| How fast | Suspending and lifting bump `admin_generation`, and the route polls before answering, as the other team writes do: the next request sees it. |
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
