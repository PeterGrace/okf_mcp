# M6 Task 2: replay: `ReplayOp::Unlist` and `ReplayOp::Relink` (2026-11-03T154141Z)

Task 2 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6).

## Added

- `ReplayOp::Unlist { target, bullet }`: removes the first bullet of an index that
  links a path; with `bullet`, the replay refuses a listing that changed since (a
  move carries that text to the new folder).
- `ReplayOp::Relink { redirect }`: re-runs a move's or split's link rewrite on an
  `index.md` or `log.md`, byte for byte, through `relink::rewrite_text`. The arm
  refuses (`Refused`) a `redirect.from` that is not a concept, which the replay's
  empty workspace relies on. It is an error rather than a `debug_assert!`
  because the op is read from the database (review on PR #130).
- `replay::{listing_line, unlist_text}` (crate-internal) and
  `OpOutcome::record_rewrites`; 9 unit tests, including an `Unlist` of a CRLF
  index (it writes `\n`, as `cmd_mv` does) and of a missing one (`NotFound`, by
  design: publish never replays onto a file deleted since the entry's base).

## Changed

- `log_index::collapse_whitespace` is `pub(crate)`; `pystr::py_truthy_str` (Python's
  `str(v) if v else None`) is new, with a unit test. It carries
  `#[cfg_attr(not(test), expect(dead_code, ...))]` until Tasks 3 and 4 call it; the
  plan tells both to delete the attribute.
- `write::merged_replay`'s `expect` text names the new field types.
- `relink.rs`'s module doc names `ReplayOp::Relink`.
- Design, "Data flow", "Write" step 4: the five replay operations, and that
  `Relink` keeps a file's line endings while the other edits write `\n`.

## Notes

- Publish needs no code: `replayed_text` replays whatever ops an entry stores.
- Link rewrites in `log.md` replay too (plan Decision 7, confirmed by pgrace):
  `is_bookkeeping` in `src/okf/ops/mod.rs` is the one place to change that.
