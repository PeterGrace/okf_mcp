# M5 Task 3: `log`, `index`, `ensure_index` and replay (2026-11-03T040445Z)

Task 3 of `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md` (milestone 5).

## Added

- `src/okf/ops/replay.rs`: `replay` (idempotent re-application onto current text),
  `log_text` (`cmd_log`'s edit) and `insert_index_bullet_text` (`insert_index_bullet`),
  plus the crate-internal `lists_target`, `missing_heading`, `folder_heading` and
  `new_index_text`; 14 unit tests, two of them Hegel properties (replay equals the op
  and is idempotent; a second branch replayed onto the first keeps every bullet once).
- `src/okf/ops/log_index.rs`: `log`, `index`, `ensure_index` (recording `Log`,
  `ListBullet` and `CreateIndex` replay ops) and the crate-internal `list_concept`
  that Task 4's `create_concept` reuses; 7 unit tests.
- Parity: the script gains `log`, `index` and `ensure_index` steps and nine setup files
  (42 steps in all); the shim runs `ensure_index` directly; `ops.json` regenerated.

## Changed

- `okf::analysis::links::INDEX_HEADING` is `pub(crate)`, shared with the index edit.
- Design, "Data flow", "Write" step 3: the three replay operations.

## Notes

- `insert_index_bullet_text` returns `None` for a missing heading, and its callers word
  okf.py's refusal with the path; `ensure_index` is `pub` for the parity test.
- Replay resolves links without directory information, so a folder link without its
  trailing slash also counts as listing that folder's `index.md`.
- The two Hegel properties run with `derandomize = true`, so every run draws the same
  bounded cases; the plan records this in a dated `Corrected (issue #93)` note, with
  two rustdoc fixes in `log_index.rs`'s module doc.
- The golden was regenerated from okf.py itself (sha256 `3ee65c4d...`): 42 steps, 8 of
  them errors, 18 changed files.
