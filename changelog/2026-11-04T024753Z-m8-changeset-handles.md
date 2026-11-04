# Milestone 8: changeset handles

Epic #159, plan `docs/superpowers/plans/2026-11-03-okf-mcp-m8-changeset-handles.md`
(PR #158). Released as 0.8.0.

## For clients

- **Open a changeset first.** `open_changeset` (optional `catalog`) returns a handle. Pass
  it as `changeset` to every write tool and to `publish`, `discard`, `revert` and
  `pending_changes`. The write tools no longer take `catalog`: the changeset decides it.
- **Reads see drafts only when asked.** `get_concept`, `read_document`, `search` and the
  reports take an optional `changeset`. `lint`'s `pending` argument became `changeset`; its result's `pending` still says whether drafts were linted. Without
  one, every read is published. A report given a handle and no `catalog` runs on the
  handle's catalog.
- **Finding a handle again:** `list_changesets` lists the open changesets the key may use,
  empty ones included, with their paths and `idle`. An admin key sees every key's, in the
  catalogs it reaches, and may use any of them.
- **Removed:** the `X-OKF-Changeset` header, `?changeset=`, the `default` label,
  `stranded_sessions` and `adopt_changeset`.
- **Refusals:**
  - A write without a handle is a tool error, ``missing field `changeset` ``.
  - An unknown handle, or another key's, is `UnknownChangeset`.
  - A closed changeset is `Refused`. A discarded one adds that an empty changeset is
    discarded after 24 hours.
- **Error paths:** every catalog path in an error payload is `/`-prefixed. Report results
  keep `okf.py`'s forms, so a lint finding is `/a.md` in `LintFailed` and `a.md` in `lint`.

## Storage

- Migration 0003 adds `changesets.handle` (a random UUID, unique). Migration 0004 drops
  `label` and its index. 0.8.0 is therefore forward-only: going back to 0.7.0 means
  restoring the database (`docs/operations.md`, section 8).

## Tasks

| Task | Issue | PR | Delivers |
|---|---|---|---|
| 1 | #160 | #168 | `/`-prefixed paths in error payloads |
| 2 | #161 | #167 | handles, `open_changeset`, `UnknownChangeset`, `ToolContext::changeset` |
| 3 | #162 | #169 | writes, changeset tools and reads take `changeset` |
| 4 | #163 | #170 | `list_changesets` replaces `stranded_sessions` and `adopt_changeset` |
| 5 | #164 | #171 | labels removed; README, operations guide, design; 0.8.0 |
| 6 | #165 | #172 | release and rollout of 0.8.0 |
| 7 | #166 | this PR | milestone changelog, design, smoke test |

## Release smoke test

`OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`
on `main` after Task 6 (`a1bd48f`): 5 passed in 6.59 s.
- **Real catalog:** 227 files, 185 documents, 0 secret findings.
- **Import:** 227 added, lint 0 errors and 11 warnings, in 297 ms.
- **Server start:** start Postgres, import and start the server in about 5 s.
- **Reads:** a 20-hit `search` in 4.9 ms, `get_concept` in 1.9 ms.
- **Write and restructure:** `writes_on_a_real_catalog` and `restructuring_on_a_real_catalog`
  opened changesets by handle and published.

## Decisions and divergences

- **The decisions held:** handles, refusal without one, ownership with admin take-over,
  several open per key, and reads opt in.
- **A one-shot `apply` tool was discussed and dropped** before planning. Handles fixed the
  problem that motivated it.
- **Divergences** are in the plan's `Corrected` notes and in the design's "What M8 shipped
  differently".

## Follow-ups

- Not filed:
  - the remaining M5 write-path gaps: pool limits, the overlay cache's size cap;
  - a minor timing side channel when a handle exists, noted in PR #167's review;
  - no rate limit on `open_changeset`, which the 24-hour prune bounds.
- The claude.ai read check from Task 6 was still pending pgrace when this was written.
