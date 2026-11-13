#!/usr/bin/env bash
# watch-review.sh <pr>
#
# Waits until the review bot has reviewed the PR's CURRENT head commit, then
# exits 0. Meant to run in the background while a coordinator waits.
#
# - Posts the bot's re-trigger command ($AGENTIC_RETRIGGER, default "/prrb")
#   once if the bot has not reacted after ~90 s, or reacted but not reviewed
#   after ~6 min.
# - Exits 2 after 30 min without a review and prints the bot's latest PR
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
start=$(date +%s)
retriggered=0

while :; do
    head=$(gh pr view "$pr" -R "$repo" --json headRefOid --jq .headRefOid) || { sleep 30; continue; }
    reviews=$(gh api "repos/$repo/pulls/$pr/reviews" \
        --jq "[.[] | select((.user.login | startswith(\"$bot\")) and .commit_id == \"$head\")] | length" \
        2>/dev/null || echo 0)
    if [ "$reviews" != 0 ]; then
        echo "reviewed PR $pr at ${head:0:7}"
        exit 0
    fi
    elapsed=$(( $(date +%s) - start ))
    reacted=$(gh api "repos/$repo/issues/$pr/reactions" \
        --jq "[.[] | select(.user.login | startswith(\"$bot\"))] | length" 2>/dev/null || echo 0)
    if [ "$retriggered" = 0 ] && { { [ "$reacted" = 0 ] && [ "$elapsed" -gt 90 ]; } || [ "$elapsed" -gt 360 ]; }; then
        gh pr comment "$pr" -R "$repo" --body "$retrigger" >/dev/null && retriggered=1 && start=$(date +%s) \
            && echo "posted $retrigger on PR $pr"
    fi
    if [ "$elapsed" -gt 1800 ]; then
        echo "TIMEOUT waiting for a review of PR $pr at ${head:0:7}"
        gh api "repos/$repo/issues/$pr/comments" \
            --jq ".[] | select(.user.login | startswith(\"$bot\")) | .body" | tail -5
        exit 2
    fi
    sleep 30
done
