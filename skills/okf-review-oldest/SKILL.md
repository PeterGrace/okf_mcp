---
name: okf-review-oldest
description: Re-check the least recently touched concept in an OKF knowledge catalog on the okf-mcp server against the real world, correct it if needed, and stamp it with the review date. Use when asked to "review the oldest concept", "verify the catalog", "check if the catalog is still accurate", "answer the catalog's open questions", or to do routine catalog freshness maintenance. Accepts an optional concept path to review that one instead.
---

# Review the oldest concept

Picks the concept whose knowledge was touched longest ago, investigates
whether it's still true, and records the result in its frontmatter. Each
run stamps the concept, so repeated runs rotate through the whole catalog.

Everything here goes through the okf-mcp tools (`mcp__okf-mcp__*`) on
the catalog the session names (a SessionStart hook names it; otherwise
use the connection's write target from `list_catalogs`). The server is the
source of truth. Never read or write a git checkout of the catalog, or run
its tooling: it is a frozen snapshot.

A catalog can cover much more than systems you can log in to. Many facts
come from the owner (decisions, preferences, history, things about people,
teams, accounts) and only the owner can confirm them.
How those are handled depends on the mode.

## Mode

- **Interactive** (default): the user invoked this skill or asked for a
  review in a conversation. You may ask the user questions.
- **Autonomous**: running unattended, from `/loop`, `/schedule`, a cron or
  headless `claude -p` run, a workflow, or the user said not to ask. Never
  ask questions or wait for answers; queue them in `open_questions` instead.

If unsure, treat it as interactive.

## Frontmatter this skill writes

OKF v0.2 (SPEC §5.2) already defines two of these; `reviewed` is a catalog
extension key, which OKF allows.

| Key | Meaning | When to write it |
|---|---|---|
| `reviewed` | The latest review by this skill: `by`, `at`, `outcome`, `note` | **Every run**, whatever the outcome. Replace the previous value; history lives in the server's revisions (`history`) and `log.md`. |
| `verified` | OKF: the content was confirmed against its sources | Only when the investigation actually confirmed the concept's key facts (outcomes `verified` and `updated`). Append `{ by, at }` to the list. |
| `generated` | OKF: when the content last meaningfully changed, and by whom | Only when you changed facts (outcome `updated`). |
| `open_questions` | Catalog extension: questions only the user can answer, as a list of quoted strings | Autonomous runs add them; whoever answers one removes it. Delete the key when the list is empty. |

Don't put review outcomes in `status`: OKF only allows `draft`, `stable` and
`deprecated` there. `reviewed.outcome`, the `open_questions` list and the
absence of a fresh `verified` entry are what mark a concept as unconfirmed.

Outcomes:

- **`verified`**: every key fact you could check matched.
- **`updated`**: you found something wrong or out of date and corrected it.
  The corrected content was checked, so it also gets a `verified` entry.
- **`partial`**: some key facts were confirmed, others couldn't be checked.
  No `verified` entry, since `verified` vouches for the whole concept. The
  note lists what was and wasn't checked.
- **`unverifiable`**: the key facts couldn't be checked at all. No
  `verified` entry; the note says why and what access would allow it.

Format (use a block mapping, and quote the note):

```yaml
reviewed:
  by: claude-code/claude-opus-5-5
  at: 2026-10-24T18:05:00Z
  outcome: partial
  note: "Confirmed the build server's runner version and queue live; couldn't reach the vendor status page to check the support contract."
```

Write all of these with the okf-mcp tools, never by editing frontmatter
text: the server stamps every `at` from its own clock and edits only the
key it writes. Every write takes the `changeset` handle from step 1.
Always pass `by: "claude-code/<model-id>"` to `stamp` and `review` (the
default is the API key's actor, which may name a person), and
`by: "human:<owner>"` only for confirmations the owner gave.

| Write | Tool |
|---|---|
| `reviewed` (+ queued questions) | `review` (`path`, `outcome`, `note`, optional `questions`) |
| `verified` entry | `stamp` with `field: verified` (optionally `by: human:<owner>`) |
| `generated` | `stamp` with `field: generated` |
| `sources` entry | `add_source` (`id` such as `live-20261024`, `resource`, `title`, `author`) |
| Remove an answered question | `unquestion` (`question`: its 1-based number or exact text) |

## 1. Open a changeset and pick

Call `open_changeset` and keep the handle it returns for every write
below. If you lose it, `list_changesets` finds it again.

Then call `oldest`. It lists the five least recently touched concepts:
the latest of `generated.at`, `verified[].at` and `reviewed.at`; ties
broken by path; deprecated concepts skipped; a stale flag for a passed
`stale_after`; and a count of queued `open_questions`. Take the first one
unless the user named a concept.

In interactive mode, also call `open_questions` (it numbers each
concept's questions, for `unquestion`). If any concept has queued
questions, offer to go through those before (or instead of) the oldest
concept. They're the cheapest way to raise the catalog's trust.

## 2. Investigate (read-only)

1. Read the whole concept (`get_concept` with `links: true`), its
   `sources`, and the concepts it links to.
   The catalog itself often says how to reach a system (an Access row, a
   kubectl context, an API).
2. Pick out the **key facts**: identity, how it's reached, addresses,
   versions, configuration, ownership, current status, and any "not yet
   applied" or "as of" claim. Historical narrative ("on 2026-09-28 X
   happened") can't go stale; don't try to re-prove it.
3. Check each key fact against the live source with **read-only** commands:
   `ssh <host>` with show/cat/ip/systemctl status, `kubectl get/describe`,
   `vtysh -c 'show ...'`, public APIs with curl, `gh api` GETs, reading the
   source repo, MCP read tools.
4. **Never change a real system during a review**, even to fix something
   obvious. If the system is wrong rather than the doc, record what you
   found in the concept and tell the user; fixing is a separate, explicit
   task.

Keep it proportionate: a few minutes per concept, not a new investigation.

## 3. When it can't be validated

Sort each fact you couldn't confirm into one of these:

- **Only the user knows** (the source is `human:` or a conversation, or
  it's a decision, preference, plan, history, or anything about people,
  places or accounts with no system to check).
- **Couldn't reach it** (host unreachable, no credentials, MCP server down).
  Try the obvious alternative routes first, e.g. another host, a public API,
  or a read-only path the catalog documents.
- **Looked and it's gone or different** (host decommissioned, URL 404s,
  config changed). That's a finding, not a gap: outcome `updated`. Correct
  the concept. If the thing no longer exists, set `status: deprecated` and
  say in the body what replaced it, if anything.

Then, depending on the mode:

**Interactive.** Stop and ask the user before writing anything. Use
AskUserQuestion, up to four questions per call. Quote each claim and offer
"Still true", "Changed" (they explain), "No longer applies" and "Don't
know". Also ask about unreachable facts if the user might know, or might
grant access. Apply the answers:

- Confirmed: it counts as checked. Add a `verified` entry for
  `human:<owner>` (a person really confirmed it), plus one for yourself if
  you machine-checked the rest. Add a `sources` entry such as
  `{ id: review-20261024, resource: "Conversation with the owner, 2026-10-24", author: human:<owner> }`.
- Changed or no longer applies: correct the concept, outcome `updated`.
- Don't know: leave the claim alone and mention it in the note. If it's
  important, mark it as unconfirmed in the body.

Also answer any existing `open_questions` on the concept the same way.

**Autonomous.** Don't ask. Add a self-contained question for each fact you
couldn't confirm to `open_questions`, then set the outcome to `partial` or
`unverifiable`, with a note that says the questions are queued. Don't
change those facts and don't add `verified`. Don't duplicate a question
that's already queued. Don't deprecate or delete anything just because it
couldn't be confirmed.

```yaml
open_questions:
  - "Is the team still on the annual support contract with the CI vendor? (okf-review-oldest, 2026-10-24)"
```

Either way, stamping `reviewed` is deliberate. It moves the concept to the
back of the queue so one blocked concept doesn't stall every future run,
and the outcome and note keep it honest. Questions queued by autonomous
runs are answered later, from `open_questions`, in an interactive
session.

## 4. Write it up

1. Update the frontmatter per the table above, using the tools. If you
   corrected facts:
   - Edit the body with `edit_document` (or `replace_body`), dating
     changed claims where it helps ("as of 2026-10-24", from
     `date -u +%Y-%m-%d`).
   - `add_source` for the live check (e.g. `id: live-20261024`, with what
     you ran as `resource`).
   - `stamp` `generated`.
   - If you changed `description` (`set_frontmatter`), update the listing
     in the folder's `index.md` with `edit_document`.
   - If `stale_after` is set and has passed, or you re-verified the
     concept, move it forward by the span it covered before
     (`set_frontmatter`).
2. `log` an entry, e.g. "Reviewed `/infra/build-server.md` (outcome:
   partial): ...".
3. Check with `pending_changes`, then `publish` with a message such as
   "Review <concept>: <outcome>". If it reports lint errors you didn't
   cause, stop and tell the user. A refused publish leaves the changeset
   open: on a `Conflict`, `discard` it, re-read the documents named and
   redo the work in a fresh changeset; if that fails too, `discard` the
   retry, then stop and tell the user.
4. Report to the user: which concept, the outcome, what changed, what
   couldn't be checked and why, any questions queued, and any problems
   found in the real system.
