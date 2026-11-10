# M9 Task 6: findings and runs

Issue #221, Task 6 of `docs/superpowers/plans/2026-11-09-okf-mcp-m9-web-console.md`.

## Added

- Six routes in `src/http/api/maintenance.rs`, each calling the matching
  `maintenance_*` tool function: `GET .../maintenance/runs`,
  `.../runs/{run}`, `.../findings` (with `?disposition=`),
  `.../findings/{finding}`, `.../counts`, and
  `POST .../findings/{finding}/disposition`. Reading needs read access,
  deciding needs write, as the tools require.
- `store::maintenance::reports::list_findings_with`: findings narrowed to one
  current disposition (the last appended, else `open`). The filter runs in
  SQL inside `page_ids`, so `limit` and `next_after` count matching findings
  only.
- `store::maintenance::reports::count_findings` and `FindingCounts`: one
  `GROUP BY` over the same current-disposition expression, shared with the
  filter through the `current_disposition!` macro so the two cannot drift.
- `mcp::maintenance::reports::maintenance_list_findings_by` and
  `maintenance_finding_counts` (console-only, not MCP tools).
- The Findings page (`pages/findings.js`): a disposition filter, a sortable
  table, "Load more", and a drawer with the claim, its pinned evidence, the
  decision history and, for write keys, the four decisions with a rationale.
  The console makes the idempotency key and reuses it when the same decision
  is retried.
- The Runs page (`pages/runs.js`): runs newest first, and a drawer listing a
  run's work items with links to their concepts and findings.
- The nav badge: `refreshFindingsBadge()` shows the selected catalog's open
  findings, after sign-in, on a catalog switch, on both pages' loads and
  after every decision.
- Tests: the store test
  `maintenance_findings_filter_and_count_by_current_disposition`, six API
  tests (access matrix, filter and counts, idempotent retry, bad requests, a
  reporter key refused a decision and an unknown catalog, run report), and the journeys
  `a_writer_dismisses_a_finding_and_its_state_changes` and
  `a_reader_sees_the_run_and_its_work_but_cannot_decide`.
- `docs/operations.md`, "Web console": a "Findings and runs" subsection;
  `README.md`: one sentence.

## Changed

- `runs::page_ids` takes a `Listing` (`Runs` or `Findings(Option<Disposition>)`)
  instead of a `findings: bool`.
- `maintenance_list_findings` delegates to `maintenance_list_findings_by` with
  no filter. The MCP tool schemas are unchanged.
- `reports::enum_text` is `pub(super)`, for `page_ids`' filter.
- `main.js` imports `refreshFindingsBadge` and calls it in `enter()` and on a
  catalog switch.

## Notes

- The disposition body is `{disposition, rationale, idempotency_key}` (the
  tool's names), not the spec's `{disposition, note}`; `idempotency_key` is
  required. The counts route is an addition to the spec.
- The plan's open checks held: `Option<Disposition>` deserializes from
  `Query` (an unknown value is 400), and an `app_with` catalog can be claimed
  for maintenance through the store.
- The drawers reuse Task 2's `dl.facts` styles; `styles.css` gains only the
  disposition button rules.
- The pages check `main.isConnected` after their awaits before opening or
  redrawing the drawer.
