# Import can replace only machinery, and can refuse a moved catalog

Issue #191. Machinery (`AGENTS.md`, `tools/`, `install/` and the rest) is
read-only to the write tools, so `okf-mcp import --replace` is the only way to
update it. Import compared every path, so an operator had to export the whole
catalog, overlay the machinery and re-import it all. A concept published
between the export and the import was reverted, and the report gave only
counts, so the revert showed up only as a number that didn't match.

## Changed

- `ImportOptions` has `scope` (`ImportScope::Everything`, the old behaviour,
  or `ImportScope::Machinery`) and `expect_generation: Option<i64>`.
- `ImportScope::Machinery` imports the catalog's live non-machinery documents
  plus the incoming machinery (`with_catalog_documents`). The incoming
  non-machinery files are ignored, so only machinery can be added, updated
  or deleted, and a concept published meanwhile is kept. The command line
  spells it `--only-machinery`, which clap only accepts with `--replace`.
- `expect_generation` is checked under the catalog's lock, against the
  catalog row read after locking. A mismatch is `Refused` and nothing is
  written (`--expect-generation N`).
- `store::export::export` returns the archive and the generation it holds,
  read in the same `REPEATABLE READ` snapshot as the documents.
  `okf-mcp export` prints it on stderr, since the archive may be on stdout.
  The HTTP export route ignores it.
- `ImportReport`'s `added`, `updated` and `deleted` are sorted path lists
  (they were counts); `unchanged` is still a count. This changes the JSON
  the command line prints and the HTTP import route returns.
- Import now checks, snapshots and lints the files after it takes the
  catalog's lock, not before: the machinery scope's snapshot needs the
  catalog's documents, and one code path serves both scopes. Publishes to
  that catalog wait for the analysis as well as the writes. Import is a rare
  operator action, so that wait is acceptable.

## Not changed

The HTTP import route has neither option. The issue concerns the command
line, and the route's refusal mapping still assumes its only `Refused` is a
non-empty catalog without `?replace=true`.

## Docs

`docs/operations.md` (updating machinery, the generation guard, the report
shape), the design spec's "Import" step 3, and the README's subcommand list.
