#!/usr/bin/env bash
# Shared helpers for the agentic-development scripts in this directory.
# Sourced, not run: `source "$(dirname "$0")/lib.sh"`.

# The GitHub repository (owner/name) of the current checkout, or $AGENTIC_REPO.
agentic_repo() {
    if [ -n "${AGENTIC_REPO:-}" ]; then
        printf '%s\n' "$AGENTIC_REPO"
    else
        gh repo view --json nameWithOwner --jq .nameWithOwner
    fi
}

# The login prefix of the review bot (matched with startswith, so the "[bot]"
# suffix GitHub adds does not matter). Override with $AGENTIC_REVIEWER.
agentic_reviewer() {
    printf '%s\n' "${AGENTIC_REVIEWER:-brooke}"
}

# The directory shared by every worktree of this repository (git's common dir),
# where cross-worktree state such as the gate lock lives.
agentic_state_dir() {
    local common
    common=$(git rev-parse --path-format=absolute --git-common-dir) || return 1
    mkdir -p "$common/agentic"
    printf '%s\n' "$common/agentic"
}

# The top of the main checkout (the parent of the common .git directory).
agentic_main_checkout() {
    local common
    common=$(git rev-parse --path-format=absolute --git-common-dir) || return 1
    dirname "$common"
}

# Every item of a paginated GitHub REST list endpoint, as one JSON array.
# `gh api --paginate` prints one array per page; `jq -s 'add'` joins them.
# Fails when any page cannot be fetched.
agentic_api_list() {
    local pages
    pages=$(gh api --paginate "$1") || return 1
    jq -s 'add // []' <<<"$pages"
}

# The most review rounds a PR gets: one round is one review by the bot, on any
# head. Asking again past this mostly produces nitpicks. Override with
# $AGENTIC_MAX_REVIEW_ROUNDS.
agentic_max_rounds() {
    printf '%s\n' "${AGENTIC_MAX_REVIEW_ROUNDS:-3}"
}

# agentic_review_counts <repo> <pr> <bot> <sha>: prints "<on_sha> <rounds>",
# the bot's reviews of that commit and of any commit, across every page.
# Fails when the reviews can't be read.
agentic_review_counts() {
    local reviews
    reviews=$(agentic_api_list "repos/$1/pulls/$2/reviews") || return 1
    jq -r --arg bot "$3" --arg head "$4" '
        map(select(.user.login | startswith($bot)))
        | "\(map(select(.commit_id == $head)) | length) \(length)"' <<<"$reviews"
}

# The clock and sleep the watchers use, in seconds. The tests swap in a fake
# clock with $AGENTIC_NOW_CMD and $AGENTIC_SLEEP_CMD so nothing really waits.
agentic_now() {
    if [ -n "${AGENTIC_NOW_CMD:-}" ]; then "$AGENTIC_NOW_CMD"; else date +%s; fi
}
agentic_sleep() {
    "${AGENTIC_SLEEP_CMD:-sleep}" "$1"
}
