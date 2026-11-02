# M3 Task 11: Lint's commit times from revisions, and the git-era wording (2026-11-02T17:23:25Z)

Issue #31, Task 11 of `docs/superpowers/plans/2026-11-02-okf-mcp-m3-storage.md`. The
server has no git, so lint's `generated-after-commit` check never ran outside tests.
A document's last revision time is the same fact as its last commit time, so the
loaded catalog now carries commit times built from each live document's
`updated_at`, and linting it runs the check.

## Added

- `src/store/documents.rs`: `commit_times(rows: &[DocumentRow]) -> CommitTimes`, keyed
  by each row's stored path (already catalog-relative, the spelling lint looks up),
  with no dirty paths (M5 adds a session's changeset paths).
- `src/catalog.rs`: `Loaded` holds a `commit_times` field, computed in `build` from
  the same rows as the snapshot (one `REPEATABLE READ` read, so they agree), with
  `Loaded::commit_times()` and `Loaded::lint(now)`, which passes
  `Some(&commit_times)` to `analysis::lint::lint`.
- Tests: `store::documents::tests::commit_times_are_the_documents_update_times`,
  `catalog::tests::lint_flags_a_stamp_later_than_the_documents_last_revision`
  (a stamp 10 minutes after the import is flagged, one an hour before is not).

## Changed

- Doc comments no longer describe a git-backed server: the `analysis::lint` module
  doc, `CommitTimes`, `CommitTimes::new` (arguments and key spelling) and `lint`'s
  `commits` argument; `okf::snapshot`'s module doc (now names `CatalogState`, rebuilt
  after each publish or import); `DiskWorkspace`'s symlink-race gap (the server
  never writes through one); `okf`'s module doc ("neither a database nor a real
  filesystem"). `src/error.rs` already had no git wording and is unchanged.
- The plan's Task 11 carries a dated "Corrected (issue #31)" note for those two
  differences. Task 13's `loaded.lint(Utc::now())` matches the shipped signature.

## Notes

- After a first import, every document's commit time is the import time, so
  `generated-after-commit` flags only stamps later than that import (plus lint's
  slack). This is the documented semantic, not a loss of history: the tarball
  import carries no commit history.
- The issue's original M2 follow-up (fill `CommitTimes` from git, stripping the
  catalog's prefix in a repository subdirectory) is superseded: stored paths are
  catalog-relative by construction.
