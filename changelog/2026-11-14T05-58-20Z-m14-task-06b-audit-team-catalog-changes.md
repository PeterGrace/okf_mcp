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
- `audit::lock_people` / `People { users, teams, catalogs }`: takes every
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
  `remove_member` (the user) read the user with `users::id_and_admin_of` and
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
- `catalogs::find` (so `delete` and `import`), `set_owner` and
  `update_description` refuse a malformed catalog name as unknown before it
  reaches SQL (part of #339).

## Notes
- The plan's Task 6b has a dated `Corrected (issue #397)` note with every
  divergence: the lock order, the no-change cases, `forced` meaning
  unpublished changes were deleted, and the helpers.
