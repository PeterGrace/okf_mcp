---
name: okf-link-check
description: Find and fix missing links between concepts in an OKF knowledge catalog on the okf-mcp server. Use after adding or editing concepts, when asked to "check links", "link the catalog", "find related documents", "find orphans", or whenever the catalog's graph needs tidying.
---

# OKF link check

The catalog's value is in its graph (see its `AGENTS.md`, via
`read_document` with path `AGENTS.md`). This skill finds places where
concepts should point to each other and adds those links with prose that
explains the relationship. OKF links are untyped, so the relationship has
to be stated in the surrounding sentence.

Everything here goes through the okf-mcp tools (`mcp__okf-mcp__*`), on
the catalog the session names (a SessionStart hook names it; otherwise use
the connection's write target from `list_catalogs`). The server is the
source of truth. Never read or write a git checkout of the catalog, or run
its tooling: it is a frozen snapshot.

## 0. Open a changeset

Call `open_changeset` and keep the handle it returns; pass it as
`changeset` to every edit below, and to the reports so they see your
drafts. If you lose it, `list_changesets` finds it again.

## 1. Run the report

`links_report` (add `changeset` once you have edits) reports:

| Section | Meaning | Action |
|---|---|---|
| Broken links | Target doesn't exist | Always fix: correct the path or remove the link |
| Relative links in concepts | Concept links not bundle-root absolute | Rewrite as `/dir/concept.md` (index files may stay relative) |
| Orphans | No other concept links to it (indexes don't count) | Find the concepts that should mention it |
| Dead ends | Links to no other concept | Find what it depends on, runs on, or is part of |
| Index drift | Concept or directory missing from its parent `index.md` | Add the listing (`index`) |
| Unlinked mentions | Another concept's filename, `title` or `aliases` appears in body prose without a link | **Candidates only**: judge each one |

`mentions: false` skips the unlinked-mention scan, the slow part, when you
only need the structural sections.

## 2. Look for relationships the report can't see

Name matching only finds explicit mentions. Also skim every concept's
`description`: `search` with an empty `query` and `limit: 200` returns them
all, and `read_document` shows each folder's `index.md`. Look for pairs
that are related without naming each other: a service and the host it runs
on, a playbook and the system it diagnoses, a project and the
infrastructure it deploys to, a symptom and its root-cause concept. Orphans
and dead ends are the best places to start. `get_concept` with
`links: true` shows a concept's outbound links and backlinks.

## 3. Decide, then edit

For each candidate, read the surrounding text in the source concept
(`get_concept`) and ask:

- Is this really a reference to the target concept, or a coincidental
  string (a hostname inside a command, a substring, a source path)?
- Would a reader of the source plausibly want to jump to the target?

If yes:

- Link the **first meaningful mention** in the body, not every one, with
  `edit_document` (`old_str` the exact sentence, `new_str` the linked
  one). Leave mentions in code blocks, tables of raw values and
  frontmatter alone.
- Use a bundle-root absolute path: `[build server](/infra/build-server.md)`.
- Make the sentence say *how* they relate ("runs on", "is diagnosed by",
  "stores its metrics in") if the existing prose doesn't already. Don't
  bolt on bare "See also" lists unless the relationship really is loose.
- When a relationship is important in both directions, link both ways.
- Don't change facts while linking. If you spot an error, fix it separately
  with honest provenance.
- If a concept is often called something other than its title or filename,
  give it an `aliases` list with `set_frontmatter` so future reports catch
  it.

Skip candidates that aren't real references; there's no need to record them.

## 4. Verify and record

1. Re-run `links_report` with the handle. Broken links must be zero; the
   remaining unlinked mentions should all be ones you deliberately skipped.
2. `log` one entry (kind `Update`): "Linked related concepts across ...
   (okf-link-check)." Linking alone doesn't change a concept's facts, so
   don't stamp `generated`.
3. Check with `pending_changes`, then `publish` with a message saying what
   was linked. It lints the changed files and refuses on errors such as
   broken links. On a `Conflict`, someone changed a document you edited:
   the refused changeset stays open, so `discard` it, re-read the
   document, redo the edit in a fresh changeset and publish again; if it
   still fails, `discard` the retry too, then stop and tell the user.
