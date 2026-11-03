# M6 Task 8: end-to-end restructuring over HTTP (2026-11-03T164915Z)

Task 8 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6), issue #127.

## Added

- `tests/restructure.rs`: 4 end-to-end tests over HTTP with rmcp's client, on M5's
  harness (`tests/support/server.rs`):
  - `a_move_is_published_and_exported`: `move_concept` renames `/infra/time.md` to
    `/infra/clock.md`. The test checks the touched list, the publish's revisions
    (create, delete, two updates) and both histories. In the exported tarball, the
    hub's anchored link and the folder listing follow the move.
  - `a_concurrent_edit_of_a_linking_file_conflicts`: another key republishes the hub
    the move relinked. The move's publish is refused with `Conflict` on `/hub.md`, and
    all four of the move's entries stay pending, the hub's marked stale.
  - `a_concurrent_index_of_the_same_folder_replays`: another key lists a new concept
    in `/infra/index.md`. The move's publish replays its `Relink` onto that index,
    and both bullets survive.
  - `a_delete_is_refused_while_linked_then_published`: `delete_concept` is refused
    with `StillLinked` (`hub.md`, line 6). Once `edit_document` removes the link, the
    delete is published, leaving the folder index as `"# Infra\n\n"`.

## Changed

- Nothing outside the tests.

## Notes

- The plan's test code compiled and passed as written. Three kinds of changes were
  made, and none alters what the plan's code does, so the plan gets no `Corrected`
  note:
  - comments on Rust idioms for a Python reader;
  - rustfmt's line wrapping;
  - one extra assertion: after the conflict, all four of the move's entries are
    still pending.
