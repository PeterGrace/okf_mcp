# AGENTS.md

Instructions for AI coding agents working in this repository, whatever the harness.

## Code rules

The Rust code rules (style, documentation, error handling, testing and the pre-commit checklist) are in [`.claude/CLAUDE.md`](.claude/CLAUDE.md). They apply to every agent, not only Claude Code.

## Agentic development tooling: use it, don't reinvent it

Shared tooling for agent-driven work lives in [`tools/agentic/`](tools/agentic/README.md). **Use these tools instead of writing your own scripts, watchers, loops or ad-hoc commands for the same jobs.** They hold lessons paid for in real incidents. A home-made replacement drops those lessons, for example the hard timeouts that stop a hung test from stalling work for hours.

| Job | Use | Not |
|---|---|---|
| Running `fmt`, `lint`, `test`, `test-ui` or a narrow `cargo test` | `tools/agentic/gate.sh <gate>` (shared lock and hard timeout; exit 124 means a hang) | bare `just test`, `cargo test` or your own `flock` / `timeout` wrapper |
| Waiting for the review bot on a PR | `tools/agentic/watch-review.sh <pr>` | polling loops or `sleep` |
| Merging a reviewed PR | `tools/agentic/merge-reviewed.sh <pr>` (checks the review, thread replies and merge state) | `gh pr merge` directly |
| Detecting hung test or gate processes while coordinating | `tools/agentic/watchdog.sh` in the background | waiting on an agent's report |
| Briefing an implementation agent | a short task brief plus [`tools/agentic/brief-common.md`](tools/agentic/brief-common.md) | rewriting the shared rules into each brief |
| Avoiding known classes of bugs | [`tools/agentic/lessons.md`](tools/agentic/lessons.md); add a rule when a review finds a new class | relearning them in review |

If a tool is missing a feature you need, extend it in `tools/agentic/` in a PR, so every agent gets the change. Don't keep a private copy.

## Workflow summary

- **One PR per sub-issue.** Each sub-issue gets its own git worktree (`.worktrees/okf-mcp-<N>`) and its own PR. The milestone's plan under `docs/superpowers/plans/` is authoritative. Where the code has to differ, correct the plan in the same PR.
- **Merging.** Every PR is reviewed by the review bot (`brooke-reviews`), and every inline comment gets a reply in its thread before the PR merges.
- **Review rounds.** A PR gets at most 3 review rounds (`AGENTIC_MAX_REVIEW_ROUNDS`). In rounds 1 and 2, address all comments. In the final round (round 3), fix only correctness, security and data-loss findings; file everything else (style, extra tests, wording, refactors) as a follow-up issue labelled `review-followup`, and reply in each thread with the issue link. Then the coordinator merges with `merge-reviewed.sh`, with no further review request.
- **Keeping branches current.** When `main` moves, merge `origin/main` into the branch. Never rebase or force-push.
- **No real infrastructure details** in files, tests, commits or issues. Use placeholders such as `example.com`.
