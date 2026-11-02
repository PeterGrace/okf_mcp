# M3 Task 1: the in-memory workspace holds bytes and execute bits (2026-11-02T15:41:33Z)

Issue #35, Task 1 of `docs/superpowers/plans/2026-11-02-okf-mcp-m3-storage.md`. A stored
catalog's snapshot will be built over `MemWorkspace` (Task 10), so it becomes production
code and must know every file of the catalog, text or not, with its execute bit.

## Added

- `okf::workspace::StoredFile { content: Vec<u8>, executable: bool }`: one stored file.
- `MemWorkspace::from_stored(files)`: builds a workspace from `(path, StoredFile)` pairs.
  `OkfError::InvalidPath` for a path outside the catalog or the root, two paths that
  normalize to the same one, or a file that is also another file's directory (review of
  PR #48: the trees `write_atomic` can never produce).
- `MemWorkspace::stored(path)`: a copy of one file's bytes and execute bit.
- `MemWorkspace::into_stored()`: every file, consuming the workspace without a copy.
- Tests: `stored_files_keep_bytes_and_execute_bits`,
  `a_write_keeps_an_existing_execute_bit_and_new_files_are_not_executable`,
  `from_stored_refuses_a_path_outside_the_catalog`,
  `from_stored_refuses_paths_that_normalize_to_the_same_file`,
  `from_stored_refuses_a_file_that_is_also_a_directory`, `into_stored_returns_every_file`
  (workspace), `a_link_to_a_binary_file_resolves` (snapshot).

## Changed

- `MemWorkspace` is production code. It holds each file as bytes with its execute bit,
  so a non-text file exists (`is_file`, `list_files`; a link to it resolves) but
  `read_to_string` on it fails with `OkfError::Io` (`InvalidData`), as `DiskWorkspace`
  does. `write_atomic` keeps an existing file's execute bit; a new file is not
  executable. `from_files` still takes text fixtures, none executable.
- Design, "Storage": text means valid UTF-8 (NUL included, since content is `bytea`).
  "In-memory snapshot": the workspace holds bytes and execute bits, replacing the plan
  of keeping execute bits and non-text files beside it.

## Removed

- The `test-util` feature (`Cargo.toml`), every `cfg(any(test, feature = "test-util"))`
  in `src/okf/workspace.rs`, and the README paragraph about it. Closes #35.

## Notes

- Plan corrections (dated notes in Task 1): the snapshot test's fixture is a plain link
  `[logo](img/logo.png)`, because `okf.py`'s link pattern skips image embeds
  (`![..](..)`), so the plan's fixture produced no link. Task 10's
  `load_builds_the_snapshot_of_the_live_documents` had the same fixture and is fixed
  inline. `read_to_string` validates the borrowed bytes with `std::str::from_utf8`
  before copying, instead of cloning them first. The design's sentence naming
  `test-util` in "In-memory snapshot" was reworded too, so the plan's `test-util` grep
  finds only the "Carried into M3" entry.
