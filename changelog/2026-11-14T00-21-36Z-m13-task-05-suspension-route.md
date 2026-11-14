# M13 Task 5: `PUT /api/v1/teams/{name}/suspension` and the team views

Issue #367, epic #362. Plan: `docs/superpowers/plans/2026-11-13-okf-mcp-m13-team-suspension.md`, Task 5.

## Added

- `PUT /api/v1/teams/{name}/suspension` (`src/http/api/people.rs`). Body `{suspended: bool, notice?: string}`, unknown fields refused. `true` suspends the team, or replaces the notice of one already suspended (its first `suspended_at` is kept); `false` lifts it. Answers the team as it now is. Needs an admin key (or admin console session) that reaches every catalog, like every other team write. The handler settles (polls) before answering, so when that poll succeeds the next request is already refused, or served again after a lift. A failed poll is logged and the route still answers with the committed change, which then takes effect at the poll loop's next successful pass (within `poll_interval`), as for every other team write.
- Errors: 403 below an every-catalog admin; 404 for an unknown or malformed team name; 409 `Refused` for a notice over 2000 characters or one holding a NUL; 422 without `suspended` or with an unknown field; 500 only for a database failure.
- `TeamView.suspended_at` (timestamp or null) and `TeamView.suspension_notice` (text or null), in every team answer (`GET /teams`, `GET /teams/{name}`, membership changes, create).
- Tests: `suspension_route_suspends_lifts_and_needs_an_every_catalog_admin` (levels, body errors, notice limits, repeat and lift) and `a_member_of_a_suspended_team_may_export_but_not_browse_or_search` (end to end: suspend through the route; a member's tree, search and named changeset list answer 403 `Suspended` with the notice; export answers 200; the listing marks the catalog; admins are served; lift; the member is served again and the mark is gone).

## Changed

- `store::teams::find`, `lock_team` and `id_of` treat a name that is not `[a-z0-9-]+` as an unknown team before it reaches SQL. A NUL in a team path segment was a Postgres error (500); it is now a 404 on every team route. `a_nul_or_malformed_name_in_a_path_is_a_404_never_a_500` covers the team routes, the suspension route included. This is the team part of #339.
- The plan's Task 5 has a dated correction note, and Task 6's API row lists the route's full error set.

## Notes

- Verified by reverting: without `settle` in the handler, the member test fails (the tree still answers 200 right after the suspension); without the name guards in `store::teams` (all, then `lock_team` alone), the NUL test fails with a 500.
- The REST changeset filtering that the plan's Step 5b gave this task already shipped in Task 4 (`http::api::changesets::tests::the_changeset_list_leaves_out_a_suspended_catalog_but_to_an_admin`); the member test checks it again through the route.
