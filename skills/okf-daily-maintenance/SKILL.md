---
name: okf-daily-maintenance
description: Daily interactive upkeep of an OKF knowledge catalog on the okf-mcp server. Checks the maintenance runner's latest run, walks its findings one at a time (apply, explain, defer, skip), then works through sprawl, links, open questions, the oldest concept, stale changesets and backlog entries, delegating to okf-reorganize, okf-link-check and okf-review-oldest. Use when asked for "daily maintenance", "go over the maintenance report", "work through the findings", "catalog cleanup", or "daily catalog upkeep".
---

# Daily catalog maintenance

The scheduled maintenance runner investigates concepts and files findings,
but it never edits the catalog or disposes of its findings. This skill is
the human half: one interactive pass, in a fixed order, through everything
the server can report as needing attention.

**Interactive only.** Never run this unattended. If you are in a headless,
scheduled or "don't ask" context, stop and say this skill needs the owner.

## Ground rules

- Everything goes through the okf-mcp tools (`mcp__okf-mcp__*`), on the
  catalog the session names (a SessionStart hook names it; otherwise the
  connection's write target from `list_catalogs`). Pass `catalog` on every
  `maintenance_*` call: those tools have no default. The server is the
  source of truth; never read or write a git checkout of the catalog.
- If the okf-mcp tools are unavailable, stop and say so.
- Follow the catalog's AGENTS.md (`read_document` with path `AGENTS.md`)
  for where things go.
- Every write goes through a changeset (`open_changeset`), one per item, so
  the owner can revert any single fix.
- Pass `by: "claude-code/<model id>"` to `create_concept`, `stamp`, `review`
  and `split_concept`.
- Dates and times in prose come from `date -u +%Y-%m-%dT%H:%M:%SZ`.
- Large concepts stay in context once read: `search` for the spot and
  `get_concept` with `section` instead of reading whole hubs.
- Never change a real system in this pass. Read-only checks only (show,
  get, status, GET requests), and only where the catalog documents access.

## Pacing

Steps run in the order below. Each step starts by stating its count ("3
findings from 2 runs", "12 concepts over the sprawl threshold") and asks
**Go**, **Skip step** or **Quit**. Every question in the pass offers Quit,
as an option where there is room (AskUserQuestion allows four) and
otherwise as the answer "quit" under Other; it stops at once and prints the
summary. A step works through its whole
list unless the owner skips or quits.

Keep a tally as you go for the summary: per step, items handled, skipped
and remaining; each publish (message and revisions); dispositions by kind;
backlog entries added; problems.

## 0. Runner health

Page `maintenance_list_runs` until `next_after` is null and take the run
with the greatest `created_at` (the listing's order is not guaranteed).
Report its `created_at`, state and counts. Warn when:

- it started more than 26 hours ago;
- its state is not `completed`;
- `failed_count` is above 0;
- every work item was inconclusive (`inconclusive_count` equals
  `primary_count`).

No runs at all is also a warning. Report warnings; don't fix them. If the
owner asks to investigate, `search` the catalog for the runner's deployment
concept and follow what it says, recording any diagnosis or fix per
AGENTS.md.

## 1. Findings

**Collect.** Page `maintenance_list_findings` (`limit: 200`) until
`next_after` is null. Keep each finding whose `dispositions` list is empty,
or whose latest entry is `open` or `needs_input`. Sort by `created_at`,
oldest first. Announce the count and how many runs they came from.

**For each finding**, in order:

1. **Show it.** Category, assertion, follow-up, uncertainty, and each
   evidence excerpt with its pinned path and revision. If `outdated` is
   true, say a cited concept has changed since. `maintenance_get_finding`
   gives the full record and disposition history.
2. **Re-check the current text.** `outdated` says only that a cited concept
   changed, not that the issue is gone, and an old proposal never applies
   to newer text unexamined. Read the current text of every cited concept
   at the cited spot. If the issue is already resolved, the proposal is
   Explain, naming the revision that resolved it.
3. **Check the real world, when cheap.** If the finding is about live state
   (addresses, versions, configuration) and the catalog documents read-only
   access, check it. A few commands, not an investigation.
4. **Propose** exactly one of:
   - an edit: the path, the exact `old_str` and `new_str`, and provenance
     (`stamp` `generated`; `stamp` `verified` too if you checked it live;
     `add_source` for the live check);
   - a reason it isn't an issue;
   - a backlog entry, when the fix needs work on a real system or the
     owner's decision: what's wrong, why it matters, where it lives, a
     possible fix, the date, and a link to the concept with the detail.
5. **Ask** with AskUserQuestion, one finding per question, quoting the
   assertion and your proposal. Options: **Apply**, **Explain**,
   **Defer**, **Skip** (AskUserQuestion allows four). Say in the question
   that answering "quit" under Other ends the pass. Any other free-text
   answer revises the proposal; ask again.

**Carry out the choice:**

| Choice | Catalog | Then `maintenance_dispose` |
|---|---|---|
| Apply | `open_changeset`; the edits; `stamp`; `add_source`; `log` "… (okf-daily-maintenance, finding `<first 8 of id>`)"; `lint` with the handle; `publish` | `accepted_for_follow_up`, rationale "Applied: <what changed>; <path> rev <N>" |
| Explain | nothing | `dismissed`, rationale: the owner's reason, or your proposed one if they accepted it |
| Defer | `open_changeset`; add the entry to the matching backlog area concept (see step 7 for finding backlogs); link the affected concept to it; `log`; `publish` | `accepted_for_follow_up`, rationale "Deferred to <backlog path>: <one line>" |
| Skip | nothing | `needs_input`, rationale "Skipped in daily pass <YYYY-MM-DD>" |
| Quit | nothing | nothing; print the summary |

- Dispose only after `publish` succeeds.
- `idempotency_key`: `daily-<finding_id>-<YYYYMMDD>-<choice>`.
- On a publish Conflict, re-read the documents it names, redo the edit in a
  fresh changeset and publish once more. If that fails too, dispose
  `needs_input` with the error as rationale, `discard` the changeset, and
  move on.
- Two findings on the same concept are still handled one at a time; the
  second re-check sees the first publish.
- Every backlog entry added (Defer) is said explicitly in the summary.

## 2. Sprawl

Call `sprawl` with `n: 200`; keep the concepts above the threshold its
header prints. Backlog-type concepts are exempt. For the worst remaining
concept, invoke the `okf-reorganize` skill (Skill tool) with its path, and
tell it the mode is interactive. When it returns, call `sprawl` again and
take the next concept still above the threshold. Don't offer again, in
this pass, a concept the owner chose to leave.

## 3. Links

Call `links_report` with `mentions: false` and announce the counts (broken
links, orphans, dead ends, index drift). Then invoke `okf-link-check` once,
interactive.

## 4. Open questions

Call `open_questions`. For each concept with queued questions, invoke
`okf-review-oldest` with that concept's path and the instruction to go
through its open questions with the owner (interactive).

## 5. Oldest concept

Invoke `okf-review-oldest` once with no path (interactive).

## 6. Stale changesets

Call `list_changesets` with the catalog and keep those flagged idle. For
each, call `pending_changes` with its handle, summarize the diff in a few
lines, and ask **Publish**, **Discard**, **Leave**, **Quit**. Publish runs
`lint` with the handle first and uses a message that describes the diff.
Never discard without the owner's answer.

## 7. Backlog triage

Find the backlog concepts: `search` with an empty query and
`type: Backlog`, plus any concept AGENTS.md names as the place for
unfixed problems (often a short hub that links area concepts). For each
entry, check cheaply whether it has since been fixed: the linked
concept's current text, and the live system where access is documented.
If it looks fixed, ask **Remove** (delete the entry with `edit_document`,
`log` the fix), **Keep**, **Skip rest**, **Quit**. Never remove an entry
without the owner's answer.

## Errors

- A sibling skill is missing or fails: say so, ask whether to continue
  with the next step, and note it in the summary.
- Lint errors the pass didn't cause: don't block on them; list them in the
  summary.

## Summary

At the end, or on Quit, print:

- runner health, with any warnings;
- per step: handled, skipped, remaining;
- every publish, one line each (message and revisions);
- dispositions by kind (accepted, dismissed, needs input);
- every backlog entry added, by path;
- problems: failed publishes, missing skills, lint errors not caused here.
