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
- Parity: 3 setup files and 5 split steps (two refused); `ops.json` regenerated
  (47 steps, 10 errors, 25 changed files).
- `tests/parity.rs`: `print_touched` and `with_touched` (Task 3, Step 10), so the
  split steps compare okf.py's touched list too. Task 3 adds the same helpers;
  whichever of #122/#123 merges second keeps one copy.

## Changed

- `markdown::headings` reads each heading through `atx_heading`.
- `pystr::py_truthy_str` loses its `expect(dead_code)` attribute: `split_concept`
  calls it.
- The `okf::ops` module doc lists `split_concept` among the restructuring edits.

## Notes

- A list or mapping `status` is written as Python's `str()` of it, where okf.py's
  `scalar` raises; the module doc lists this divergence.
- The plan's code compiled and passed as written, except one doc link: `split_concept`'s
  `# Errors` linked the crate-private `load_concept`, which `cargo doc -D warnings`
  refuses; it now names those errors itself (Corrected note in the plan).
