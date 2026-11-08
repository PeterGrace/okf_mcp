# Search hits carry each concept's size

Issue #185. An agent could not tell how big a concept was before reading
it, and whatever it reads stays in its context for the rest of the session.
Every `search` hit now reports the concept's size, so an agent can skip a
large `get_concept` or go to `sprawl` and `edit_document` instead.

## Changed

- `SearchHit` (`src/okf/search.rs`) gains `lines` and `chars`, both measured
  over the **whole file, frontmatter included**, about the text `get_concept`
  and `read_document` put in context. They use the snapshot's
  universal-newline text, so a `\r\n` ending counts as one character.
  `lines` counts like `wc -l` plus a final unterminated line (Rust's
  `str::lines`); `chars` counts Unicode code points, not bytes.
- The sizes are computed only when a hit is built, from the text the
  snapshot already holds, so a search does no extra I/O and the per-document
  search index is unchanged. Each catalog builds at most `limit` hits before
  the merge cuts the combined list to `limit`, so a search across several
  catalogs measures at most `catalogs x limit` documents; each of those hits
  already scans its body for the snippet, so this adds the same order of work.
- A search through a changeset reports the draft's size, since it runs over
  the changeset's overlay snapshot.
- The `search` tool description names the new fields and suggests checking
  them before a `get_concept`. The output schema documents them as required
  integers.
- The design document's `search` row lists the new fields.
- `src/investigator/contract_tools.json`, the maintenance runner's pinned
  copy of the tool contracts, is re-captured with
  `cargo test --lib investigator_contract_capture_fixture -- --ignored`;
  only the `search` entry changed.

## Compatibility

Additive for MCP clients: the JSON gains two keys per hit, and clients that
ignore unknown keys need no change.

The maintenance runner (`okf-maintenance`) compares the server's `search`
output schema with its pinned contract and fails closed on any structural
difference. Deploy the runner image and the server from the same release:
a runner built before this change refuses a server with it (`Contract`), and
the reverse also fails.
