#!/usr/bin/env bash
# merge-reviewed.sh <pr>
#
# Merges a PR (merge commit) only when all of these hold, and refuses otherwise:
#   1. the review bot has reviewed the PR's current head commit;
#   2. every top-level inline comment from the bot has a reply in its thread;
#   3. GitHub reports the PR as CLEAN (mergeable, up to date as required).
# Then fetches origin so the next worktree starts from the new main.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

pr=${1:?usage: merge-reviewed.sh <pr>}
repo=$(agentic_repo)
bot=$(agentic_reviewer)

info=$(gh pr view "$pr" -R "$repo" --json headRefOid,mergeStateStatus)
head=$(jq -r .headRefOid <<<"$info")
state=$(jq -r .mergeStateStatus <<<"$info")

reviewed=$(gh api "repos/$repo/pulls/$pr/reviews" \
    --jq "[.[] | select((.user.login | startswith(\"$bot\")) and .commit_id == \"$head\")] | length")
if [ "$reviewed" = 0 ]; then
    echo "REFUSE: no $bot review on head ${head:0:7}"
    exit 1
fi

unreplied=$(gh api --paginate "repos/$repo/pulls/$pr/comments" | jq -s --arg bot "$bot" '
    add // []
    | (map(select(.in_reply_to_id != null) | .in_reply_to_id) | unique) as $replied
    | map(select((.user.login | startswith($bot))
                 and .in_reply_to_id == null
                 and ((.id as $i | $replied | index($i)) == null)))
    | map(.id)')
if [ "$unreplied" != "[]" ]; then
    echo "REFUSE: $bot threads without a reply: $unreplied"
    exit 1
fi

if [ "$state" != CLEAN ]; then
    echo "REFUSE: mergeStateStatus=$state"
    exit 1
fi

gh pr merge "$pr" -R "$repo" --merge
git -C "$(agentic_main_checkout)" fetch -q origin
echo "MERGED PR $pr -> main $(git -C "$(agentic_main_checkout)" rev-parse --short origin/main)"
