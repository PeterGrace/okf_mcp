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
