# M11 Task 3: Catalog ownership

## Added
- `Catalog.owner_team: Option<String>`: the owning team's name, `None` only for a catalog from before migration 0009 not yet assigned. `list` and `find` read it through one shared `select_catalogs!()` query.
- `store::catalogs::set_owner(pool, name, team)`: moves a catalog to another team or assigns a legacy one, bumps the administrative generation, and returns the previous owner. It is one `UPDATE ... FROM (SELECT ... FOR UPDATE)`, like `update_description`, so two concurrent changes each report the owner they replaced.
- `store::teams::id_of`, which Task 2 left out. It reads the team `FOR KEY SHARE`.
- `CatalogState::owner_team()` / `set_owner_team()`: the loaded state carries the owner outside the snapshot, as it does the description. The poll copies it from each row, so an owner change shows without a rebuild.
- `okf-mcp catalog create --owner <team>` (required) and `POST /api/v1/catalogs` `owner` (required: a body without it is a 422; an unknown team is a 404). `GET /api/v1/catalogs` and the create answer include `owner_team`.
- The console's create-catalog form has a required Owner text field, so it keeps working until Task 8 replaces it with a picker.
- `tests/support/server.rs`: `OWNER` (`tester`) and `migrate_with_owner`, used by the server, round-trip and smoke tests.
- Tests: `a_new_catalog_is_owned_by_the_named_team`, `an_unknown_owner_is_unknown_name_and_creates_nothing`, `set_owner_moves_a_catalog_and_returns_the_previous_owner`, `set_owner_assigns_a_legacy_catalog`, `set_owner_refuses_an_unknown_catalog_or_team_and_changes_nothing`, `a_create_that_waited_on_a_team_delete_is_unknown_name`, `poll_picks_up_a_new_owner_without_a_rebuild`, plus owner assertions in the catalog-state load test, the REST list and create tests, and the CLI parse test.

## Changed
- `store::catalogs::create` takes `owner_team: &str` before `now`; an unknown team is `UnknownName` and creates nothing. Every caller passes an owner: `crate::db::TEST_TEAM` in unit tests, `support::server::OWNER` in integration tests.
- `users::tests::seed_catalog_owned_by` creates the catalog with its owner instead of writing `owner_team_id` with SQL. The race test creates its catalog owned by `TEST_TEAM` and keeps its explicit `UPDATE` inside the held transaction.
- CLI tests that run `catalog create` start from `test_db()`, which already has the fixture user.

## Notes
- Without `FOR KEY SHARE` in `teams::id_of`, a create racing a team delete fails its foreign key (SQLSTATE 23503, a 500). With it, the create waits for the delete and reports the team as unknown. The new race test fails without the lock.
- Plan corrections (Task 3 Step 3 and Step 4, Task 4 Step 6, Task 7 Step 4, Task 8 Step 5): `id_of` locks `FOR KEY SHARE`; `create` returns the owner via `$5::text` instead of an `InsertedCatalog` struct; `set_owner` is a single statement; `owner_team()` returns `Option<Arc<str>>`; the support helpers and the console stop-gap are recorded; Task 4 is warned that a required `user` will break the console key form the same way.
