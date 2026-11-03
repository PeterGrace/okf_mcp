# M6 Task 6: the restructuring tools (2026-11-03T163332Z)

Task 6 of `docs/superpowers/plans/2026-11-03-okf-mcp-m6-restructuring.md` (milestone 6),
issue #125.

## Added

- `src/mcp/restructure.rs`: `move_concept`, `split_concept` and `delete_concept`,
  each on `write::apply`, in `restructure_router`; 5 tests (what a move stores; a
  split's listing and notes; a delete refused while linked, then allowed; a move
  through `pending_changes`, `revert` and `discard`; the tool listing).

## Changed

- `OkfServer::tools()` joins `restructure_router`, last.
- Design: the `move_concept` and `split_concept` rows.

## Notes

- `split_concept`'s `type` is the Rust field `kind` (`#[serde(rename = "type")]`);
  the listing test checks the input schema says `type`, not `kind`.
- A move is several entries; `discard` of some leaves the rest, and publish's lint
  finds anything left broken (plan Decision 17).
- The plan's code compiled and passed as written, with the corrections Tasks 3-5
  already made (split's `ListBullet` recorded ahead of `Relink`; a delete's emptied
  index keeps `"# Infra\n\n"`), so the plan needed no Corrected note for this task.
