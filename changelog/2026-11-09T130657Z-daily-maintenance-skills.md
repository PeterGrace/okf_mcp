# Skills for daily catalog maintenance

The maintenance runner files findings but never disposes of them or edits
the catalog. Nothing walked those findings, or the other upkeep reports
(sprawl, links, open questions, freshness, idle changesets, backlog
entries), with a person in the loop.

## Added

- `skills/okf-daily-maintenance/SKILL.md`, an interactive conductor. It
  checks the runner's latest run, walks each undisposed or `needs_input`
  finding (Apply, Explain, Defer, Skip; "quit" ends the pass), re-reading
  the current text before proposing anything, and disposes of each one only
  after its fix is published. Then it works through sprawl, links, open
  questions, the oldest concept, idle changesets and backlog entries, and
  prints a summary.
- Generic copies of `okf-reorganize`, `okf-link-check` and
  `okf-review-oldest`, which the conductor delegates to. They name no
  particular catalog or owner.
- `tests/skills.rs`. It starts a server, lists its tools, and checks every
  backticked snake_case name in the skills is a tool, an input-schema
  property, an enum or const value, or a listed result field.
- `skills/README.md` (install by symlinking into `~/.claude/skills`).

Spec: `docs/superpowers/specs/2026-11-09-okf-daily-maintenance-design.md`.
