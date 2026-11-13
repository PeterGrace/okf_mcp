# Agentic development tools

These are the scripts and briefs used when a coordinator agent carries a
milestone's sub-issues through to merged PRs. Each sub-issue gets its own
background agent, git worktree and PR, and every PR is reviewed by the review
bot before it merges.

| File | Used by | What it does |
|---|---|---|
| `gate.sh` | implementation agents | Runs `fmt`, `lint`, `test`, `test-ui` or a narrow `cargo test`. Every gate has a hard timeout; the heavy gates also take a lock shared by all worktrees. Exit 124 means a test hung. |
| `watch-review.sh <pr>` | coordinator | Waits until the bot has reviewed the PR's current head. Re-triggers the bot once if it is silent, and gives up after 30 min, printing why. |
| `merge-reviewed.sh <pr>` | coordinator | Merges only if the bot reviewed the current head, every bot thread has a reply, and the PR is `CLEAN`. |
| `watchdog.sh` | coordinator | Runs in the background for the whole epic. Exits when any test or gate process in this repository runs past its limit, which catches agents stuck before they open a PR. |
| `brief-common.md` | implementation agents | The shared rules every implementation agent follows. |
| `lessons.md` | implementation agents | Rules learned from real review findings. Extend it whenever a review finds a new class of bug. |
| `lib.sh` | the scripts | Repository, reviewer and shared-state helpers. |

## Coordinator loop

1. **Start the watchdog** in the background, and restart it after each alert: `tools/agentic/watchdog.sh`.
2. **For each ready sub-issue**, create a worktree from a fresh `origin/main`:
   ```
   git worktree add .worktrees/okf-mcp-<N> -b <type>-issue-<N>-<slug> origin/main
   ```
3. **Start an agent with a short brief:** the issue, its worktree and branch, the plan task, and what runs in parallel. Point it at `tools/agentic/brief-common.md` and `tools/agentic/lessons.md`.
4. **When the agent reports a PR,** run `tools/agentic/watch-review.sh <pr>` in the background.
5. **When the review lands,** send the same agent back with the comments. Repeat until a review leaves no new comments.
6. **Merge** with `tools/agentic/merge-reviewed.sh <pr>`. Then remove the worktree and branch, and tell agents with open PRs to `git merge origin/main`.
7. **If an agent goes quiet** for longer than its gates should take, check it directly. Use `ps` for its processes, and query `pg_stat_activity` in its test container. Don't wait for its report.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `AGENTIC_REPO` | from `gh repo view` | `owner/name` of the GitHub repository |
| `AGENTIC_REVIEWER` | `brooke` | Login prefix of the review bot (matched with `startswith`) |
| `AGENTIC_RETRIGGER` | `/prrb` | Comment that re-triggers the bot |
| `AGENTIC_TIMEOUT_<GATE>` | 5m / 30m / 75m / 60m / 15m | Per-gate limits for `gate.sh`: fmt, lint, test, test-ui, narrow |
| `AGENTIC_WATCHDOG_TEST`, `_GATE`, `_INTERVAL` | 1200, 4800, 300 (seconds) | Watchdog limits and check interval |

The gate lock is `.git/agentic/gates.lock` in the shared git directory, so every worktree uses the same one.
