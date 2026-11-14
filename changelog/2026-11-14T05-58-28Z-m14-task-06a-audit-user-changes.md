# M14 Task 6a: recording user changes

Issue #396, epic #390. Plan:
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 6a.

## Added
- Audit entries, each written in the change's own transaction (a failed
  write fails the change), and none when nothing changed:
  `user.created`, `user.email_changed` (`{"cleared"}`, never the address),
  `user.admin_changed` (`{"from", "to"}`), `user.invited`,
  `user.entitlement_set` (`{"catalog", "access", "previous"}`, in the
  catalog's owner team's log), `user.entitlement_removed` (`{"catalog"}`,
  likewise) and `user.sessions_revoked` (`{"sessions": n}`).
- `users::AdminChange { on, allow_last }` and `ceiling::NewEntitlement
  { user, catalog, access }`, the input structs the new signatures take.
- `users::end_session_by_prefix(pool, name, id_prefix, actor, now)`: the
  admin route's one-session revoke, under the user's row lock (user, then
  sessions) and recorded in the same transaction. It shares
  `user_sessions::revoke_by_prefix_in(conn, ..)` with the self-service
  `revoke_by_prefix`, which records nothing.
- `users::lock_users` / `lock_target` (with `UserLocks` and `RowLock`): a
  change locks its target, its actor's user and credential owner, and a
  catalog's personal owner (the rows `audit::record` locks `FOR KEY SHARE`)
  first, in `set_admin`'s order (admin users, then the rest, each by id),
  before `server_state`. Without it, a change recorded as an admin that
  bumped the generation and then waited on the admin's row deadlocked with
  a concurrent `set_admin` (tests
  `an_admins_change_and_a_concurrent_set_admin_both_finish` and
  `an_admins_entitlement_change_and_a_concurrent_set_admin_both_finish`).
- `users::user_entry(action, user_id, details)`, the entry every
  user-targeted record builds (`user.deleted` now uses it too).
- Provisioner guards: `users::set_email` and `invite::send_setup` refuse an
  admin target (`Forbidden`) under a lock `set_admin` conflicts with,
  before anything else, so an invite never tells a provisioner whether an
  admin has an email.
- Tests: one per action (an entry on a change, none without one), the
  forced audit failure per module (`a_failed_audit_write_fails_the_change`,
  `a_failed_audit_write_fails_the_entitlement_change`,
  `a_failed_audit_write_fails_the_invite`), the provisioner refusals
  (`a_provisioner_cannot_set_an_admins_email`,
  `a_provisioner_cannot_invite_an_admin_with_or_without_an_email`), the
  lock order (`ending_one_session_waits_for_a_sign_in_holding_the_user`) and
  the REST wiring (`the_user_routes_record_their_changes_as_the_api_caller`).
- `docs/operations.md`: seven rows in the "Audit log" table.

## Changed
- `users::create`, `set_email`, `set_admin`, `end_sessions`,
  `invite::send_setup`, `ceiling::entitle` and `unentitle` take
  `actor: &AuditActor` and `now`. The CLI passes `AuditActor::cli()`, the
  REST handlers the request's `AuditActor`, every test `AuditActor::cli()`.
- `users::set_email` locks the user row first and then updates it by id.
- `invite::send_setup` locks the user `FOR NO KEY UPDATE` before issuing
  the link (the lock `issue_for_user_in` takes anyway).
- `DELETE /api/v1/users/{name}/sessions/{id_prefix}` calls
  `users::end_session_by_prefix`.

## Notes
- The fixture user of every test database is created through
  `users::create`, so each starts with one `user.created` entry; the
  `store::audit` tests that read the whole log now filter by action.
- `entitle` reads the previous level from its upsert (`RETURNING xmax = 0`)
  instead of a separate read, which a concurrent entitle could race.
- Plan corrected with a dated note under Task 6a, and Task 8a's export test
  counts the fixture's entry.
