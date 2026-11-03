# M5 Task 12: `stranded_sessions`, `adopt_changeset` and the empty-changeset prune

Issue #97 (epic #89). Plan: `docs/superpowers/plans/2026-11-03-okf-mcp-m5-writes.md`, Task 12.

## Added

- `src/mcp/changesets.rs`, created by this task with the module doc shared by Tasks 10-12.
  Task 12's tools sit in their own `#[tool_router(router = lifecycle_router)]` block, its tests
  in `lifecycle_tests`, and its store imports are function-local, so Tasks 10 and 11 can add
  their blocks in any order.
- `stranded_sessions(catalog?)`: open, non-empty changesets whose last change is older than
  `session_idle_timeout` (`idle_cutoff`: now less the timeout, or the earliest time if that
  overflows). A key sees its own changesets, minus the calling connection's own (same key and
  label); an `admin` key sees every key's. Only catalogs the key reaches are listed, and
  `catalog` narrows the list to one.
- `adopt_changeset(id)`: moves an open changeset to this connection's key and label, which
  bumps its `version`, so cached overlays go stale without an explicit `forget`. The key's own
  changeset needs `write`, another key's needs `admin` (`Forbidden` otherwise). An unknown id,
  a closed changeset and one in an unreachable catalog get the same `Refused`, so a key cannot
  probe. It is refused while this connection already has another open changeset in that catalog.
  If the changeset changes between the access check and the relabel (its `version` moved by
  more than the relabel's bump), the call answers `Busy` and rolls back.
- `App::poll` ends each pass by discarding empty open changesets whose last change is over
  24 hours old (`EMPTY_CHANGESET_HOURS`). A failure is logged at `error` and retried at the
  next pass. It doesn't fail the pass.
- Tests: six in `mcp::changesets::lifecycle_tests` and
  `app::tests::poll_discards_empty_changesets_a_day_old_and_keeps_the_rest`.

## Changed

- `OkfServer::tools` adds `lifecycle_router`; `src/mcp/mod.rs` declares `mod changesets;`.
- Design: the `stranded_sessions` and `adopt_changeset` table rows, and "Session lifecycle"
  says the prune runs on every poll pass.

## Notes

- The changeset's `actor` stays the opener's after adoption; Task 10's `publish` should record
  the publishing key's actor.
- Plan: a dated Corrected note on Task 12 records the `version` check.
