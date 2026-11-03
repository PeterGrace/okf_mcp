# M7 Task 8: backup restore drill (2026-11-03T211902Z)

Task 8 of `docs/superpowers/plans/2026-11-03-okf-mcp-m7-deployment.md` (milestone 7, issue #147).
This was done on the deployed cluster, with pgrace's go-ahead and with publishing paused.
The entry is generic because the repository is public.

## What was done

1. **A fresh backup.** I took an on-demand plugin backup, which completed in 6 s. Before it,
   I recorded the live counts: 1 catalog, 226 documents, 226 revisions, the time of the
   last revision, and `index.md`'s sha256.
2. **A scratch cluster.** I created a 1-instance `Cluster`, `okf-mcp-db-restore`, by hand
   from `docs/operations.md`'s restore example. It had `bootstrap.recovery` from the
   object store, `database`/`owner` `okf_mcp`, the source's `imageName`, and no
   `plugins` section. It was ready 191 s after it was created.
3. **A comparison** at the same moment, live against restored:

   | Field | Live | Restored |
   |---|---|---|
   | catalogs / documents / revisions | 1 / 226 / 226 | 1 / 226 / 226 |
   | last revision | `20:56:45.103701Z` | `20:56:45.103701Z` |
   | `index.md` sha256 | `2651d748...` | `2651d748...` |
   | checksum over every document | `7d115e1e...` | `7d115e1e...` |

   The restored cluster's `-app` secret named `okf_mcp`/`okf_mcp`, which confirms the
   `database`/`owner` fix from Brooke's review of PR #154.
4. **Cleanup.** I deleted the scratch cluster. CloudNativePG removed its pod and volume,
   and nothing named `okf-mcp-db-restore*` is left.

## Changed

- `docs/operations.md`, "Restore":
  - the `imageName` point;
  - the comparison query, now with an all-content checksum;
  - the rule for when the live database moved meanwhile;
  - a line saying the section was tested in this drill.
