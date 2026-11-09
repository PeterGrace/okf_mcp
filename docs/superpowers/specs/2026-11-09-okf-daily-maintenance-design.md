# Daily catalog maintenance skill

## Intent

The report-only maintenance runner investigates concepts on a schedule and files
findings, but it never edits canonical content or disposes of its own findings. A
human-in-the-loop pass is needed to turn findings into catalog edits, and the
catalog has other routine upkeep (sprawl, links, open questions, freshness, stale
changesets, backlog entries) that the server can report but nothing works through.

Ship a set of Claude Code skills in this repository, under `skills/`, that a
catalog owner runs once a day in an interactive session:

- `okf-daily-maintenance`: a new conductor skill that walks the runner's findings
  one at a time, then works through the other upkeep reports.
- `okf-reorganize`, `okf-link-check`, `okf-review-oldest`: existing single-purpose
  skills, published here in generic form, which the conductor delegates to.

## Decisions

- **Interactive only.** The scheduled runner is the unattended half; this skill is
  the human half. It never runs headless and has no autonomous mode. (The sibling
  skills keep their existing autonomous modes for other uses.)
- **Everything, every time.** Each step works through its whole report. Every step
  announces its count first and can be skipped with one answer; Quit at any prompt
  ends the pass with a summary.
- **Delegate, don't duplicate.** Steps covered by a sibling skill invoke it with the
  Skill tool, so each procedure lives in exactly one file.
- **Public repository.** No skill or document names a particular catalog, owner,
  host, domain, cluster or private path. The catalog is the one the session names
  (a SessionStart hook, or the connection's write target from `list_catalogs`); the
  owner is "the owner". Examples use placeholder paths such as
  `/infra/build-server.md` and `example.internal`.

## Layout

```
skills/
  README.md                        what each skill does; install by symlinking into ~/.claude/skills
  okf-daily-maintenance/SKILL.md   conductor (new)
  okf-reorganize/SKILL.md          generic copy
  okf-link-check/SKILL.md          generic copy
  okf-review-oldest/SKILL.md       generic copy
```

### Genericizing the existing skills

Procedures are unchanged. Only owner-specific text changes:

- "catalog `<name>`" becomes "the catalog the session names" (SessionStart hook, else
  the connection's write target).
- The owner's name becomes "the owner"; `by: "human:<owner>"` stays as a pattern.
- The warning about one particular git checkout becomes a general rule: the server
  is the source of truth; never read or write a git checkout of the catalog.
- Example concept paths and hostnames become placeholders.

## The daily pass

Steps run in this order. Each begins by stating its count and offering Skip.

| # | Step | Server tools | Handled by |
|---|---|---|---|
| 0 | Runner health | `maintenance_list_runs` | conductor |
| 1 | Findings | `maintenance_list_findings`, `maintenance_get_finding`, `maintenance_dispose` | conductor |
| 2 | Sprawl | `sprawl` | `okf-reorganize`, once per concept |
| 3 | Links | `links_report` | `okf-link-check`, once |
| 4 | Open questions | `open_questions` | `okf-review-oldest`, once per concept with questions |
| 5 | Oldest concept | `oldest` | `okf-review-oldest`, once |
| 6 | Stale changesets | `list_changesets`, `pending_changes`, `publish`, `discard` | conductor |
| 7 | Backlog triage | `search` (`type: Backlog`), `get_concept`, `edit_document`, `log` | conductor |

At the start, if the okf-mcp tools are unavailable, stop and say so.

### Step 0: runner health

Page `maintenance_list_runs` to the end and take the run with the greatest
`created_at` (the listing's order is not part of its contract). Report its `created_at`,
`state` and counts. Warn when any holds:

- the latest run started more than 26 hours ago (time from `date -u`);
- `state` is not `completed`;
- `failed_count` > 0;
- every work item ended `inconclusive` (`inconclusive_count` equals `primary_count`).

A warning is reported, not fixed. To investigate, `search` the catalog for the
runner's deployment concept and follow what it says; record any diagnosis or fix
per the catalog's conventions (its AGENTS.md). Investigate only if the owner asks.

### Step 1: findings

**Collect.** Page `maintenance_list_findings` (`limit: 200`) until `next_after` is
null. Keep findings with no disposition or whose latest disposition is `open` or
`needs_input`. Order by `created_at`, oldest first. Announce "N findings from M runs".

**For each finding:**

1. **Show** category, assertion, `follow_up`, `uncertainty`, and each evidence
   excerpt with its pinned path and revision. Note `outdated: true` if set.
2. **Re-check current text.** `outdated` only means a cited concept changed. Always
   read the current text of each cited concept, using `search` for the spot and
   `get_concept` with `section` rather than full reads of large concepts. An old
   proposal never applies to newer text unexamined. If the issue is gone, the
   proposal is Explain with a rationale naming the revision that resolved it.
3. **Live check, when cheap.** If the finding concerns real-world state and the
   catalog documents read-only access, check it (show/get/GET commands only).
   Never change a real system in this step.
4. **Propose** one of: an exact edit (path, `old_str` → `new_str`, plus
   `stamp`/`add_source` provenance); a reason it is not an issue; or a backlog
   entry (what's wrong, why it matters, where, a possible fix, the date, a link).
5. **Ask** with AskUserQuestion, one finding per prompt: **Apply**, **Explain**,
   **Defer**, **Skip**, **Quit**. A free-text answer revises the proposal and the
   finding is asked again.

**Execute:**

| Choice | Catalog | `maintenance_dispose` |
|---|---|---|
| Apply | New changeset: edits, `stamp generated` (and `verified` if checked live), `add_source`, `log` "(okf-daily-maintenance, finding `<id8>`)", `lint`, `publish` | `accepted_for_follow_up`, "Applied: `<what>`; `<path>` rev N" |
| Explain | none | `dismissed`, the owner's reason or the proposed one |
| Defer | New changeset: add the entry to the matching backlog area concept, link the affected concept to it, `log`, `publish` | `accepted_for_follow_up`, "Deferred to `<backlog path>`" |
| Skip | none | `needs_input`, "Skipped in daily pass `<YYYY-MM-DD>`" |
| Quit | none | none; go to the summary |

Rules:

- One changeset per finding, so each fix can be reverted alone.
- Dispose only after `publish` succeeds.
- `idempotency_key`: `daily-<finding_id>-<YYYYMMDD>-<choice>`.
- `by: "claude-code/<model id>"` on every `stamp`, `review`, `create_concept` and
  `split_concept`.
- On a publish Conflict, re-read the named documents, redo the edit in a fresh
  changeset and publish once more; if that fails, dispose `needs_input` with the
  error and continue.
- Findings that touch the same concept are still handled one at a time; each
  re-check sees the previous publish.
- Dates in prose and keys come from `date -u`.

### Step 2: sprawl

Call `sprawl` (`n` large enough to cover every concept over the threshold printed in
its header). Backlog-type concepts are exempt. For the worst remaining concept,
invoke `okf-reorganize` with its path, stating that the mode is interactive. After
it returns, re-run `sprawl` and take the next concept still over the threshold.
Concepts the owner chose to leave are not offered again in this pass.

### Step 3: links

Invoke `okf-link-check` once (interactive). Announce the counts from
`links_report` (broken links, orphans, dead ends, index drift) first.

### Step 4: open questions

Call `open_questions`. For each concept with queued questions, invoke
`okf-review-oldest` with that concept's path and the instruction to answer its
open questions with the owner.

### Step 5: oldest concept

Invoke `okf-review-oldest` once with no path.

### Step 6: stale changesets

`list_changesets` for the catalog; keep those flagged idle. For each: show
`pending_changes` summarised, then ask **Publish**, **Discard**, **Leave**. Publish
runs `lint` first and uses a message describing the diff. Never discard without
the owner's answer.

### Step 7: backlog triage

Find the backlog concepts: `search` with `type: Backlog`, plus any the catalog's
AGENTS.md names. For each entry, make a cheap read-only check of whether it has
been fixed (the linked concept's current text; the live system when documented).
Propose **Remove** (edit plus `log`) when it looks fixed, otherwise **Keep**;
**Skip rest** moves on. Never remove an entry without the owner's answer.

## Errors

- okf-mcp unavailable: stop before step 0 and say so.
- A sibling skill is missing or fails: report it, ask whether to continue with the
  next step, and record it in the summary.
- Lint errors the pass did not cause: don't block; list them in the summary.
- Quit at any prompt: stop and print the summary.

## Summary

Printed at the end or on Quit:

- runner health result;
- per step: items handled, skipped and remaining;
- every publish (message and resulting revisions), one line each;
- dispositions by kind;
- every backlog entry added, stated explicitly;
- problems: failed publishes, missing skills, lint errors not caused by the pass.

## Testing

Skills are prose; verification is behavioural (see `superpowers:writing-skills`).

1. **Tool contract.** Every tool and argument the skills name exists in the
   server's tool definitions with that spelling.
2. **Empty findings path.** Run the pass when every finding is already disposed;
   step 1 reports zero and moves on.
3. **Full pass.** Run steps 2-7 against a catalog with real sprawl, link and
   question items.
4. **One finding end to end** after the next scheduled run files findings: Apply,
   then confirm the disposition and the publish.
5. **Privacy.** Before committing, grep `skills/` and this spec for catalog names,
   personal names, hostnames, domains and private paths; none may appear.

## Out of scope

- An autonomous or scheduled mode for the conductor.
- Changes to the runner, the server, or its tools.
- Applying proposals without the owner's per-item answer.
