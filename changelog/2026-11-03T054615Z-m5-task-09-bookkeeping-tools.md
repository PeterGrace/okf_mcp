# M5 Task 9: the bookkeeping write tools (2026-11-03T054615Z)

Issue #99. Ports of `okf.py`'s concept commands, `log` and `index` as MCP write tools.

## Added

- Seven tools in `src/mcp/writes.rs`, each calling its `okf::ops` function through
  `write::apply`: `stamp`, `review`, `question`, `unquestion`, `add_source`, `log` and
  `index`. `stamp` and `review` take `by` (default: the key's actor); no tool takes a
  timestamp.
- Their params: `StampParams`, `ReviewParams`, `QuestionParams`, `UnquestionParams`,
  `AddSourceParams`, `LogParams` (`kind` optional, default `Update`), `IndexParams`; and
  `QuestionRef` (a 1-based number or the exact text, `#[serde(untagged)]`).
- `bookkeeping_router`, a second `#[tool_router]` block, joined in `OkfServer::tools()`.
- Tests (`bookkeeping_tests`): `log_records_its_replay_on_the_log_md_entry`,
  `a_free_edit_of_an_index_after_index_clears_its_replay`,
  `the_concept_commands_edit_frontmatter_as_free_edits`,
  `unquestion_takes_a_number_or_text_and_log_takes_okf_py_kinds`,
  `the_bookkeeping_tools_are_listed_with_their_defaults`.

## Changed

- `okf::ops::StampField` and `okf::ops::LogKind` derive `JsonSchema`, and `LogKind`
  derives `Default` (`Update`, `okf.py log`'s default). The params use them directly,
  so the plan's mirror enums `StampFieldParam` and `LogKindParam` and their `From`
  impls were not added.
- `OkfServer::tools()` adds `+ Self::bookkeeping_router()`.

## Notes

- A separate router rather than extending `writes_router`: Task 8's block stays as it
  was, and the two groups match the design's split between content edits and
  `okf.py` ports.
- `unquestion` takes a JSON number or a string. A string of digits is still a position,
  as in `okf.py`, since the op parses the argument the same way.
- `log`'s kinds are `okf.py`'s capitalized spellings (`Update`, `Creation`, `Change`,
  `Import`); `update` is a parameter error, as argparse's `choices` refuse it.
- The frontmatter commands store no replay, so they conflict on a stale base like any
  free edit. `log` and `index` store theirs, and a later free edit of the same file
  (`edit_document` of an `index.md`) clears it.
