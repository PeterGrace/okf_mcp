# M3 Task 10: `catalog::CatalogState`: load, rebuild and swap the snapshot (2026-11-02T16:56:30Z)

Issue #32, Task 10 of `docs/superpowers/plans/2026-11-02-okf-mcp-m3-storage.md`. Each
catalog the server holds in memory is a `CatalogState`: the snapshot of its live
documents, rebuilt and swapped in when the catalog's `generation` moves.

## Added

- `src/catalog.rs`: `Loaded` (one immutable state: `generation`, `snapshot`) and
  `CatalogState` (`load`, `current`, `refresh`, `id`, `name`).
  - The state is a `std::sync::RwLock<Arc<Loaded>>`. The lock is held only to clone
    or replace the `Arc`, never across an `.await` and never while a snapshot is
    read or built, so a reader that took the old `Arc` keeps a whole, consistent
    snapshot after a swap.
  - The generation and the live rows (`store::documents::published`) are read in one
    `REPEATABLE READ, READ ONLY` transaction, so they agree even if an import
    commits meanwhile. The rows become a `MemWorkspace` (`from_stored`) and
    `Snapshot::build` runs on `tokio::task::spawn_blocking`.
  - `refresh` reads the generation and returns `false` without rebuilding when it
    has not moved. Rebuilds of one catalog run one at a time under a
    `tokio::sync::Mutex<()>`; a refresh that waited re-checks the generation, so
    concurrent refreshes build once (review of PR #58). `Cargo.toml`'s `tokio`
    gains the `sync` feature.
  - A deleted catalog is `OkfError::CatalogNotSelected`, from `refresh`'s check or
    from the build's own read (a delete between the check and the transaction).
- `src/lib.rs`: `pub mod catalog;`.
- Tests: `catalog::tests::{load_builds_the_snapshot_of_the_live_documents,
  load_refuses_an_unknown_catalog,
  refresh_swaps_in_a_new_snapshot_only_when_the_catalog_changed,
  concurrent_refreshes_build_and_swap_once, a_deleted_catalog_cannot_be_refreshed, build_names_a_catalog_deleted_before_it_read}`.

## Changed

- The plan's Task 10 carries a dated "Corrected (issue #32)" note: the private
  `build` takes the catalog's name and reads the generation with `fetch_optional`,
  so a catalog deleted mid-build is `CatalogNotSelected` rather than a bare
  `Database(RowNotFound)`; two extra tests. The public API matches the plan, so
  Tasks 11 and 13 need no change. A second note (after review) records the
  rebuild mutex and `begin_with`.

## Notes

- The load test's fixture links with `[logo](logo.png)`, not an image embed: the
  link parser, like `okf.py`'s, ignores `![..](..)`.
- M4's poll loop and M5's publish are the callers of `refresh`.
