# M6 Task 5: `okf::ops::delete`: `delete_concept` and `OkfError::StillLinked` (2026-11-03T160145Z)

Task 5 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6),
issue #124.

## Added

- `src/okf/ops/delete.rs`: `delete_concept`: refuses non-concepts (`resolve_concept`),
  machinery (`rules::check_path` on the resolved path, before anything is read or changed)
  and a concept other documents link to (`StillLinked`, one entry per link); removes the
  concept and its listing (`Unlist`, any bullet); a note for an emptied folder;
  7 unit tests.
- `OkfError::StillLinked { path, links }` and `LinkSource { source, line }`, HTTP 409;
  1 unit test, and a 409 assertion in `http::tests::publish_refusals_have_their_statuses`.

## Changed

- `src/okf/ops/mod.rs`: `pub mod delete;`, `pub use delete::delete_concept;`, and the
  module doc's list of restructuring edits names `delete_concept`.
- Design: the "Errors" table and the `delete_concept` row.
- Plan: two dated corrections in Task 5 (the emptied-folder test's expected text, and a
  redundant `#` filter in `links_into`).

## Notes

- Links from `log.md` count (plan Decision 4): publish's `links_into_deleted` and
  lint's `broken-link` read `log.md` too, so the publish would be refused anyway.
- The check runs on the changeset's view at write time; publish's
  `links_into_deleted` catches a link someone else published afterwards.
- An emptied folder's `index.md` loses only the bullet's line, so `"# Solo\n\n* ...\n"`
  becomes `"# Solo\n\n"`, exactly what the `Unlist` replay produces.
- A refusal reports paths and line numbers only, never document text.
