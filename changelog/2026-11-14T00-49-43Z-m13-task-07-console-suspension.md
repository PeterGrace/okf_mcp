# M13 Task 7: Console: suspend and lift teams, suspended pills, the Browse notice

Issue #369, epic #362. Plan: `docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md`, Task 7.

## Added

- Teams drawer, **Suspension** section (`src/http/ui/js/pages/teams.js`): an active team offers an optional notice (up to 2000 characters) and Suspend, behind a confirm; a suspended one shows since when, its notice (no paragraph when there is none) and Lift suspension. Both send `PUT /api/v1/teams/{name}/suspension`. The controls need an admin that reaches every catalog, like the other team writes, and are disabled otherwise.
- A "suspended" pill on the team in the Teams list and on each of its catalogs in the Catalogs list; the catalog pill's title is the notice (or names the team).
- Browse (`pages/browse.js`): a catalog whose team is suspended shows `#browse-suspended` with the team, the notice (or "Ask the server operator for details.") and a link to Import & export (`#/transfer/<catalog>`), since export still works. A `.notice` style in `styles.css` (adaptive, from the theme's tokens).
- Journeys in `tests/ui_e2e/teams.rs`: `a_team_is_suspended_with_a_notice_shown_on_its_catalogs_then_lifted` (with the no-notice case and the stored result), `a_member_sees_the_notice_on_browse_with_a_link_to_export` (and exports through it), `a_suspension_write_runs_once_and_settles_in_a_reopened_drawer` and `a_refused_lift_keeps_its_reason_and_a_late_read_never_draws_over_a_newer_one`.

## Changed

- `core/api.js`: a 403 `Suspended` toasts its own message (catalog, team and notice) instead of "Your key/account cannot do this.".
- The Teams drawer follows the console rules in `tools/agentic/lessons.md`: suspension writes are guarded by a pending-write key (`guard`/`once`), a refused write shows its reason in the drawer, a failed re-read draws the last answer marked as possibly out of date, only the newest read draws, and a settled write redraws whichever drawer shows its team now. Membership changes use the same redraw. The list's reads are numbered too.
- The plan's Task 7 has a dated correction note.
- Review of PR #384: the team drawer settles the team and the users one by one (`Promise.allSettled`), so a failed users read after a successful suspend or lift no longer throws the PUT's answer away and redraws the old state; only the failed part is marked out of date. Same class in `pages/keys.js`: a key's or sign-in code's drawer redrawn after a write fell silent when the users read failed (the drawer kept showing the old user); it now falls back to the users it was drawn with (`usersOr`). Journeys `teams::a_written_suspension_shows_when_reading_the_users_again_fails` and `keys::an_assigned_user_shows_when_reading_the_users_again_fails`, both revert-checked. A rule for the class in `tools/agentic/lessons.md`.

## Notes

- Verified by reverting each fix and running its journey: the generic 403 toast (member journey: no "is suspended" toast), the Browse notice (no `#browse-suspended`), the catalog pill (no `catalog-suspended-main`), `once` around the PUT (Suspend not disabled while held), the read counter (the late read drew the team as suspended over the lift) and the stale fallback (no `#team-stale`).
- Editing a suspended team's notice in the console is follow-up #361 and not done here.
