#!/usr/bin/env bash
# Shared helpers for the agentic-development scripts in this directory.
# Sourced, not run: `source "$(dirname "$0")/lib.sh"`.

# --- Bounded commands -------------------------------------------------------
# Every external command the tools run goes through agentic_run_bounded, so a
# stalled `gh`, `git` or test can't block past its budget or outlive it.

# How long agentic_stop_group waits between TERM and KILL, in seconds.
agentic_kill_after=${agentic_kill_after:-5}
# Set by agentic_run_bounded: 1 when its last command ran out of time.
agentic_timed_out=0

# agentic_stop_group <pgid>: TERM what is left of the process group, KILL it
# after $agentic_kill_after seconds, and return once it is empty (at once when
# it already is).
agentic_stop_group() {
    local tick
    kill -TERM -- "-$1" 2>/dev/null || return 0
    for ((tick = 0; tick < agentic_kill_after * 10; tick++)); do
        kill -0 -- "-$1" 2>/dev/null || return 0
        sleep 0.1
    done
    kill -KILL -- "-$1" 2>/dev/null || return 0
    # KILL can't be ignored; allow a few seconds for the dead to be reaped.
    for ((tick = 0; tick < 50; tick++)); do
        kill -0 -- "-$1" 2>/dev/null || return 0
        sleep 0.1
    done
}

# agentic_run_bounded <seconds> <cmd...>: runs cmd in its own session and
# process group with at most <seconds> to finish. When time runs out the group
# is stopped (agentic_stop_group). However cmd ends, anything still left in its
# group is stopped too, with a warning naming how many processes that was, and
# this returns only once the group is empty. An INT or TERM meanwhile stops the
# group and then kills this shell with the same signal.
# Returns cmd's status, or 124 when it ran out of time (agentic_timed_out=1).
agentic_run_bounded() {
    local pgid timer finished="" status=0 leftover
    agentic_timed_out=0
    # A background job of a non-interactive shell is not a group leader, so
    # setsid makes the job itself the leader of a new session: pid = group id.
    setsid "${@:2}" &
    pgid=$!
    # The timer is a plain sleep beside the command; whichever ends first wins.
    sleep "$1" &
    timer=$!
    trap 'kill "$timer" 2>/dev/null; agentic_stop_group "$pgid"; trap - INT; kill -INT "$BASHPID"' INT
    trap 'kill "$timer" 2>/dev/null; agentic_stop_group "$pgid"; trap - TERM; kill -TERM "$BASHPID"' TERM
    wait -n -p finished "$pgid" "$timer" || status=$?
    if [ "$finished" = "$pgid" ]; then
        kill "$timer" 2>/dev/null || true
        wait "$timer" 2>/dev/null || true
        if kill -0 -- "-$pgid" 2>/dev/null; then
            leftover=$(pgrep -c -g "$pgid" || true)
            agentic_stop_group "$pgid"
            echo "agentic: warning: '$2' exited (status $status) leaving $leftover" \
                "process(es) behind; stopped them" >&2
        fi
    else
        # shellcheck disable=SC2034 # read by the scripts that source this file
        agentic_timed_out=1
        agentic_stop_group "$pgid"
        wait "$pgid" 2>/dev/null || true
        status=124
    fi
    trap - INT TERM
    return "$status"
}

# The time a single gh or git call may take: $AGENTIC_CALL_TIMEOUT (default
# 60 s), and never past $agentic_deadline (epoch seconds on agentic_now's
# clock) when the caller has set one. 0 means no time is left.
agentic_call_budget() {
    local cap=${AGENTIC_CALL_TIMEOUT:-60} left
    if [ -n "${agentic_deadline:-}" ]; then
        left=$((agentic_deadline - $(agentic_now)))
        [ "$left" -lt "$cap" ] && cap=$left
    fi
    [ "$cap" -gt 0 ] || cap=0
    printf '%s\n' "$cap"
}

# agentic_gh <args...> / agentic_git <args...>: gh or git, bounded by
# agentic_call_budget. Return 124 at once when no time is left.
agentic_gh() { agentic_bounded_call gh "$@"; }
agentic_git() { agentic_bounded_call git "$@"; }
agentic_bounded_call() {
    local budget
    budget=$(agentic_call_budget)
    [ "$budget" -gt 0 ] || return 124
    agentic_run_bounded "$budget" "$@"
}

# --- Repository, reviewer and GitHub helpers --------------------------------

# The GitHub repository (owner/name) of the current checkout, or $AGENTIC_REPO.
agentic_repo() {
    if [ -n "${AGENTIC_REPO:-}" ]; then
        printf '%s\n' "$AGENTIC_REPO"
    else
        agentic_gh repo view --json nameWithOwner --jq .nameWithOwner
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
    common=$(agentic_git rev-parse --path-format=absolute --git-common-dir) || return 1
    mkdir -p "$common/agentic"
    printf '%s\n' "$common/agentic"
}

# The top of the main checkout (the parent of the common .git directory).
agentic_main_checkout() {
    local common
    common=$(agentic_git rev-parse --path-format=absolute --git-common-dir) || return 1
    dirname "$common"
}

# Every item of a paginated GitHub REST list endpoint, as one JSON array.
# `gh api --paginate` prints one array per page; `jq -s 'add'` joins them.
# Fails when any page cannot be fetched.
agentic_api_list() {
    local pages
    pages=$(agentic_gh api --paginate "$1") || return 1
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
