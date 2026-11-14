# M13 Task 6: `okf-mcp team suspend` and `unsuspend`, at parity

Issue #368, epic #362. Plan: `docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md`, Task 6.

## Added

- `okf-mcp team suspend NAME [--notice TEXT]` (`src/cli.rs`, `src/cli/people.rs`): suspends the team through `store::teams::suspend` and prints `suspended team NAME`. Repeating it replaces the notice and keeps the first suspension time; a blank or missing notice stores none.
- `okf-mcp team unsuspend NAME`: lifts the suspension through `store::teams::unsuspend` and prints `lifted the suspension of team NAME`. A team that is not suspended is left unchanged and the command still succeeds, as the REST route does.
- Both exit non-zero naming the team for an unknown or malformed name (the store's `UnknownName`, never a database error), and `suspend` for a notice over 2000 characters or holding a NUL (`Refused`). Like every CLI write, the change reaches a running server at its next poll (within `poll_interval`), since the store bumps the administrative generation.
- Tests: `cli::people::tests::team_suspend_and_unsuspend_print_and_list_the_state` (output, stored notice, the list column while suspended and after the lift, a repeat lift) and `team_suspend_without_a_notice_and_its_refusals` (no notice; unknown and NUL names for both subcommands; a long and a NUL notice leave the suspension unchanged).
- `tests/admin_parity.rs` `COVERED` lists `team suspend` and `team unsuspend`.
- `docs/operations.md`: the two parity rows (both `PUT /api/v1/teams/{name}/suspension`) and the route's API reference row.

## Changed

- `okf-mcp team list` gains a `suspended TIME` cell (`-` when active) after `catalogs`, before the display name. `team_membership_round_trip` checks the new column; `docs/operations.md` documents it.
- The plan's Task 6 has a dated correction note, and Task 8's Step 1 names the two command-table rows it still has to add.

## Notes

- Verified by reverting: without the `suspended` cell in `format_team`, `team_suspend_and_unsuspend_print_and_list_the_state` fails; without the route's API reference row, `every_parity_row_names_a_documented_route_and_a_console_control` fails. Before the change, `admin_parity` failed with "`okf-mcp team suspend` is not a subcommand".
- The "Users and teams" command table in `docs/operations.md` does not yet list the two subcommands: that file's sections other than the parity table, the API row and the team list format belong to Task 8.
