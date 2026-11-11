# M10 Task 1: Tool annotations (issue #245)

## Added

- Every MCP tool (41) has a `title` and `annotations` with the same title
  and `readOnlyHint`, `destructiveHint`, `idempotentHint` and
  `openWorldHint`, for the Claude and ChatGPT connector directories.
- `src/mcp/annotations.rs` (test-only): the tool classification (17 reads,
  20 staged edits, 4 irreversible or deleting) and tests that every
  registered tool is classified exactly once, has a Title Case title in
  both places and carries its class's hints, and that the hints serialize
  as camelCase.

## Changed

- No tool's name, description, parameters or results changed.
- `src/investigator/contract_tools.json` is recaptured (its ignored capture
  test), so the investigator's nine pinned tools carry the new `title` and
  `annotations`. Its contract check compares only schemas and is unchanged.

## Notes

- `revert` is a staged edit (it adds an earlier revision to the changeset
  for review and publish), not destructive as the spec's table had it. The
  spec is corrected in Task 8.
- `openWorldHint` is false everywhere: every tool acts only on this
  server's catalogs.
