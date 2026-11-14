# Milestone 13: team suspension (epic #362)

An operator can suspend a team. Every catalog the team owns then refuses
every non-admin credential (API keys, OAuth grants and console sessions)
through MCP, the REST API and the console, with a 403 `Suspended` naming the
catalog, the team and an optional notice, until the suspension is lifted.
Export, console sign-in and listing stay open, admins are exempt, and
nothing is deleted. CLI, REST and console are at parity. Design:
`docs/superpowers/specs/2026-11-13-okf-team-suspension-design.md`; plan:
`docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md` (PR #360).

## Added

- **Task 1, migration and team store (#363, PR #374):** migration
  `0011_team_suspension.sql` (`teams.suspended_at`,
  `teams.suspension_notice`, at most 2000 characters, only with a
  suspension); `store::teams::{suspend, unsuspend, suspended_catalogs}`,
  `Suspension`, `SuspendedCatalog` and `TeamRecord.suspension`. Both writes
  lock the team row and bump `admin_generation`; lifting an active team
  changes nothing.
- **Task 2, the `Suspended` error kind (#364, PR #376):**
  `OkfError::Suspended { catalog, team, notice }`, 403, with `catalog`,
  `team` and `notice` in its JSON body.
- **Task 3, enforcement (#365, PR #377):** the key cache loads the
  suspended-catalogs map and every principal it builds, console sessions
  included, shares it. `Principal::require` checks reach, then level, then
  suspension; admins are exempt; `Principal::require_export` keeps export
  open.
- **Task 4, selection and listing (#366, PR #378):** `selection::resolve`
  refuses a named suspended catalog and drops suspended ones from a
  connection's list; the REST search and `list_changesets` leave them out;
  `list_catalogs` and `GET /api/v1/catalogs` mark them with `suspended`
  instead of hiding them.
- **Task 5, the REST route (#367, PR #380):**
  `PUT /api/v1/teams/{name}/suspension` (`{suspended, notice?}`), which
  settles before answering; `GET /api/v1/teams` shows `suspended_at` and
  `suspension_notice`.
- **Task 6, CLI and parity (#368, PR #382):** `okf-mcp team suspend NAME
  [--notice TEXT]` and `okf-mcp team unsuspend NAME`, the `suspended` cell
  of `team list`, and their parity and API reference rows.
- **Task 7, console (#369, PR #384):** the team drawer's Suspension section
  (Suspend with a notice, Lift suspension), "suspended" pills on teams and
  catalogs, the Browse notice with a link to Import & export, and the 403's
  own message instead of "cannot do this".
- **Task 8, documentation (#370):** "Suspending a team" in
  `docs/operations.md`, the command table rows, the API reference fields,
  the Troubleshooting and Upgrades entries, the README and the spec's "What
  shipped differently".

## Changed

- `okf-mcp team list` has a `suspended TIME` cell (`-` when active) before
  the display name, which moved from field 5 to field 6.
- A key that reaches a suspended catalog below the level a tool needs gets
  `Suspended`, not `Forbidden`, from tools that select their catalog before
  checking their level (`open_changeset`, the maintenance tools).
- A team name that is not `[a-z0-9-]+` is an unknown team (404) on every team
  route and command before it reaches SQL, so a NUL is never a 500 (the team
  part of #339).
- The console's Teams drawer numbers its reads, settles the team and the
  users separately, and guards its suspension writes by a pending-write key;
  the key and sign-in code drawers in `pages/keys.js` got the same fixes.

## Review findings worth knowing

Each became a rule in `tools/agentic/lessons.md`:

- **A test that a JSON field is `null` checks that the key is there.**
  `body["notice"] == Null` passed with the field removed, since serde_json's
  indexing answers `Null` for a missing key (#364).
- **A failed side read never discards a write's answer.** The team drawer
  redrew from the PUT's answer and the users list with `Promise.all`, so a
  failed users read threw away a committed suspension and showed the team as
  active (PR #384).
- **Take a redraw's number as the write settles, and scope it to the drawer
  it updates.** Numbered after the list refresh, an older write whose list
  read answered late drew over a newer one; a page-wide number bumped by a
  write of one team cancelled the opening drawer of another (PR #384).

## Notes

- Migration `0011` only adds: every team stays active until an admin
  suspends one. `docs/operations.md`, "Upgrades", has the line.
- A suspension takes effect on the next request when the route's poll
  succeeds; otherwise at the poll loop's next successful pass, within
  `poll_interval`. The command line writes the database only, so a running
  server picks it up within `poll_interval`.
- What differs from the design is in the spec's "What shipped differently".
- Follow-ups:
  - #361 edit a suspended team's notice in the console without lifting it;
  - #381 the team drawer's membership and delete controls are not guarded
    by a pending-write key;
  - #385 the users and My keys drawers number their redraw after the list
    refresh;
  - the narrower "provisioner" admin scope is milestone 14's (PR #375).
