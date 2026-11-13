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

# The review bot's full login. Override with $AGENTIC_REVIEWER.
agentic_reviewer() {
    printf '%s\n' "${AGENTIC_REVIEWER:-brooke-reviews[bot]}"
}

# agentic_jq_bot <bot> <filter> [jq options...]: runs jq with `is_bot`
# defined, true when the input login IS the bot: an exact comparison after
# dropping an optional trailing "[bot]" from both sides, so "brooke-reviews"
# and "brooke-reviews[bot]" match but "brooke-example" never does.
agentic_jq_bot() {
    jq "${@:3}" --arg bot "$1" \
        "def is_bot: rtrimstr(\"[bot]\") == (\$bot | rtrimstr(\"[bot]\")); $2"
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
    # shellcheck disable=SC2016 # a jq program: $vars are jq's, not the shell's
    agentic_jq_bot "$3" '
        map(select(.user.login | is_bot))
        | "\(map(select(.commit_id == $head)) | length) \(length)"' -r --arg head "$4" \
        <<<"$reviews"
}

# The clock and sleep the watchers use, in seconds. The tests swap in a fake
# clock with $AGENTIC_NOW_CMD and $AGENTIC_SLEEP_CMD so nothing really waits.
agentic_now() {
    if [ -n "${AGENTIC_NOW_CMD:-}" ]; then "$AGENTIC_NOW_CMD"; else date +%s; fi
}
agentic_sleep() {
    "${AGENTIC_SLEEP_CMD:-sleep}" "$1"
}
