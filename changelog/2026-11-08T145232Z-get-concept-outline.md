# get_concept outlines large concepts; `section` and `full`

Issue #186. `get_concept` always returned the whole body, and with `raw`
the whole file a second time. For a large concept that is tens of thousands
of tokens, which stay in an agent's context for the rest of its session,
and Claude Code spills a result over its cap to a file. Refusing large
concepts would be wrong (reorganizing one starts by reading it), so a large
body is now described instead of returned, and the agent asks for what it
needs.

## Changed

- New setting `concept_outline_chars` (default 20,000; `src/config.rs`).
  A body longer than that many characters (Unicode code points) is not
  returned. Instead `get_concept` gives the frontmatter, title, the body's
  `lines` and `chars`, `truncated: true`, a `note` saying how to read more,
  and an `outline`: each heading's text, slug, level, 1-based file line, and
  its section's `lines` and `chars`. A body exactly at the limit is returned
  as before.
- `section` (new parameter): a heading's text (case-insensitive) or its
  anchor slug, the same slug a link's `#fragment` uses. It returns that
  section, subsections included, in `body`, and its outline entry in
  `section`, at any size. A section runs to the next heading of the same or
  a higher level, less trailing blank lines. An unknown heading is
  `Refused` with every heading listed; a text that several headings share
  is `Refused` with their slugs.
- `full` (new parameter) returns the whole body regardless of size.
- `raw` follows the same rule: the file only when the body is within the
  limit, or with `full`.
- Every result now carries `lines`, `chars` and `truncated`. `body` is
  absent when the body is outlined, and `note`, `outline` and `section` are
  absent when not used.
- `src/okf/outline.rs` (new) builds the outline and extracts sections in one
  linear pass. Section ends come from a stack over the headings, and sizes
  from prefix sums of line lengths, so nested sections are not rescanned.
- `split_concept`'s `find_section` (`src/okf/ops/split.rs`), which `section`
  reuses, could not pick the first of two headings sharing a text. Its own
  refusal offers `notes, notes-1`, but `notes` matched both again by text.
  Now, where several headings share a text, a `want` equal to one's slug
  (as written, so `Notes` stays ambiguous) picks it. This is listed as a
  divergence from `okf.py`.
- The maintenance runner's evidence collector (`src/investigator/collect.rs`)
  passes `full: true`, since it needs the whole file as `raw`. The pinned
  contract (`src/investigator/contract_tools.json`) is re-captured with
  `cargo test --lib investigator_contract_capture_fixture -- --ignored`, and
  the runner's fake `get_concept` replies gain the new required fields.
- The tool description, README, `docs/operations.md` and the design
  document describe the threshold and both parameters.

## Not done

`read_document` (large `log.md` and `index.md` files) is unchanged; the
issue leaves it for a follow-up.

## Compatibility

For MCP clients, `body` is now optional: a concept over 20,000 characters no
longer has one unless the call passes `full: true`. A concept within the
limit gives the same result as before, plus three new keys.

The maintenance runner fails closed on a changed `get_concept` schema.
Deploy the runner and the server from the same release.
