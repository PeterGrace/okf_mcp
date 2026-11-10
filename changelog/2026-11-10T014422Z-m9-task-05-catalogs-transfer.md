# M9 Task 5: catalogs and import/export

Issue #220, Task 5 of `docs/superpowers/plans/2026-11-09-okf-mcp-m9-web-console.md`.

## Added

- `src/http/api/catalogs.rs` (replaces Task 1's stub):
  - `GET /api/v1/catalogs`: every loaded catalog the key reaches, with its
    description, file and concept counts, the loaded snapshot's generation and
    its last change (`CatalogSummary`).
  - `POST /api/v1/catalogs` (201): creates an empty catalog. It needs an admin
    key that reaches every catalog; a key granted named catalogs is 403, since
    it would not reach the catalog it made. An invalid or taken name is 409.
  - `PATCH /api/v1/catalogs/{catalog}`: replaces the description and answers
    `{name, previous, description}`, as `okf-mcp catalog update` does.
  - `DELETE /api/v1/catalogs/{catalog}[?force=true]`: 409 with unpublished
    changes unless forced; the store's "pass --force" hint becomes "add
    ?force=true".
  - Every change polls at once, so the next request sees it. Catalog rows
    record no actor (as with the CLI); a `tracing::info!` line names the key
    id and actor.
- `GET /api/v1/catalogs/{catalog}/export` and `POST .../import` under
  `/api/v1`: the same handlers, timeout, body cap and upload deadline as the
  old `/catalogs/{name}/...` routes, mounted through `catalogs::routes(app)`
  inside `api::routes`, so `require_key` and the API headers apply.
- Import options on both paths: `?only_machinery=true` (the CLI's
  `--only-machinery`, `ImportScope::Machinery`) and `?expect_generation=N`
  (`--expect-generation N`; a stale one is 409 "... export it again").
- The Catalogs page (`pages/catalogs.js`): a sortable list; admins edit the
  description and delete (typed confirmation, a force box); an all-catalogs
  admin creates. The catalog switcher is rebuilt after a create or delete.
- The Import & export page (`pages/transfer.js`) and `components/upload.js`:
  export as a `blob:` download (an `<a href>` cannot carry the bearer
  header); import by picker or drag and drop with an `XMLHttpRequest` upload
  progress bar, the CLI's options, the expected generation pre-filled from
  the listing, and a result panel with the report's counts, paths and lint,
  or the refusal with its problems.
- Tests: `only_machinery_replaces_machinery_and_keeps_the_concepts`,
  `an_import_expecting_another_generation_is_409_and_imports_nothing`
  (`archives.rs`); the access matrix and five behaviour tests in
  `api/catalogs.rs`; four browser journeys in `tests/ui_e2e/catalogs.rs`.
- Docs: `docs/operations.md` "Catalogs and import/export" subsection, the
  route table, section 9's HTTP import options with a guarded machinery-only
  `curl`, and the 409 troubleshooting row; `README.md`'s import row, the 409
  paragraph and a Web console line.

## Changed

- `src/http/archives.rs`: the route builders `export_route()` and
  `import_route(app)` are shared by both mounts; `routes(app)` uses them.
- The import's `Refused` rewrite is now a text replacement of "pass
  --replace" with "add ?replace=true", so the generation refusal keeps its own
  message. The existing "not empty" message is unchanged.
- The test tarball helper `tarball_of` moved to module level
  (`#[cfg(test)] pub(crate)`) for the API tests.
- `api::require_admin` and `Ui::wait_hidden` lose their
  `#[allow(dead_code)]`: this task calls both.

## Notes

- Matrix rows: list (any reaching key, the stranger sees `other`), export
  (read), import (admin; `{}` is not gzip, so 400 past the key check), create
  (403 for every catalog-scoped key), update and delete (admin; the stranger
  404). The delete row runs last.
- After review (PR #228), the Import & export page runs one export or import
  at a time: the catalog picker, Export and Import are disabled while one is
  in flight, a file chosen meanwhile does not unlock Import, and each
  operation names its own catalog in its URL, file name and messages. The
  journey `an_export_or_import_in_flight_locks_the_catalog_and_admits_no_second_import`
  delays the export and the upload to check it.
- The pages check `main.isConnected` after awaits before closing the drawer
  or handing over a download (plan "Corrected (issue #220)" notes).
