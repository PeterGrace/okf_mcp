#!/usr/bin/env bash
# watch-review.sh <pr>
#
# Waits until the review bot has reviewed the PR's CURRENT head commit, then
# exits 0. Meant to run in the background while a coordinator waits.
#
# - Posts the bot's re-trigger command ($AGENTIC_RETRIGGER, default "/prrb")
#   once if the bot has not reacted after ~90 s, or reacted but not reviewed
#   after ~6 min, but never while the PR is behind main (merge origin/main and
#   push first; the bot skips a PR that is out of date). Never re-triggers once the bot has given the maximum number
#   of review rounds (agentic_max_rounds).
# - Exits 4 at once when the current head has no review and the round cap is
#   reached: merge with merge-reviewed.sh instead of asking again.
# - A failed lookup (PR, reviews, reactions) decides nothing on that pass: no
#   re-trigger and no exit; the next poll retries.
# - Exits 2 once 30 min ($AGENTIC_WATCH_TIMEOUT seconds) have passed since it
#   started, whatever happened in between (re-trigger, API failures, a stalled
#   gh), and prints the bot's latest PR comments: the usual cause is a skip
#   notice such as "out of date with main" (merge origin/main into the branch
#   and push again).
# - Every gh call is bounded by the time left before that deadline, and by a
#   per-call cap ($AGENTIC_CALL_TIMEOUT, default 60 s); the final diagnostics
#   get a few seconds of their own.
set -u
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

pr=${1:?usage: watch-review.sh <pr>}
started=$(agentic_now) # for re-trigger timing
readonly deadline=$((started + ${AGENTIC_WATCH_TIMEOUT:-1800})) # overall limit; never moves
agentic_deadline=$deadline # bounds every gh call (lib.sh: agentic_call_budget)
diagnostics_budget=3
repo=$(agentic_repo) || { echo "watch-review.sh: could not resolve the repository" >&2; exit 1; }
bot=$(agentic_reviewer)
retrigger=${AGENTIC_RETRIGGER:-/prrb}
poll=30
max_rounds=$(agentic_max_rounds)
retriggered=0
warned_behind=0
head=""

# Posts the re-trigger, but never while the PR is behind main: the bot skips
# such a PR, so asking would only waste the request.
retrigger_if_current() {
    local merge_state
    if ! merge_state=$(agentic_gh pr view "$pr" -R "$repo" --json mergeStateStatus \
        --jq .mergeStateStatus); then
        echo "watch-review.sh: could not read the merge state of PR $pr; retrying" >&2
    elif [ "$merge_state" = BEHIND ]; then
        [ "$warned_behind" = 1 ] || echo "PR $pr is behind main: merge origin/main and push" \
            "before the bot is asked again; not re-triggering"
        warned_behind=1
    elif agentic_gh pr comment "$pr" -R "$repo" --body "$retrigger" >/dev/null; then
        retriggered=1
        echo "posted $retrigger on PR $pr"
    fi
}

while :; do
    # Any lookup that fails leaves its answer unknown: no decision is taken on
    # this pass (no re-trigger, no exit), the deadline stays, and the next
    # poll retries.
    if ! head=$(agentic_gh pr view "$pr" -R "$repo" --json headRefOid --jq .headRefOid); then
        echo "watch-review.sh: could not read PR $pr; retrying" >&2
    elif ! counts=$(agentic_review_counts "$repo" "$pr" "$bot" "$head"); then
        echo "watch-review.sh: could not read the reviews of PR $pr; retrying" >&2
    else
        read -r on_head rounds <<<"$counts"
        if [ "$on_head" != 0 ]; then
            echo "reviewed PR $pr at ${head:0:7}"
            exit 0
        fi
        if [ "$rounds" -ge "$max_rounds" ]; then
            echo "review round limit reached ($max_rounds); merge with merge-reviewed.sh" \
                "once the gates pass and every thread has a reply"
            exit 4
        fi
        if [ "$retriggered" = 0 ]; then
            waited=$(($(agentic_now) - started))
            if ! reactions=$(agentic_api_list "repos/$repo/issues/$pr/reactions"); then
                echo "watch-review.sh: could not read the reactions on PR $pr; retrying" >&2
            else
                reacted=$(agentic_jq_bot "$bot" '[.[] | select(.user.login | is_bot)] | length' \
                    <<<"$reactions")
                if { [ "$reacted" = 0 ] && [ "$waited" -gt 90 ]; } || [ "$waited" -gt 360 ]; then
                    retrigger_if_current
                fi
            fi
        fi
    fi
    if [ "$(agentic_now)" -ge "$deadline" ]; then
        echo "TIMEOUT waiting for a review of PR $pr at ${head:-unknown head}"
        agentic_deadline=$(($(agentic_now) + diagnostics_budget))
        agentic_api_list "repos/$repo/issues/$pr/comments" 2>/dev/null \
            | agentic_jq_bot "$bot" '.[] | select(.user.login | is_bot) | .body' -r \
            | tail -5
        exit 2
    fi
    left=$((deadline - $(agentic_now)))
    agentic_sleep "$((left < poll ? left : poll))"
done
