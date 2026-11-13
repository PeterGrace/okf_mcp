#!/usr/bin/env bash
# watch-review.sh <pr>
#
# Waits until the review bot has reviewed the PR's CURRENT head commit, then
# exits 0. Meant to run in the background while a coordinator waits.
#
# - Posts the bot's re-trigger command ($AGENTIC_RETRIGGER, default "/prrb")
#   once if the bot has not reacted after ~90 s, or reacted but not reviewed
#   after ~6 min. Never re-triggers once the bot has given the maximum number
#   of review rounds (agentic_max_rounds).
# - Exits 4 at once when the current head has no review and the round cap is
#   reached: merge with merge-reviewed.sh instead of asking again.
# - Exits 2 once 30 min have passed since it started, whatever happened in
#   between (re-trigger, API failures), and prints the bot's latest PR
#   comments: the usual cause is a skip notice such as "out of date with main"
#   (merge origin/main into the branch and push again).
set -u
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

pr=${1:?usage: watch-review.sh <pr>}
repo=$(agentic_repo)
bot=$(agentic_reviewer)
retrigger=${AGENTIC_RETRIGGER:-/prrb}
poll=30
started=$(agentic_now)           # for re-trigger timing
readonly deadline=$((started + 1800)) # overall limit; never moves
max_rounds=$(agentic_max_rounds)
retriggered=0
head=""

while :; do
    if head=$(gh pr view "$pr" -R "$repo" --json headRefOid --jq .headRefOid); then
        counts=$(agentic_review_counts "$repo" "$pr" "$bot" "$head" 2>/dev/null) || counts="0 0"
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
            reacted=$(agentic_api_list "repos/$repo/issues/$pr/reactions" 2>/dev/null \
                | jq --arg bot "$bot" '[.[] | select(.user.login | startswith($bot))] | length') \
                || reacted=0
            if { [ "$reacted" = 0 ] && [ "$waited" -gt 90 ]; } || [ "$waited" -gt 360 ]; then
                gh pr comment "$pr" -R "$repo" --body "$retrigger" >/dev/null \
                    && retriggered=1 && echo "posted $retrigger on PR $pr"
            fi
        fi
    else
        echo "watch-review.sh: could not read PR $pr; retrying" >&2
    fi
    if [ "$(agentic_now)" -ge "$deadline" ]; then
        echo "TIMEOUT waiting for a review of PR $pr at ${head:-unknown head}"
        agentic_api_list "repos/$repo/issues/$pr/comments" 2>/dev/null \
            | jq -r --arg bot "$bot" '.[] | select(.user.login | startswith($bot)) | .body' \
            | tail -5
        exit 2
    fi
    agentic_sleep "$poll"
done
