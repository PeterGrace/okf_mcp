# M11 Task 4: Keys, grants and sign-in codes belong to users

## Added
- `NewKey.user` and `NewSignInCode.user` (`&str`, required): the user a new key or code belongs to. An unknown user is `UnknownName` (404) and creates nothing.
- `KeyRecord.user` and `SignInCodeRecord.user` (`Option<String>`): the user's name, `None` only for a row from before migration 0009 not yet assigned. `keys::list` and the code listings read it through a `LEFT JOIN users`.
- Redeeming a sign-in code copies its `user_id` to the new grant; a legacy code's grant stays unassigned.
- `store::keys::assign_user_by_id(pool, id, user)` (exact id, any kind, revoked included; returns the row's name; an unknown id is `NotFound { path: "keys/<id>" }`) and `assign_user_target(pool, target, actor, user)` (id or unambiguous live name, grants included; returns the id). Both bump the administrative generation.
- `store::oauth::codes::assign_user(pool, id, user)`: any state; an unknown id is `NotFound { path: "oauth/codes/<id>" }`. No generation bump (codes are not in the key cache).
- `store::unassigned::list` and `Unassigned { keys, codes, catalogs }` with `is_empty()`.
- `okf-mcp key create --user` and `okf-mcp oauth code create --user` (required), and a required `user` in the `POST /api/v1/keys` and `POST /api/v1/oauth/codes` bodies (missing: 422). `KeyView` and `SignInCodeView` show `user`.
- The console's New key and New sign-in code forms have a required User text field (a stop-gap until Task 8's picker); the `ui_e2e` key and code journeys fill it.
- Tests: `a_key_belongs_to_its_user`, `a_key_for_an_unknown_user_is_unknown_name`, `assigning_a_legacy_key_sets_its_user_and_bumps_the_generation`, `assigning_reassigns_a_revoked_key_and_refuses_unknown_names`, `assign_by_name_reaches_grants_and_a_shared_name_is_ambiguous`, `a_create_that_waited_on_a_user_delete_is_unknown_name`, `an_assign_that_waited_on_a_user_delete_is_unknown_name`, `a_code_belongs_to_its_user_and_an_unknown_user_is_unknown_name`, `a_redeemed_grant_inherits_its_codes_user`, `a_legacy_codes_grant_stays_unassigned`, `a_blank_actor_defaults_to_the_codes_name`, `assign_user_sets_a_codes_user_without_bumping_the_generation`, `assign_user_refuses_an_unknown_code_or_user`, `a_code_create_or_assign_that_waited_on_a_user_delete_is_unknown_name`, `unassigned_lists_exactly_the_rows_with_no_user_or_owner`, `a_fresh_database_has_nothing_unassigned`, plus REST checks for an unknown and a missing `user`.

## Changed
- `users::id_of` reads the user `FOR KEY SHARE`, as `teams::id_of` does. Every create and assign path resolves the user inside its own transaction with it.
- `teams::set_member` and `remove_member` resolve the user before locking the team, so every path takes a user before a team, as `users::delete` does (user, then its personal team). Without this, the new `FOR KEY SHARE` made a membership change on a user's personal team deadlock with that user's deletion. Regression test: `a_membership_change_racing_a_user_delete_waits_instead_of_deadlocking`, which fails under the old order.
- A sign-in code's blank actor defaults to the code's name instead of being refused (plan Decision 6). The REST and CLI fields stay as they were until Tasks 6 and 7 make them optional.
- The key resolver behind `okf-mcp key ...` takes `include_grants`; only `assign_user_target` sets it (`Selector::ResolveAnyKind`).
- `catalogs::tests::until_locks_are_awaited` is `pub(crate)` for the code race test.
- CLI tests that create keys start from `test_db()`, which has the fixture user.

## Notes
- Without `FOR KEY SHARE` in `users::id_of`, a key create or assign racing a user delete fails its foreign key (a 500). With it, the create waits and reports the user as unknown. Both key race tests fail without the lock.
- The superseded plan test `a_user_with_a_key_is_not_deleted` is not added: `users::tests::a_user_named_by_a_key_is_not_deleted` and its sign-in-code twin already cover it.
- `docs/operations.md` and `README.md` examples gain `--user` in Task 9, as `--owner` does.
