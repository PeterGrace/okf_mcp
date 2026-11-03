# M6 Task 7: publish: moves, splits and deletes against other sessions (2026-11-03T163350Z)

Task 7 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6), issue #126.

## Added

- `src/mcp/changesets.rs`, `restructure_publish_tests`: 9 database tests:
  - a move's revisions, tombstone and per-path histories;
  - its `Relink` replayed onto a concurrent `index` and onto a concurrent `log`;
  - the `Conflict` on a concurrent edit of a linking file;
  - `link-into-deleted` for a link to the old path published meanwhile;
  - `link-into-deleted` for the link a partial `discard` of the move leaves;
  - a cross-folder move refused when its old listing changed meanwhile;
  - a delete's tombstone and `Unlist` replayed onto a concurrent listing;
  - a split's four revisions (the parent, the new concept, the folder index
    and a linking file).
- A test helper, `tombstoned`, shared by the move and delete tests.

## Changed

- Design, "Publish" step 4: moves, splits and the changed-listing refusal.
- Plan, Task 7: the expected count is 9, not 8; the code block is the shipped
  module; a dated Corrected note says so.

## Notes

- Publish needed no code: `replayed_text` replays any stored ops, and
  `links_into_deleted` reads deletion entries. All 9 tests passed on their
  first run, so the plan gains no T7 -> T8 edge.
