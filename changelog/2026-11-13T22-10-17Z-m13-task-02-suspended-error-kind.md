# M13 Task 2: the `Suspended` error kind (issue #364)

## Added
- `OkfError::Suspended { catalog, team, notice }`, kind `Suspended`: the key reaches the catalog, but its owner team is suspended. Its message is `catalog "<c>" is unavailable: its team "<t>" is suspended`, followed by `. <notice>` when the operator left one.
- Its JSON body carries `catalog`, `team` and `notice` (`null` when there is none). It is not an internal error, so the message reaches clients as written.
- HTTP answers it with 403, like `Forbidden`.
- Tests: `suspended_names_the_catalog_team_and_notice`, `suspended_without_a_notice_has_a_null_notice` (`src/error.rs`), `suspended_is_403` (`src/http/mod.rs`).
- A lessons rule: a test that a JSON field is `null` checks the key is present with `get`.

## Changed
- `AppError::status`'s doc comment: 403 also covers a suspended team's catalog.
- The M13 plan, Task 2: `suspended_message` takes `Option<&str>`, and the no-notice test uses `body.get("notice")`, with a dated correction note. Task 5's `after["suspended_at"]` null check uses `get` too.

## Notes
- Nothing returns `Suspended` yet; Task 3 (`Principal::require`) and Task 4 (`selection::resolve`) do.
- The MCP tool-error mapping (`tool_error` in `src/mcp/mod.rs`) builds its body from `to_json` and matches no variants, so it needed no change.
- The plan's `assert_eq!(body["notice"], Null)` passed with the `notice` field removed from `to_json`, because serde_json indexing answers `Null` for a missing key. Found by reverting the field; the test now checks the key exists.
