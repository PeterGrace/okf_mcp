# Maintenance runner: excerpts may span a hard line wrap

Issue #210. `to_submission` accepted an evidence excerpt only if it was an
exact byte substring of its source. Catalog prose is hard-wrapped at about
80 columns, and models often join a wrapped sentence with a space. In run
`7542030392c6223f90eb708f05f752ba` that one mismatch rejected a proposal
whose other findings were valid.

## Changed

- `to_submission` (`src/investigator/proposal.rs`) now locates each excerpt
  with `source_span`. A verbatim quote is tried first (`str::find`). If it
  is absent, both texts are folded by `fold_whitespace` (each run of ASCII
  whitespace becomes one space; no case folding, no Unicode
  normalization) and the folded excerpt is searched in the folded source.
  The first match is mapped back through `FoldedSource::starts` to the
  original byte range.
- The stored `Evidence::excerpt` is always that exact span,
  `source.text[start..end]`, so it stays a true substring of the pinned
  revision and the server's exact-substring check still holds. A trailing
  whitespace run in the excerpt keeps the source's whole run.
- The 2048-byte bound now applies to the stored span, not the model's
  text. The empty check still applies to the model's excerpt.
- Each source is folded at most once per proposal, and only when some
  excerpt of it is not verbatim. Matching is linear in the source length;
  no pattern is compiled from model text.
- The model prompt (`collect.rs` `INSTRUCTIONS`) says an excerpt may span a
  line break and that whitespace runs match any whitespace run.
- `docs/operations.md` describes the runner's tolerance.

## Tests

- Unit tests in `proposal.rs` for `fold_whitespace`, `FoldedSource`
  offsets, exact slices, and the verbatim fast path (which never folds).
- `investigator_proposal_excerpt_tolerates_whitespace_and_stores_source_span`:
  the run-7542030 case keeps its newline; spaces, tabs and CRLF in either
  side; multibyte text; trailing whitespace; first match; verbatim
  preference; every stored excerpt is source text.
- `investigator_proposal_excerpt_rejects_non_whitespace_differences`: an en
  dash changed to a hyphen, case, punctuation, removed whitespace and extra
  trailing text are rejected.
- `investigator_proposal_excerpt_bound_applies_to_stored_span`.
- Three existing cases that relied on `\n` not matching `\r\n` (or a space
  not matching a newline) now use a punctuation or case difference instead.
