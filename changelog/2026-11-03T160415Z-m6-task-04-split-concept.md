# M6 Task 4: `okf::ops::split`: `split_concept`, and `split` parity (2026-11-03T160415Z)

Task 4 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6,
issue #123).

## Added

- `src/okf/ops/split.rs`: `split_concept`, `Split` and `DEFAULT_SPLIT_TYPE`, a port
  of okf.py's `cmd_split` and `find_section`: sources chosen as okf.py chooses them,
  headings re-levelled, footnotes carried or kept, the parent's stub, links to the
  section and its subsections redirected (`Redirect` with anchors), the new concept
  listed (`ListBullet`); 6 unit tests whose expected texts the plan probed from okf.py
  (three of them are also parity steps).
- `markdown::footnote_refs` (FOOTREF_RE, its lookahead done by hand) and the
  crate-internal `atx_heading`; 2 unit tests and a doc example.
- Parity: 3 setup files and 5 split steps (two refused), and a `split` arm in
  `tests/parity.rs`' `run_op`. `ops.json` was regenerated: 47 steps, 10 errors and
  25 changed files before main (#122, #124) was merged in; 52 steps, 11 errors and
  35 changed files with every step after the merge.

## Changed

- `markdown::headings` reads each heading through `atx_heading`.
- The `okf::ops` module doc lists `split_concept` among the restructuring edits.

## Notes

- `tests/parity.rs`' `print_touched` and `with_touched` (which give the split steps
  okf.py's touched list) are #122's copy, kept by the merge with main; this branch
  had added the same helpers before #122 merged.
- `py_truthy_str`'s `expect(dead_code)` attribute was already removed on main by #122,
  so this PR makes no change to `src/okf/pystr.rs`; `split_concept` is a second caller.

- A list or mapping `status` is written as Python's `str()` of it, where okf.py's
  `scalar` raises; the module doc lists this divergence.
- The plan's code compiled and passed as written, except one doc link: `split_concept`'s
  `# Errors` linked the crate-private `load_concept`, which `cargo doc -D warnings`
  refuses; it now names those errors itself (Corrected note in the plan).
