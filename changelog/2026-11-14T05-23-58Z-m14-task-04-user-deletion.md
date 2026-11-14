# M14 Task 4: deleting a user

Issue #394 (also closes #379), epic #390. Plan:
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 4.

## Added
- Migration `0013_user_deletion.sql`: `api_keys.user_id`,
  `sign_in_codes.user_id` and `team_members.user_id` now cascade when their
  user is deleted (they were `RESTRICT`); `teams.personal_of` stays
  `RESTRICT`.
- `users::DeletedUser { keys, codes, discarded }`, what a deletion removed.
- `users::id_and_admin_of`, the `FOR KEY SHARE` reader Task 6b's provisioner
  guards call (marked `expect(dead_code)` outside tests until then).
- Store tests: `deleting_a_user_removes_its_credentials_and_keeps_the_history`,
  `a_deleted_users_open_changesets_are_discarded_and_audited`,
  `deletion_is_refused_for_owned_catalogs_a_sole_owner_and_the_last_admin`,
  `a_key_created_while_deletion_waits_is_deleted_with_the_user`,
  `a_provisioner_is_refused_an_admin_user`,
  `deleting_an_unknown_or_malformed_user_is_unknown_name`,
  `a_refused_deletion_changes_nothing`,
  `id_and_admin_of_reads_the_flag_and_knows_no_stranger`; REST
  `deleting_a_user_over_rest_takes_its_keys_and_refuses_the_last_admin`; CLI
  `deleting_a_user_takes_its_keys_and_may_delete_the_last_admin`.
- Browser journeys (#379): `a_user_whose_only_key_is_revoked_is_deleted_from_the_console`,
  `a_delete_clicked_again_while_it_runs_sends_one_request`,
  `a_last_owner_or_an_owned_catalog_refuses_the_delete_until_cleared`.

## Changed
- `users::delete(pool, name, allow_last_admin, actor, now)` deletes the user
  with its keys and grants (revoked ones too), sign-in codes, sessions,
  sign-in methods, emailed links, entitlements, memberships and personal
  team, in one transaction. The open changesets of its keys and grants are
  discarded, each with a `catalog.changeset_discarded` entry
  (`reason: user_deleted`); the deletion is recorded as `user.deleted`, and
  `audit::forget_person` runs before the user row goes. Catalogs, their
  history and the audit log stay.
- Refusals are now: its personal team owns a catalog, it is the only owner
  of a shared team, or it is the last admin user (REST and console only;
  the CLI may delete the last admin). Holding credentials or being a member
  of another team no longer refuses.
- `DELETE /api/v1/users/{name}` takes the caller as its audit actor.
- `okf-mcp user delete NAME` prints what went: keys and grants, sign-in
  codes and discarded changesets.
- Console (#379): Delete user is enabled whatever credentials the user has;
  the confirmation says its keys, grants and sign-in codes go with it.
  Only an owned catalog disables it in advance; the server's other refusals
  are kept in the drawer with Delete usable again. Delete is a pending
  write (`user:<name>:delete`), so a reopened drawer cannot send it twice.
- `docs/operations.md`: "Deletion rules", the `user delete` row, the API
  row, and the user-deletion rows of the audit log table.

## Notes
- The deletion locks, in order: the admin rows, the user, its personal
  team, its shared teams, its keys and grants, their open changesets. A
  publish by one of the user's keys already running when the deletion
  starts can deadlock with it (the publish records its entry under the
  user's `FOR KEY SHARE` after taking its changeset); Postgres aborts one of
  the two as `Busy`, which is retryable and leaves nothing half done.
- Closes #297's goal (disabling a user) by deletion.
