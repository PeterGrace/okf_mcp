# Shared brief for implementation agents

The coordinator gives each agent a short task brief (the issue, its worktree and branch, the plan task, and what runs in parallel) and points it at this file. Everything here applies in full.

## Project rules
- **The plan is authoritative.** The milestone's plan under `docs/superpowers/plans/` governs, with the spec it names. Read its shared sections first (from the top through "Task dependencies"), then only your own task: `grep -n '^### Task' <plan>` finds it.
- **The plan's code has not been compiled.** Where it disagrees with `main` or with earlier merged tasks, `main` wins. Fix the plan in the same PR, inline or with a dated `> **Corrected (issue #N).**` note, then grep later tasks for the changed symbol and fix them too.
- **Read every comment on your issue before starting.** Earlier tasks leave heads-ups there.
- **Read `tools/agentic/lessons.md`** and follow every rule that applies. Each one was a real bug found in review.
- **Code rules:** follow `.claude/CLAUDE.md`:
  - doc comments with Arguments, Returns and Errors;
  - comments explaining Rust-specific constructs for a Python-expert reader;
  - no `.unwrap()` outside tests;
  - `thiserror` errors;
  - no emoji.
- **Changelog:** one entry per task, `changelog/<UTC ts>-m<N>-task-<NN>-<slug>.md` (letter tasks use `07a`), with `## Added`, `## Changed` and `## Notes`. Get the timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`.
- **Test databases:** each DB test starts a throwaway Postgres container, so Docker is needed. Re-run a failing test alone before concluding the failure is yours.
- **Builds:** build and test with `SQLX_OFFLINE=true`, which the justfile exports. Never set `DATABASE_URL` while building.
- **Browser tests** (`just test-ui`) use rustwright. Leave `RUSTWRIGHT_CHROMIUM` unset.
- **No real infrastructure details anywhere:** no real hostnames, domains, cluster names, IPs, personal names or emails in files, tests, commits or issues. Use placeholders such as `alice`, `example.com` and `example.internal`.

## Gates (all must pass before you push)
Run every gate through `tools/agentic/gate.sh`. It takes the cross-worktree lock for heavy gates and applies a hard timeout:

```
tools/agentic/gate.sh fmt
tools/agentic/gate.sh lint
tools/agentic/gate.sh test
tools/agentic/gate.sh test-ui              # if you touch the console or tests/ui_e2e/
tools/agentic/gate.sh narrow --lib -- <filter>   # while iterating
```

- **Exit 124 means a test hung.** It is neither a pass nor an ordinary failure, so find the stuck test before anything else (the script prints how).
- **Bound every wait.** Never run tests outside `gate.sh`, and never wait on a background job without a bound.
- **Tests you write that hold a lock or wait on another task** wrap the wait in `tokio::time::timeout`.

## Shared machine
- Write logs only inside your worktree, for example `target/i<N>-logs/`, and take test counts from your own logs.
- Never kill processes by pattern (`pkill -f`, `killall`). Kill only PIDs you started.
- Don't start extra parallel builds.

## When main moves
Run `git merge origin/main`. Never rebase or force-push. Keep both sides of a conflict, then re-run the gates before pushing. The review bot skips a PR that is behind `main`.

**Before asking for a review or a re-review** (`/prrb`), always bring the PR up to date with `main` first: merge `origin/main`, re-run the gates, push, and only then ask. The bot skips a PR that is behind `main`, so a request made before that is wasted.

## Review replies
When you address a review, reply in each inline thread (as a reply to that comment), not only in a top-level comment. `merge-reviewed.sh` refuses to merge while any reviewer thread has no reply. Keep the PR description and its claims in step with the final code after every round.

## Review rounds
A PR gets at most 3 review rounds from the bot (`AGENTIC_MAX_REVIEW_ROUNDS`).
- In rounds 1 and 2, address all comments.
- In the final round (round 3), fix only correctness, security and data-loss findings. File everything else (style, extra tests, wording, refactors) as a follow-up issue labelled `review-followup`, and reply in each thread with the issue link.
- Then the coordinator merges with no further review request.

## When done
- Don't merge.
- End commit messages with the attribution line the coordinator gives you, and use `Closes #<N>` in the PR body.
- Report:
  - the PR URL and head SHA;
  - gate results, with test counts from your own logs;
  - plan divergences, and which plan sections you corrected;
  - what you verified by reverting a fix to see its test fail.
- Keep your context: the coordinator will message you when the review lands.
- Don't hand back while your own background work is still running: finish, push, then report.
