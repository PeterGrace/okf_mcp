# Point every agent at AGENTS.md and tools/agentic/

## Changed
- `.claude/CLAUDE.md`, which Claude Code loads into every session in this repository, now opens with a pointer to `AGENTS.md` and `tools/agentic/`. Any agent doing agentic or multi-agent work then follows the shared process and tools, even when its brief doesn't mention them.
- `tools/agentic/lessons.md`: the "tests must fail without the fix" rule now says to `touch` a restored file. Otherwise cargo can reuse a test binary built from the reverted copy.

## Notes
- Requested by pgrace so the shared process applies to every fresh agent right away, not only to the ones whose brief names it.
