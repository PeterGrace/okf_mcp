# M6 Task 3: `okf::ops::moves`: `move_concept`, and `mv` parity (2026-11-03T160305Z)

Task 3 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6),
issue #122.

## Added

- `src/okf/ops/moves.rs`: `move_concept`, a port of okf.py's `cmd_mv`: the old
  listing is taken (`Unlist`, with the bullet it read), the file renamed, the
  listing added in the new folder (`ensure_index`, `ListBullet`), and every link
  rewritten (`Relink` on index.md and log.md, free edits elsewhere); 5 unit tests.
- `MemWorkspace::rename`, keeping bytes and the execute bit; 1 unit test.
- Parity: 5 setup files and 5 steps (a log line and four moves, one refused); the
  shim stubs `okf.git` with `untracked_git` (the copy is never tracked, so
  `cmd_mv` renames); the Rust side appends okf.py's `print_touched` block;
  `ops.json` regenerated (47 steps, 9 errors, 28 changed files).

## Changed

- `MemWorkspace::write_atomic` shares its placement check (`placement_conflict`)
  with `rename`.
- `pystr::py_truthy_str` lost its `expect(dead_code)` attribute: `move_concept`
  calls it.
- The `okf::ops` module doc lists `move_concept` among the restructuring edits.

## Notes

- The op's message is okf.py's first line; the tool returns `touched` itself.
- okf.py runs the shim's `mv` without git either way: verified while planning, the
  stubbed and unstubbed runs printed identical JSON.
- Plan correction: the shim's git stub is a named, typed function instead of the
  plan's one-line lambda (dated Corrected note in Task 3 Step 8).
