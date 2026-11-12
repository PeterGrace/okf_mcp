# M11 Task 5: The principal's user in whoami and on the consent page

## Added
- `Principal.user` (`Option<String>`): the user the credential belongs to, `None` for one not yet assigned. `From<KeyRecord>` moves `record.user` into it. Display only: `reaches` and `require` never read it, so access is unchanged.
- `GET /api/v1/whoami` returns `user` (the name, `null` when unassigned) and `teams` (that user's teams, sorted, the personal team included; empty without a user). The user comes from the key row read by the request, not the cached principal, so a reassignment since the last poll shows at once.
- `GrantSummary.user`, filled from the sign-in code by `authorize::summary`. The consent summary reads "as <user> (actor <actor>)", both escaped; an unassigned code keeps "as actor <actor>".
- Tests: `whoami_names_the_keys_user_and_its_teams`, `whoami_of_an_unassigned_key_has_no_user`, `a_deleted_user_has_no_teams_not_an_error`, `the_summary_of_an_unassigned_code_names_only_the_actor`.

## Changed
- `the_summary_names_access_catalogs_actor_and_default` expects the user wording; `a_hostile_host_cannot_break_out_of_its_markup` also feeds a hostile user name and checks it is escaped; `a_code_then_approve_creates_a_grant_and_redirects_with_a_code` expects the fixture user on the page.
- The `KeyCache` test fixture record carries `TEST_USER`, and `the_cache_finds_live_keys_only` checks it reaches the principal.
- `docs/operations.md`: the `whoami` row lists `user` and `teams`.

## Notes
- A user deleted between reading the key and reading the user yields an empty `teams` list, not an error (`identity::teams_of` maps `UnknownName` to none).
- `TestApp` has no `pool` field; tests use `test.app.pool()`. The plan's Task 5 is corrected to match.
