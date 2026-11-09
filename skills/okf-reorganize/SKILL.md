---
name: okf-reorganize
description: Reorganize a sprawling concept in an OKF knowledge catalog on the okf-mcp server for progressive disclosure, so hub concepts stay short and link out to incident, playbook and child-system concepts. Use when asked to "reorganize the catalog", "split a concept", "this concept is too long", "tidy up build-server.md", when `lint` shows a `sprawl` warning, or for routine catalog structure maintenance. Accepts an optional concept path to work on that one instead of the worst.
---

# Reorganize a sprawling concept

Hub concepts (a cluster, a network, a service) collect things that aren't
about the hub: an incident investigated on it, the fix procedure that came
out of it, a change log of what an agent did, a status snapshot from one
afternoon, another system that runs on it. Each addition is reasonable;
together they bury the thirty lines that say what the thing *is*. This skill
moves each of those to where it belongs, so a reader gets the hub in a
screen and follows links for the rest.

## Mode

- **Interactive** (default): the user invoked this skill or asked for a
  reorganization in a conversation. Propose, ask, then execute only what
  the owner approves.
- **Autonomous**: running unattended (`/loop`, `/schedule`, cron, headless
  `claude -p`, a workflow, or the user said not to ask). **Never
  restructure.** Moving and splitting change what other sessions read and
  where they write, so they need the owner's approval. Only record the
  proposal (step 5).

If unsure, treat it as interactive.

## 0. Tools and changeset

Everything here goes through the okf-mcp tools (`mcp__okf-mcp__*`) on
the catalog the session names (a SessionStart hook names it; otherwise
use the connection's write target from `list_catalogs`). The server is the
source of truth. Never read or write a git checkout of the catalog, or run
its tooling: it is a frozen snapshot.

Before the first write, call `open_changeset` and keep the handle it
returns; pass it as `changeset` to every write, and to the reports so they
see your drafts. If you lose it, `list_changesets` finds it again. Keep one
changeset per reorganization, so the owner can review it as one publish.

## 1. Pick the concept

`sprawl` gives the top 5 (`n` for more); `sprawl` with `path` scores one
concept.

Each row shows the score and its parts: non-blank body lines, headings,
sources, volatile headings or `**Lead-in.**` paragraphs (as of, current
state, outage, change log, open items, trap, fix, stuck, ...) and distinct
dates. Under it are the split candidates: volatile sections, sections
holding a fifth of the body, and volatile lead-in paragraphs, with line
numbers. `lint` warns with rule `sprawl` above a fixed cutoff, which its
message prints ("sprawl score 17.8 > 16"); the `sprawl` result itself
has no cutoff field.
Concepts of type `Backlog` (e.g. `/projects/backlog.md`) grow by
design and are exempt: the ranking skips them, and naming one shows its
score marked exempt. Don't reorganize them with this skill.

Take the top concept unless the user named one. The score only points;
read the whole concept before deciding anything.

## 2. Classify every section

Read the concept, the concepts it links to and the ones linking to it
(`get_concept` with `links: true` lists both). For each section,
and each long lead-in paragraph inside a section, decide what it really is:

| Section is really… | Where it goes |
|---|---|
| Another system that runs on this one | its own concept; hub keeps one line + link |
| An incident/investigation | incident concept or the affected system's concept |
| A fix procedure (check/fix steps) | a playbooks/ concept |
| A change-log entry | log.md + git; concept keeps only current truth (one dated line at most) |
| A dated status snapshot | delete if stale, or move to the system's concept with stale_after |

Anything else that describes what the thing is, how it's built, how it's
reached or how it behaves stays in the hub. So do short facts (one or two
lines) even when they're dated.

Before proposing a new concept, check whether a home already exists: an
existing concept for the affected system, an existing playbook, an
existing `incidents/` or `playbooks/` folder near the hub (for example
`infra/playbooks/`). Prefer extending one over
creating a near-duplicate. Match the neighbours' naming and `type`s
(`Playbook`, `Incident`, `System`, ...).

A section often mixes kinds. A typical incident section holds the
incident narrative, a check/fix procedure and a change log; plan each part
separately (the incident goes to an incident concept, the procedure to a
playbook that the incident links to, the change log goes away).

## 3. Write the plan

For each move, give: the section (heading and line range), its kind, the
destination (new path, or existing concept), the new concept's `type`,
title and one-sentence description, which of the parent's sources it
needs, and the one line the hub keeps. Also list:

- deletions (stale snapshots, change logs whose facts are already in
  `log.md` or the revisions; check `log.md` (`read_document`) and the
  concept's `history` first, and
  keep a single dated line in the hub if the change is still relevant);
- inbound links that will be rewritten (the tools do this; listing them
  lets the owner see the blast radius);
- anything you'd leave alone, and why.

## 4. Interactive: ask, then execute what's approved

Present the plan with AskUserQuestion: one question per move (up to four
per call; more calls if needed), each quoting the section and the proposed
destination, with options "Do it", "Different destination" (they explain),
"Leave it" and "Delete instead" where deletion makes sense. Execute only
the approved moves.

Tools:

- `split_concept`: `path`, `section`, `to`, `type`, `title`,
  `description`, optional `stub` (the hub line) and `sources` (parent
  source ids), and always `by: "claude-code/<model-id>"` (as for `stamp`).
- `move_concept`: `old`, `new`.

`split_concept` moves the section's content (subsections included) into a new
concept, drops its heading (it becomes the title) and re-levels its
subsections to start at `#`. The new concept gets `generated` now, the
parent's `status`, and sources: the `--source` ids if given, else those the
section's footnotes cite, else all of the parent's (prune them by hand;
`the tool's notes say which rule it used). It never copies `verified`: splitting
isn't verification. The parent keeps the heading with a stub paragraph
(`--stub`, or the link plus `--description`). Footnotes go where they're
cited, links to `PARENT#section` and its subsections are rewritten across
the bundle, and the new concept is listed in its folder's `index.md`
(created, and listed in the parent index, for a new folder).

`move_concept` renames or moves a concept, rewrites every link to it
(anchors kept), and moves its `index.md` listing.

Both return the paths they touched, all held in the changeset. Then
finish with the edit tools:

1. **Prose.** Rewrite the new concept's opening line ("Split out of …") into
   a real explanation of what it is and how it relates to the hub. Trim a
   section that ran to the end of the document (`split_concept` warns) of closing
   paragraphs that belong to the hub, and move them back (`edit_document`,
   `replace_body`). Merge a stub
   into the surrounding hub prose if a heading for one line reads badly.
   Replace any `TODO` lines in newly created `index.md` files.
2. **Splitting a section further.** To split part of a section (say, the fix
   procedure inside an incident), first give it its own heading in the new
   concept, then `split_concept` again from there.
3. **Deletions.** Remove change-log paragraphs whose facts are in `log.md`
   and git; leave one dated line in the hub if the change still matters
   (e.g. "Since 2026-10-24, the build server alerts to chat").
4. **Frontmatter.** Add `stale_after` to a moved status snapshot and set
   `type` (`set_frontmatter`). Pruning copied `sources` has no tool yet
   (the server allows only `add_source`), so pass `sources` to
   `split_concept` to copy only the ones the new concept needs, and leave
   unused hub sources in place, listing them in the report. Stamp
   `generated` on the hub (`stamp`), since its content changed. Don't add
   `verified`.
5. **Check.** `links_report` (no broken links or anchors; link the new
   concepts generously), `lint` and `sprawl` with the hub's `path`, all
   with the handle, to see the score drop.
6. **Record.** `log` (kind `Change`): "Reorganized `/infra/x.md`: moved
   … to …, … (okf-reorganize)."
7. **Publish.** Show the owner `pending_changes` if the plan changed along
   the way, then `publish` with "Reorganize <hub>: <what moved where>". A
   refused publish leaves the changeset open: on a `Conflict`, `discard`
   it, re-read the documents named, and redo the work in a fresh
   changeset. If that doesn't clear it, `discard` the retry too, then stop
   and tell the user.

## 5. Autonomous: record the proposal only

Don't call `split_concept` or `move_concept`, and don't edit prose. Queue
the plan on the concept as one self-contained open question per move, with
`question`, without `review` (nothing was re-checked):

> "Reorganize proposal (okf-reorganize, <YYYY-MM-DD>): split '# Stuck deploys on the build server' (L112-176, an incident with a fix procedure and a change log) into /infra/incidents/build-server-stuck-deploys.md and /infra/playbooks/build-server-stuck-deploy.md; drop the change log (in log.md). Approve?"

Take the date from `date -u +%Y-%m-%d`. Skip a proposal already queued.
Then `log` "... (okf-reorganize, proposal only)" and `publish`. An
interactive session answers the questions later (`open_questions`), runs
this skill on the concept, and removes each answered question with
`unquestion`.

## 6. Report

Which concept, its sprawl score before and after, each move made (or
proposed) with its destination, the deletions, files touched, the publish,
and anything left for the owner (TODOs in new indexes, unpruned sources,
questions queued).
