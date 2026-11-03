# M7 Task 1: `StillLinked` paths with a leading slash (2026-11-03T19:16:11Z)

Issue #140, Task 1 of `docs/superpowers/plans/2026-11-03-okf-mcp-m7-deployment.md`
(Decision 12). The follow-up from Brooke's review of PR #137, recorded in
`changelog/2026-11-03T171325Z-m6-restructuring.md`.

## Changed

- `delete_concept`'s `StillLinked` error writes its `path` and each
  `links[].source` catalog-relative with a leading `/` (`/infra/x.md`,
  `/hub.md`), like every other path a tool returns, so an agent can feed them
  back into a tool unchanged. The error's message starts with the same
  `/`-prefixed path (`/infra/x.md: still linked from ...`).
- `src/error.rs`: the docs of `StillLinked::path` and `LinkSource::source` say
  so.
- Tests: `src/okf/ops/delete.rs`'s `link` helper and `path` matches,
  `src/mcp/restructure.rs`'s delete test, `tests/restructure.rs` (the refused
  delete's `path` and `links`) and `tests/smoke.rs`'s
  `restructuring_on_a_real_catalog` expect the slash. The fixtures that build
  a `StillLinked` by hand (`src/error.rs`, `src/http/mod.rs`) use the real
  shape too; the HTTP one now carries one link instead of none.
- The design's "Errors" table: the `StillLinked` row says `path` and each
  source are `/`-prefixed.

## Notes

The other `OkfError` variants that carry a path, and how each writes it
today. This records the facts; none of them changes here.

| Variant | Field | How it is written |
|---------|-------|-------------------|
| `NotFound` | `path` | From the workspace (`read_text`, `rename`, ...): catalog-relative, no leading `/` (`infra/missing.md`). From `resolve_concept`: the reference as the caller gave it, with or without `/`. |
| `InvalidPath` | `path` | Mixed. As the caller gave it when the path leaves the catalog or uses a reserved name (the workspace's `checked`, `new_concept_path`); normalized and catalog-relative, no leading `/`, for machinery (`check_path`), "already exists" (`new_concept_path`) and "is not a concept document" (`resolve_concept`). |
| `FrontmatterParse` | `path` | Catalog-relative, no leading `/` (`a.md`). |
| `SecretDetected` | `path` | Catalog-relative, no leading `/` (`notes.md`). |
| `Shape` | `path` | Catalog-relative, no leading `/`. |
| `EditMismatch` | `path` | Catalog-relative, no leading `/` (`a.md`). |
| `Io` | `path` | Catalog-relative, no leading `/`, for a document; a filesystem path for a catalog root (`DiskWorkspace`); an archive entry's name for import and export. |
| `Config` | `path` | The configuration file's filesystem path, as given. |
| `LintFailed` | `errors[].file`, `warnings[].file` | Catalog-relative, no leading `/` (its doc says so). |
| `Conflict` | `files[].path` | `/`-prefixed. |
| `StillLinked` | `path`, `links[].source` | `/`-prefixed (this change). |

- The release smoke test `restructuring_on_a_real_catalog` was run once
  against the live catalog, as the task asks.
