#!/usr/bin/env bash
# merge-reviewed.sh <pr>
#
# Merges a PR (merge commit) only when all of these hold, and refuses otherwise:
#   1. the review bot has reviewed the PR's current head commit, or it has
#      already given the maximum number of review rounds (agentic_max_rounds;
#      the final round's non-blocking findings go to follow-up issues);
#   2. every top-level inline comment from the bot has a reply in its thread;
#   3. GitHub reports the PR as CLEAN (mergeable, up to date as required).
# The merge is pinned to the inspected head, so a concurrent push makes it fail.
# Then fetches origin so the next worktree starts from the new main.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

pr=${1:?usage: merge-reviewed.sh <pr>}
repo=$(agentic_repo)
bot=$(agentic_reviewer)

info=$(gh pr view "$pr" -R "$repo" --json headRefOid,mergeStateStatus) \
    || { echo "REFUSE: could not read PR $pr"; exit 1; }
head=$(jq -r .headRefOid <<<"$info")
state=$(jq -r .mergeStateStatus <<<"$info")

counts=$(agentic_review_counts "$repo" "$pr" "$bot" "$head") \
    || { echo "REFUSE: could not read the reviews of PR $pr"; exit 1; }
read -r on_head rounds <<<"$counts"
max_rounds=$(agentic_max_rounds)
capped=0
if [ "$on_head" = 0 ]; then
    if [ "$rounds" -lt "$max_rounds" ]; then
        echo "REFUSE: no $bot review on head ${head:0:7} ($rounds of $max_rounds rounds used)"
        exit 1
    fi
    capped=1
fi

comments=$(agentic_api_list "repos/$repo/pulls/$pr/comments") \
    || { echo "REFUSE: could not read the inline comments of PR $pr"; exit 1; }
unreplied=$(jq -c --arg bot "$bot" '
    (map(select(.in_reply_to_id != null) | .in_reply_to_id) | unique) as $replied
    | map(select((.user.login | startswith($bot))
                 and .in_reply_to_id == null
                 and ((.id as $i | $replied | index($i)) == null)))
    | map(.id)' <<<"$comments")
if [ "$unreplied" != "[]" ]; then
    echo "REFUSE: $bot threads without a reply: $unreplied"
    exit 1
fi

if [ "$state" != CLEAN ]; then
    echo "REFUSE: mergeStateStatus=$state"
    exit 1
fi

if [ "$capped" = 1 ]; then
    echo "merging ${head:0:7} under the review round cap ($rounds of $max_rounds rounds;" \
        "the latest $bot review is of an earlier head)"
fi

# --match-head-commit makes GitHub refuse the merge if someone pushed after the
# checks above, so only the reviewed commit can ever be merged.
gh pr merge "$pr" -R "$repo" --merge --match-head-commit "$head" \
    || { echo "REFUSE: GitHub rejected the merge of ${head:0:7} (did the head change?)"; exit 1; }
git -C "$(agentic_main_checkout)" fetch -q origin
echo "MERGED PR $pr -> main $(git -C "$(agentic_main_checkout)" rev-parse --short origin/main)"
