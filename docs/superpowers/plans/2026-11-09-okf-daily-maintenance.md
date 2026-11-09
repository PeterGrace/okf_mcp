# Daily catalog maintenance skills Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship four Claude Code skills under `skills/` (a new `okf-daily-maintenance` conductor plus generic `okf-reorganize`, `okf-link-check` and `okf-review-oldest`), with a test that every tool and argument name they cite exists on the server.

**Architecture:** Skills are markdown files with YAML frontmatter. The conductor delegates steps to its siblings with the Skill tool. A Rust integration test starts a real server (as `tests/maintenance.rs` does), lists its tools, and checks every backticked snake_case name in the skills against tool names, input-schema property names, enum/const values and a short list of known output fields.

**Tech Stack:** Markdown skills; Rust integration test (`tokio`, `rmcp` client, `regex`, `serde_json`, testcontainers Postgres via `tests/support`).

**Spec:** `docs/superpowers/specs/2026-11-09-okf-daily-maintenance-design.md`

**Privacy rule (public repository):** no file or commit message may name a particular catalog, owner, host, domain, cluster, registry, repository or IP address. Use "the catalog", "the owner", and placeholders such as `/infra/build-server.md` and `example.internal`. Task 6 checks this with a term list kept outside the repository.

---

## File structure

| File | Responsibility |
|---|---|
| `tests/skills.rs` | Tool-contract test over `skills/*/SKILL.md` |
| `skills/README.md` | What each skill does; how to install |
| `skills/okf-reorganize/SKILL.md` | Generic copy of the local skill |
| `skills/okf-link-check/SKILL.md` | Generic copy of the local skill |
| `skills/okf-review-oldest/SKILL.md` | Generic copy of the local skill |
| `skills/okf-daily-maintenance/SKILL.md` | The conductor (new) |
| `README.md` | One line pointing to `skills/` |

---

### Task 1: Tool-contract test (failing)

**Files:**
- Create: `tests/skills.rs`

- [ ] **Step 1: Write the test**

```rust
//! The skills under `skills/` cite the server's tools and their arguments in
//! backticks. This test starts a server, lists its tools, and checks that
//! every backticked snake_case name in a skill is a tool name, an input-schema
//! property, an enum or const value, or one of the known output fields below,
//! so a renamed or misspelled tool fails here rather than in a session.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use okf_mcp::config::Config;
use regex::Regex;
use serde_json::Value;
use support::server::{files, TestServer};

/// Names the skills cite that are not input-schema names: fields of tool
/// results and catalog frontmatter keys.
const KNOWN: &[&str] = &[
    "next_after",
    "created_at",
    "ended_at",
    "primary_count",
    "completed_count",
    "failed_count",
    "inconclusive_count",
    "follow_up",
    "finding_ids",
    "stale_after",
];

/// Adds every property name, enum value and const value in `schema`.
fn schema_names(schema: &Value, out: &mut BTreeSet<String>) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                out.extend(properties.keys().cloned());
            }
            if let Some(Value::Array(values)) = map.get("enum") {
                out.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            if let Some(Value::String(value)) = map.get("const") {
                out.insert(value.clone());
            }
            map.values().for_each(|value| schema_names(value, out));
        }
        Value::Array(items) => items.iter().for_each(|item| schema_names(item, out)),
        _ => {}
    }
}

/// `skills/<name>/SKILL.md` for every skill directory, sorted.
fn skill_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("directory entry").path().join("SKILL.md"))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_cite_only_real_tools_and_arguments() {
    let skills = skill_files();
    assert_eq!(skills.len(), 4, "expected four skills, found {skills:?}");

    let server = TestServer::start(vec![("main", files(&[]))], Config::default()).await;
    let key = server.reporter_key("skills", &["main"]).await;
    let client = server.client(&key, None).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();

    let mut valid: BTreeSet<String> = KNOWN.iter().map(|name| name.to_string()).collect();
    for tool in &tools {
        valid.insert(tool.name.to_string());
        schema_names(&Value::Object((*tool.input_schema).clone()), &mut valid);
    }

    let cited = Regex::new(r"`(?:mcp__okf-mcp__)?([a-z][a-z0-9]*(?:_[a-z0-9]+)+)`").unwrap();
    let mut unknown = Vec::new();
    for path in &skills {
        let text = std::fs::read_to_string(path).unwrap();
        for capture in cited.captures_iter(&text) {
            if !valid.contains(&capture[1]) {
                unknown.push(format!("{}: {}", path.display(), &capture[1]));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "names that are not tools, arguments or known fields:\n{}",
        unknown.join("\n")
    );
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test --test skills`
Expected: FAIL, panic `read .../skills: No such file or directory`.

- [ ] **Step 3: Commit**

```bash
git add tests/skills.rs
git commit -m "test: check skills cite only real okf-mcp tools and arguments"
```

---

### Task 2: Generic copies of the three existing skills

**Files:**
- Create: `skills/okf-reorganize/SKILL.md` (from `~/.claude/skills/okf-reorganize/SKILL.md`)
- Create: `skills/okf-link-check/SKILL.md` (from `~/.claude/skills/okf-link-check/SKILL.md`)
- Create: `skills/okf-review-oldest/SKILL.md` (from `~/.claude/skills/okf-review-oldest/SKILL.md`)

- [ ] **Step 1: Copy the three files**

```bash
mkdir -p skills/okf-reorganize skills/okf-link-check skills/okf-review-oldest
for s in okf-reorganize okf-link-check okf-review-oldest; do
  cp ~/.claude/skills/$s/SKILL.md skills/$s/SKILL.md
done
```

- [ ] **Step 2: Genericize, keeping every procedure unchanged**

Apply these rewrites in each file (Edit tool, one at a time, reading the
surrounding sentence so it still reads well):

1. In `description:` frontmatter, "the OKF knowledge catalog (catalog `<name>`
   on the okf-mcp server)" becomes "an OKF knowledge catalog on the okf-mcp
   server".
2. Any sentence naming a specific catalog ("on catalog `<name>`", "the catalog
   is `<name>`") becomes "on the catalog the session names (a SessionStart
   hook names it; otherwise use the connection's write target from
   `list_catalogs`)". State this once per file, in the tools paragraph.
3. Any sentence warning about one specific git checkout becomes: "The server
   is the source of truth. Never read or write a git checkout of the catalog,
   or run its tooling: it is a frozen snapshot."
4. The owner's name becomes "the owner" (and "<name>'s approval" style
   phrases become "the owner's approval"). `by: "human:<name>"` examples
   become `by: "human:<owner>"`.
5. Example concepts and hosts that name real systems become placeholders:
   network/router examples become `/infra/build-server.md`-style examples;
   hostnames become `host.example.internal`. Keep the example's shape.
6. Leave model ids such as `claude-code/claude-opus-5-5` as they are (public
   product names).

- [ ] **Step 3: Check nothing private remains**

First build the private term list exactly as Task 6 Step 1 describes (it is
reused there). Then run:
`grep -n -i -f "$OKF_PRIVATE_TERMS" skills/*/SKILL.md || echo clean`
Expected: `clean`.

- [ ] **Step 4: Commit**

```bash
git add skills/okf-reorganize skills/okf-link-check skills/okf-review-oldest
git commit -m "feat(skills): publish generic okf-reorganize, okf-link-check, okf-review-oldest"
```

---

### Task 3: The conductor skill

**Files:**
- Create: `skills/okf-daily-maintenance/SKILL.md`

- [ ] **Step 1: Write the skill**

````markdown
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
````

- [ ] **Step 2: Run the contract test**

Run: `cargo test --test skills`
Expected: PASS. If it reports an unknown name that is a real result field
or frontmatter key, add it to `KNOWN` in `tests/skills.rs` and rerun; if it
is a misspelled tool or argument, fix the skill and rerun.

- [ ] **Step 3: Commit**

```bash
git add skills/okf-daily-maintenance tests/skills.rs
git commit -m "feat(skills): add okf-daily-maintenance conductor skill"
```

---

### Task 4: Skills README and root README pointer

**Files:**
- Create: `skills/README.md`
- Modify: `README.md` (add one line in its documentation list or at the end)

- [ ] **Step 1: Write `skills/README.md`**

```markdown
# Claude Code skills for okf-mcp catalogs

Interactive skills for keeping an OKF catalog healthy through the okf-mcp
server's tools. They work on whichever catalog the session names (a
SessionStart hook, or the connection's write target).

| Skill | What it does |
|---|---|
| `okf-daily-maintenance` | The daily pass: runner health, then the maintenance findings one at a time, then sprawl, links, open questions, the oldest concept, stale changesets and backlog entries. Delegates to the three below. |
| `okf-reorganize` | Splits a sprawling concept into hub, incident, playbook and child-system concepts. |
| `okf-link-check` | Fixes broken links, orphans, dead ends and index drift. |
| `okf-review-oldest` | Re-checks the least recently touched concept against the real world, and answers queued open questions. |

## Install

Symlink each skill into your Claude Code skills directory:

    for s in skills/okf-*; do ln -sfn "$PWD/$s" ~/.claude/skills/"$(basename "$s")"; done

Then ask Claude Code for "daily maintenance", or invoke
`/okf-daily-maintenance`.

`cargo test --test skills` checks that every tool and argument the skills
cite exists on the server.
```

- [ ] **Step 2: Add the root README pointer**

Find the README's list of docs (`grep -n "docs/" README.md`) and add next to
it, or append at the end if there is none:

```markdown
- [`skills/`](skills/README.md): Claude Code skills for daily catalog maintenance.
```

- [ ] **Step 3: Commit**

```bash
git add skills/README.md README.md
git commit -m "docs: describe the skills directory"
```

---

### Task 5: Repository validation

- [ ] **Step 1: Run the repository's checks**

Run: `just fmt && just lint && cargo test --test skills`
Expected: all pass. (`just test` runs the whole suite; run it if time
allows, and report any failure unrelated to this change rather than fixing
it.)

- [ ] **Step 2: Commit any formatting fix**

```bash
git add -A tests/skills.rs && git commit -m "style: rustfmt tests/skills.rs" || true
```

---

### Task 6: Privacy check

- [ ] **Step 1: Build the private term list outside the repository**

Create `$OKF_PRIVATE_TERMS` (a file in a scratch directory, never under
the repository) with one case-insensitive pattern per line: the catalog
names, the owner's name and handles, and every hostname, domain, cluster
and repository name that appears in `~/.claude/skills/okf-*/SKILL.md` or
in the owner's catalog setup. Do not commit it or paste its contents into
any repository file or commit message.

- [ ] **Step 2: Grep everything this branch adds**

Run:
`git diff --name-only origin/main...HEAD | xargs grep -n -i -f "$OKF_PRIVATE_TERMS" || echo clean`
and
`git log origin/main..HEAD --format=%B | grep -n -i -f "$OKF_PRIVATE_TERMS" || echo clean`
Expected: `clean` twice. Fix and amend/fixup any hit.

---

### Task 7: Ship and install locally

- [ ] **Step 1: Push and open the PR**

```bash
git push -u origin feat/okf-daily-maintenance-skill
gh pr create --title "Add daily catalog maintenance skills" --body-file <(cat <<'EOF'
Adds `skills/` with four Claude Code skills: a new interactive
`okf-daily-maintenance` conductor (runner health, findings one at a time,
then sprawl, links, open questions, oldest concept, stale changesets and
backlog triage) and generic `okf-reorganize`, `okf-link-check` and
`okf-review-oldest`, which it delegates to.

`tests/skills.rs` starts a server and checks every tool, argument and enum
value the skills cite exists.

Spec: docs/superpowers/specs/2026-11-09-okf-daily-maintenance-design.md
Plan: docs/superpowers/plans/2026-11-09-okf-daily-maintenance.md

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)
```

- [ ] **Step 2: Replace the local skills with symlinks**

```bash
for s in okf-reorganize okf-link-check okf-review-oldest; do
  mv ~/.claude/skills/$s ~/.claude/skills/$s.local-bak
done
for s in skills/okf-*; do ln -sfn "$PWD/$s" ~/.claude/skills/"$(basename "$s")"; done
ls -l ~/.claude/skills | grep okf-
```

Expected: four `okf-*` symlinks into the repository, and three
`*.local-bak` directories. The session's SessionStart hook still names the
catalog, so the generic skills find it.

- [ ] **Step 3: Smoke run**

Invoke `okf-daily-maintenance` in a session. Expected: step 0 reports the
latest run; step 1 reports its count (zero is fine) and moves on; Quit at
step 2 prints the summary.
