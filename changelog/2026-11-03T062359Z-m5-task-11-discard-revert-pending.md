# M5 Task 11: `discard`, `revert` and `pending_changes` (issue #101)

## Added
- `discard`: drops the named paths from this connection's changeset, or every entry, which also marks the changeset `discarded`. It takes publish's locks in publish's order (in-process publish lock, advisory lock, catalog row, changeset row) and forgets the cached overlay.
- `revert`: adds a document's content at an earlier revision, or its deletion for a `delete` revision, through `write::apply`, as a free edit (no replay). Machinery is refused before the history is read.
- `pending_changes`: each entry with its change (`create`, `update`, `delete`), base and current revision, a `stale` flag, `replayable`, and a unified diff against the published text (`/dev/null` for a missing side, `Binary files ... differ` for non-UTF-8).
- `changes_router` in `src/mcp/changesets.rs`, joined in `OkfServer::tools()` (ten routers).
- `MemWorkspace::remove`; `store::history::{content_at, RevisionContent}`; the `similar` 2.7.0 dependency.
- Tests: `remove_deletes_a_file_and_says_whether_there_was_one`, `content_at_reads_one_revision_or_its_deletion`, and 10 in `changes_tests`, among them `a_write_queued_behind_a_discard_lands_in_a_new_changeset` and `reverting_a_created_file_to_absent_leaves_no_entry`.

## Changed
- `own_changeset` skips the open-changeset lookup for a key below `write`, as `ToolContext::view` does.
- The design's rows for the three tools.
- The plan's Task 11 gains a Corrected note.

## Notes
- A deletion is a touched path that is no longer a file after the op; a file the changeset created and a revert sends back to absent leaves no entry, so `touched` is empty.
- A write queued behind a closing discard lands in a new open changeset, never `Busy`.
- `pending_changes` reads entries and the published state, not the overlay, so a changeset whose overlay no longer applies can still be listed and discarded.
- Diffs run in parallel (rayon) on the blocking pool.
