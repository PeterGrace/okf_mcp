# M5 Task 2: `okf::ops`: the shared op types and the frontmatter ops (2026-11-03T033828Z)

Task 2 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md` (milestone 5), issue #91.

## Added

- `src/okf/ops/mod.rs`: `OpContext`, `OpOutcome` (`touched`, `replay`, `message`,
  `notes`), `StampField`, `SourceEntry`, `LogKind`, and the crate-internal helpers
  `load_concept`, `save`, `stamp_value`, `refused`.
- `src/okf/ops/frontmatter.rs`: `stamp`, `review`, `question`, `unquestion`, `source`,
  `OUTCOMES`; ports of `okf.py:653-750`, with 14 unit tests on a `MemWorkspace` with a
  frozen clock.
- `src/okf/ops/replay.rs`: the `ReplayOp` type (Task 3 adds its functions).
- `Doc::remove_lines`, for `unquestion`'s single-item removal (1 unit test).
- Parity: `okf_shim.py ops ROOT NOW` runs a JSON script through okf.py's `cmd_*`
  functions on a temp copy with `now_utc` frozen; `tests/parity/ops_script.json`
  (26 steps, one setup file); `tests/golden/ops.json`, generated from okf.py
  (SHA-256 `3ee65c4d...`, unchanged in `tests/golden/SOURCE`); `ops_match_golden` and
  the live `live_ops_parity`.

## Changed

- Design, "Errors": the ported commands refuse with okf.py's wording.
- Plan, Task 2 and Task 3: `OpOutcome::record` moves to Task 3, its first caller
  (an unused `pub(crate)` method fails `clippy -D warnings`).
- Plan, Task 2 and Task 3: public doc comments no longer link the crate-private
  `load_concept` (rustdoc `-D warnings` rejects it); they spell the errors out or point
  at `stamp`.

## Notes

- `die(msg)` maps to `Refused { reason: msg }`; `NotFound`, `InvalidPath`,
  `FrontmatterParse` and `Shape` keep their M1 wording, so the parity script holds only
  refusals.
- Removing a mapping item from `open_questions` prints Python's `str()` of it
  (`{'k': 'v'}`), and converting a flow list re-renders such an item as that quoted
  text, as okf.py does.
- The unit tests' inputs were also run through okf.py (via the new shim command) while
  implementing: every expected file and message is okf.py's.
- Review of PR #105: `Doc::remove_lines` is `pub(crate)`; `unquestion` matches
  `open_questions` exhaustively; a 14th test pins `unquestion` on a multi-line flow list and
  a multi-line mapping item to okf.py's output, and a comment names the shapes that reach
  its rewrite fallback (flow lists; probing okf.py found no block list that does).
