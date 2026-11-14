# M14 Task 7a: recording catalog writes

Issue #400, Task 7a of `docs/superpowers/plans/2026-11-13-okf-mcp-m14-operator-api.md`.

## Added

- `catalog.published`: a publish that writes at least one revision records
  one entry in its own transaction, with the changeset handle, the number of
  revisions and the catalog generation before and after. A publish that
  writes no revision records nothing.
- `catalog.reverted`: `revert` records the path and revision (and the
  handle) in the edit's own transaction, through the new
  `write::apply_audited` and `write::OpAudit`.
- `catalog.changeset_discarded` (`reason: "admin"`): an admin closing
  another key's changeset, through `discard` or
  `DELETE /api/v1/changesets/{handle}`, records the handle and the entries
  dropped. A key discarding its own changeset records nothing.
- `ToolContext::with_requester` and `ToolContext::audit_actor`: a tool
  call knows the request's address and source. MCP calls take them from the
  request (`from_parts`), REST calls from the `Requester` extractor.
- `docs/operations.md`: a new "Audit log" section with the three actions.

## Changed

- `api::context(app, principal, requester)` takes the request's
  `Requester`; its 18 REST callers extract one.
- `store::publish::publish(app, state, &PublishInput { changeset, actor,
  message }, audit)`: the arguments are a struct plus the audit actor.
- The plan: dated corrections for `OpAudit.details` (a JSON map) and the
  tests' placement, and where the "Audit log" section lives.

## Notes

- Every entry targets the catalog and belongs to its owner team
  (`audit::catalog_teams`). A failed entry write fails the publish, revert
  or discard with it; tests force that with a failing trigger.
