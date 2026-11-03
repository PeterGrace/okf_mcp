# M6 Task 7: publish: moves, splits and deletes against other sessions (2026-11-03T163350Z)

Task 7 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6), issue #126.

## Added

- `src/mcp/changesets.rs`, `restructure_publish_tests`: 12 database tests:
  - a move's revisions, tombstone and per-path histories;
  - its `Relink` replayed onto a concurrent `index` and onto a concurrent `log`;
  - the `Conflict` on a concurrent edit of a linking file;
  - `link-into-deleted` for a link to the old path published meanwhile;
  - `link-into-deleted` for the link a partial `discard` of the move leaves;
  - a cross-folder move refused when its old listing changed meanwhile;
  - a delete's tombstone and `Unlist` replayed onto a concurrent listing;
  - a split's four revisions (the parent, the new concept, the folder index
    and a linking file);
  - links published meanwhile into the split section: the section's own
    anchor still resolves (the parent keeps the heading), while a subsection's
    anchor is a `broken-anchor` warning and the publish goes through;
  - a delete's `Unlist { bullet: None }` removing its listing although
    another session reworded it meanwhile (Decision 6);
  - the `Conflict` on a concurrent edit of a split's parent (Decision 1).
- Test helpers: `tombstoned`, shared by the move and delete tests, and
  `split_part`, shared by the two split tests. `BIG` gains a `### Deep`
  subsection.

## Changed

- Design, "Publish" step 4: moves, splits and the changed-listing refusal.
- Plan, Task 7: the expected count is 12 (the plan said 8); the code block is
  the shipped module; a dated Corrected note says so.
- Design "Publish" step 4 and plan Decision 18 (dated Corrected note): a link
  to a split-out section's own anchor keeps resolving; only subsection anchors
  become `broken-anchor` warnings. Found by the test Brooke's review on PR #135
  asked for.

## Notes

- Publish needed no code: `replayed_text` replays any stored ops, and
  `links_into_deleted` reads deletion entries. The 9 tests the plan
  listed passed on their first run against publish as merged, so the plan
  gains no T7 -> T8 edge. Of the three review added, the split-anchor test
  failed as first written, against the old Decision 18, and corrected that
  decision; the other two passed as written.
- Review on PR #135 made the log relink test's `replayed` check exact and has
  both `link-into-deleted` tests assert that the changeset stays `Open`.
