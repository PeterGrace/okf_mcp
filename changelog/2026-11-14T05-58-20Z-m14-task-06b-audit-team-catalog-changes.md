# M14 Task 6b: recording team and catalog changes

Issue #397, epic #390. Plan:
`docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`, Task 6b.

## Added
- Audit entries, each written in the change's own transaction (a failed
  audit write fails the change): `team.created`, `team.member_added`,
  `team.role_changed`, `team.member_removed`, `team.deleted`,
  `team.suspended`, `team.unsuspended`, `catalog.created`,
  `catalog.owner_changed`, `catalog.description_changed`,
  `catalog.member_access_changed`, `catalog.deleted` and `catalog.imported`.
  A call that changes nothing records nothing; a refusal records nothing.
- `audit::lock_people` / `People { target, users, teams, catalogs }`: takes every
  user row a change's entry will resolve (the actor, named users, a personal
  team's or catalog owner's person) `FOR KEY SHARE` first in its
  transaction, in `users::set_admin`'s order (admins by id, then the rest),
  so users are locked before any team, catalog or `server_state` row.
- `teams::Membership { team, user, role }` and `catalogs::NewCatalog { name,
  description, owner_team }`, the input structs of `set_member` and
  `catalogs::create`; `catalogs::record_catalog` / `CatalogEvent`, the shared
  writer of catalog entries.
- Provisioner guards (they refuse once Task 5 sets
  `AuditActor.provisioner`): `teams::create` (the owner), `set_member` and
  `remove_member` (the user) read the locked target flag from `audit::lock_people` and
  refuse an admin with `Forbidden`.
- Tests: one per action (an entry on a change, none without one or on a
  refusal), `a_failed_audit_write_fails_the_{team,catalog,member_access}_change`
  and `a_failed_audit_write_fails_the_import`,
  `a_provisioner_cannot_make_an_admin_an_owner_or_change_an_admins_membership`,
  `a_provisioners_membership_change_waits_for_a_promotion_in_flight`,
  `a_recorded_change_locks_the_users_it_names_before_the_team`,
  `an_admins_change_to_another_admin_takes_admins_in_set_admins_order`,
  `a_malformed_catalog_name_is_unknown_and_never_reaches_sql`, and two REST
  tests that an entry records the calling key (`source` `api`).
- `docs/operations.md`: the thirteen actions in the "Audit log" table.

## Changed
- New signatures (every caller, tests included, updated):
  `teams::create(pool, new, actor, now)`,
  `teams::set_member(pool, &Membership, actor, now)`,
  `teams::remove_member(pool, team, user, actor, now)`,
  `teams::delete(pool, name, actor, now)`,
  `teams::suspend(pool, name, notice, actor, now)`,
  `teams::unsuspend(pool, name, actor, now)`,
  `catalogs::create(pool, &NewCatalog, actor, now)`,
  `catalogs::set_owner(pool, name, team, actor, now)`,
  `catalogs::update_description(pool, name, description, actor, now)`,
  `catalogs::delete(pool, name, force, actor, now)`,
  `ceiling::set_member_access(pool, catalog, access, actor, now)`, and
  `ImportOptions.audit`. The CLI passes `AuditActor::cli()`; the REST team,
  suspension, catalog and import handlers take the `AuditActor` extractor.
- Suspending a team already suspended with the same notice, giving a catalog
  to its current owner, and setting a description to its current text no
  longer write or bump the administrative generation.
- Task 6d's `audit::lock_actor` is folded into `lock_people` and deleted:
  `settings::set` and `reset` call `lock_people` (the actor and its
  credential's owner, now admins first rather than in plain id order), and
  `tools/agentic/lessons.md` has one lock-order rule.
- `catalogs::find` (so `delete` and `import`), `set_owner` and
  `update_description` refuse a malformed catalog name as unknown before it
  reaches SQL (part of #339).

## Notes
- Recovery validation on 2026-11-14: `gate.sh fmt` and `lint` passed;
  `gate.sh test` passed (1949 passed, 0 failed, 17 ignored; library 1689
  passed, 2 ignored). Dedicated `test-ui` passed all 109 browser journeys.
  Its first run had one toast-wait timeout; the isolated retry and then
  the complete browser rerun passed without code changes.
- The held-row reciprocal suspension regression passed alone (1 test) and
  in the full suite. Restoring the old upsert resolver made it fail on its
  bounded audit-row wait (1 failed); the production resolver was restored
  before final validation. Logs are local under `target/i397-logs/recovery-*`.
- Recovery on 2026-11-14 integrates Task 6a without discarding its user,
  invitation, entitlement or session audit behavior. `audit::RowLock` and
  `People::target` unify both lock helpers and preserve each target lock mode.
- Review comment 4571925955 is addressed using Task 6a migration 0014: existing
  reference reads do not update audit rows, and stale refreshes skip locked
  rows. No redundant audit-row locking is added. The reciprocal suspension
  regression holds a stale person row while both real team writes finish.
- Catalog ownership can change after the initial person snapshot; that race
  can still require a retry. The helper does not guarantee absence of every
  possible deadlock.
- The plan's Task 6b has a dated `Corrected (issue #397)` note with every
  divergence: the lock order, the no-change cases, `forced` meaning
  unpublished changes were deleted, and the helpers.
