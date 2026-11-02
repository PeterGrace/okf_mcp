# M3 Task 2: the unlinked-mention matcher is built once per snapshot (2026-11-02T15:58:10Z)

Issue #33, Task 2 of `docs/superpowers/plans/2026-11-02-okf-mcp-m3-storage.md`.
`links_report` compiled its `RegexSet` over every concept name on each call. Snapshots
become long-lived in M3 (Task 10), so the set is now compiled once per snapshot.

## Added

- `Snapshot::mention_matcher()` (`pub(crate)`): the matcher over every concept in the
  link graph, built on the first call behind a `OnceLock` field and reused afterwards.
- `Snapshot::link_graph_concepts()` (`pub(crate)`): the concepts in the link graph, the
  one filter `links_report` (mention sources) and `mention_matcher` (targets) share
  (review of PR #52).
- Tests: `the_mention_matcher_is_built_once_per_snapshot` (snapshot): the cache is
  empty before the first links report and filled by it, the second report reuses
  that matcher, and the two reports are equal (tightened in review of PR #52);
  `link_graph_concepts_are_the_concepts_outside_root_machinery` (snapshot).

## Changed

- The mention matcher is built once per snapshot (#33). `MentionMatcher` owns its
  concept paths (`String`, no lifetime) so the snapshot can hold it, is `pub(crate)`,
  and derives `Debug, Clone` (as does `NameSearch`), since `Snapshot` derives both.
  `links_report` uses `snapshot.mention_matcher()`. The links JSON is unchanged.
- Plan, Task 2: `with_set_limit` stays private (dated correction note).

## Notes

- On the real catalog (release build, 143 concepts, 200 unlinked mentions) the first
  links report on a snapshot took 154 ms and the next two 14 ms and 13 ms, measured
  with a throwaway test that was not committed.
