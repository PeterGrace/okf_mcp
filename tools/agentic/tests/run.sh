#!/usr/bin/env bash
# Shell tests for tools/agentic. Run with `just test-agentic`, or directly.
#
# A stub `gh` (stub/gh) sits first on PATH and answers from per-test fixture
# files, watch-review.sh runs on a fake clock (stub/fake-now, stub/fake-sleep),
# and every git repository is a throwaway one under a temporary directory, so
# nothing touches GitHub, the real checkout or the wall clock (the gate and
# watchdog tests wait about a second each).
#
# Exit status: 0 when every test passes, 1 otherwise.
set -u
tests_dir=$(cd "$(dirname "$0")" && pwd)
tools=$(dirname "$tests_dir")
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'rm -rf "$work"' EXIT

export PATH="$tests_dir/stub:$PATH"
export AGENTIC_REPO=owner/repo AGENTIC_RETRIGGER=/prrb
# The scripts' defaults are under test, so none of these come from the caller.
unset AGENTIC_REVIEWER AGENTIC_MAX_REVIEW_ROUNDS AGENTIC_NOW_CMD AGENTIC_SLEEP_CMD

bot='brooke-reviews[bot]'
head_a=aaaaaaa1111111111111111111111111111111111
head_b=bbbbbbb2222222222222222222222222222222222

git_quiet() {
    git -c user.name=test -c user.email=test@example.invalid \
        -c commit.gpgsign=false -c core.hooksPath=/dev/null "$@"
}

# A main checkout with an origin, and a linked worktree OUTSIDE the main
# checkout (a sibling directory), as `git worktree add ../wt` would make.
repo=$work/repo
git init -q -b main "$repo"
git_quiet -C "$repo" commit -q --allow-empty -m init
git clone -q --bare "$repo" "$work/origin.git"
git -C "$repo" remote add origin "$work/origin.git"
git -C "$repo" fetch -q origin
git -C "$repo" worktree add -q -b wt "$work/wt"
mkdir "$work/wt2" "$work/elsewhere"

# --- helpers (each test runs in its own subshell, so `exit 1` fails just it) ---

new_fixtures() {
    fx=$(mktemp -d "$work/fx.XXXXXX")
    mkdir "$fx/api"
    : >"$fx/calls.log"
    export GH_FIXTURES=$fx
}

# A user whose login shares the bot's prefix but is not the bot.
impostor='brooke-example'

# old_reviews <n>: n bot reviews, all of an earlier head than the PR's.
old_reviews() {
    jq -n --arg bot "$bot" --argjson n "$1" '[range($n) | {user: {login: $bot}, commit_id: "0ld"}]' \
        >"$fx/api/pulls_7_reviews.1.json"
}

# good_pr <head>: a PR whose every merge check passes for <head>.
good_pr() {
    printf '{"headRefOid":"%s","mergeStateStatus":"CLEAN"}\n' "$1" >"$fx/pr-view.json"
    printf '[{"user":{"login":"%s"},"commit_id":"%s"}]\n' "$bot" "$1" \
        >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"id":1,"in_reply_to_id":null,"user":{"login":"%s"}},
             {"id":2,"in_reply_to_id":1,"user":{"login":"author"}}]\n' "$bot" \
        >"$fx/api/pulls_7_comments.1.json"
}

# A full first page of 30 bot reviews of an older commit; the current-head
# review then has to be on page two. The round cap is raised so that the 30
# old reviews alone can't satisfy the scripts.
old_reviews_page() {
    export AGENTIC_MAX_REVIEW_ROUNDS=100
    jq -n --arg bot "$bot" '[range(30) | {user: {login: $bot}, commit_id: "0ld"}]' \
        >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"user":{"login":"%s"},"commit_id":"%s"}]\n' "$bot" "$head_a" \
        >"$fx/api/pulls_7_reviews.2.json"
}

die() {
    echo "  $*"
    printf '  output:\n%s\n' "${out:-}" | sed 's/^/    /'
    exit 1
}
want_rc() { [ "$rc" = "$1" ] || die "exit $rc, want $1"; }
want_out() { grep -qF -- "$1" <<<"$out" || die "output lacks '$1'"; }
want_no_out() { ! grep -qF -- "$1" <<<"$out" || die "output has '$1'"; }
want_not_merged() { [ ! -e "$fx/merged" ] || die "merged $(cat "$fx/merged")"; }

# The real-time timeout only stops a broken script from hanging the suite.
run_merge() {
    out=$(cd "$repo" && timeout 15 "$tools/merge-reviewed.sh" 7 2>&1)
    rc=$?
}

# block_gh: every gh call hangs from now on; the EXIT trap kills whatever the
# stub started, in case a script under test leaves it behind.
block_gh() {
    touch "$fx/block"
    trap '[ -s "$fx/blocked.pids" ] && xargs kill -KILL <"$fx/blocked.pids" 2>/dev/null' EXIT
}

# want_no_blocked_left: every process the blocking stub started is gone.
want_no_blocked_left() {
    [ -s "$fx/blocked.pids" ] || die "the stub was never called"
    local pid
    while read -r pid; do
        ! kill -0 "$pid" 2>/dev/null || die "stub process $pid outlived the script"
    done <"$fx/blocked.pids"
}

# The watcher on a fake clock starting at 1000 s. The real-time timeout only
# stops a broken watcher from hanging the suite; it is never reached when the
# watcher works.
run_watch() {
    echo 1000 >"$fx/clock"
    out=$(cd "$repo" && AGENTIC_NOW_CMD=fake-now AGENTIC_SLEEP_CMD=fake-sleep \
        timeout 15 "$tools/watch-review.sh" 7 2>&1)
    rc=$?
    clock=$(cat "$fx/clock")
}

# --- merge-reviewed.sh ---

test_merge_refuses_unreviewed_head() {
    new_fixtures; good_pr "$head_a"
    printf '[{"user":{"login":"%s"},"commit_id":"%s"}]\n' "$bot" "$head_b" \
        >"$fx/api/pulls_7_reviews.1.json"
    run_merge
    want_rc 1; want_out "REFUSE: no brooke-reviews[bot] review"; want_not_merged
}

test_merge_refuses_unreplied_thread() {
    new_fixtures; good_pr "$head_a"
    printf '[{"id":1,"in_reply_to_id":null,"user":{"login":"%s"}}]\n' "$bot" \
        >"$fx/api/pulls_7_comments.1.json"
    run_merge
    want_rc 1; want_out "without a reply: [1]"; want_not_merged
}

test_merge_refuses_unclean_state() {
    new_fixtures; good_pr "$head_a"
    printf '{"headRefOid":"%s","mergeStateStatus":"BEHIND"}\n' "$head_a" >"$fx/pr-view.json"
    run_merge
    want_rc 1; want_out "mergeStateStatus=BEHIND"; want_not_merged
}

test_merge_refuses_on_api_error() {
    new_fixtures; good_pr "$head_a"
    touch "$fx/api/pulls_7_reviews.fail"
    run_merge
    want_rc 1; want_out "REFUSE: could not read the reviews"; want_not_merged
}

test_merge_refuses_on_pr_view_error() {
    new_fixtures; good_pr "$head_a"; touch "$fx/pr-view.fail"
    run_merge
    want_rc 1; want_out "REFUSE: could not read PR 7"; want_not_merged
}

test_merge_refuses_on_comments_api_error() {
    new_fixtures; good_pr "$head_a"; touch "$fx/api/pulls_7_comments.fail"
    run_merge
    want_rc 1; want_out "REFUSE: could not read the inline comments"; want_not_merged
}

test_merge_ignores_prefix_sharing_reviewer() {
    new_fixtures; good_pr "$head_a"
    # Three reviews of the current head by someone who is not the bot: they
    # must neither count as the head's review nor use up rounds.
    jq -n --arg who "$impostor" --arg head "$head_a" \
        '[range(3) | {user: {login: $who}, commit_id: $head}]' >"$fx/api/pulls_7_reviews.1.json"
    run_merge
    want_rc 1; want_out "(0 of 3 rounds used)"; want_not_merged
}

test_merge_ignores_prefix_sharing_thread() {
    new_fixtures; good_pr "$head_a"
    printf '[{"id":5,"in_reply_to_id":null,"user":{"login":"%s"}}]\n' "$impostor" \
        >"$fx/api/pulls_7_comments.1.json"
    run_merge
    want_rc 0; want_out "MERGED PR 7"
}

test_merge_matches_bot_login_without_suffix() {
    new_fixtures; good_pr "$head_a"
    out=$(cd "$repo" && AGENTIC_REVIEWER=brooke-reviews "$tools/merge-reviewed.sh" 7 2>&1)
    rc=$?
    want_rc 0; want_out "MERGED PR 7"
}

test_merge_refuses_on_hung_call() {
    new_fixtures; good_pr "$head_a"; block_gh
    started=$SECONDS
    AGENTIC_CALL_TIMEOUT=1 run_merge
    want_rc 1; want_out "REFUSE: could not read PR 7"; want_not_merged
    [ $((SECONDS - started)) -le 4 ] || die "took $((SECONDS - started)) s"
    want_no_blocked_left
}

test_merge_refuses_head_changed_before_merge() {
    new_fixtures; good_pr "$head_a"
    echo "$head_b" >"$fx/merge-head"
    run_merge
    want_rc 1; want_out "REFUSE: the merge of aaaaaaa failed"; want_not_merged
}

test_merge_finds_review_on_page_two() {
    new_fixtures; good_pr "$head_a"; old_reviews_page
    run_merge
    want_rc 0; want_out "MERGED PR 7"; want_no_out "round cap"
}

test_merge_below_cap_requires_current_head_review() {
    new_fixtures; good_pr "$head_a"; old_reviews 2
    run_merge
    want_rc 1; want_out "REFUSE: no brooke-reviews[bot] review on head aaaaaaa (2 of 3 rounds used)"
    want_not_merged
}

test_merge_at_cap_merges_newer_head() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    run_merge
    want_rc 0; want_out "under the review round cap (3 of 3 rounds"; want_out "MERGED PR 7"
    grep -qF -- "--match-head-commit $head_a" "$fx/calls.log" || die "merge not pinned"
}

test_merge_at_cap_refuses_unreplied_thread() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    printf '[{"id":1,"in_reply_to_id":null,"user":{"login":"%s"}}]\n' "$bot" \
        >"$fx/api/pulls_7_comments.1.json"
    run_merge
    want_rc 1; want_out "without a reply: [1]"; want_not_merged
}

test_merge_at_cap_refuses_unclean_state() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    printf '{"headRefOid":"%s","mergeStateStatus":"BLOCKED"}\n' "$head_a" >"$fx/pr-view.json"
    run_merge
    want_rc 1; want_out "mergeStateStatus=BLOCKED"; want_not_merged
}

test_merge_merges_when_all_checks_pass() {
    new_fixtures; good_pr "$head_a"
    run_merge
    want_rc 0; want_out "MERGED PR 7 -> main"
    [ "$(cat "$fx/merged")" = "$head_a" ] || die "merged the wrong head"
    grep -qF -- "pr merge 7 -R owner/repo --merge --match-head-commit $head_a" "$fx/calls.log" \
        || die "merge not pinned to the head"
}

# --- watch-review.sh ---

test_watch_succeeds_on_current_head_review() {
    new_fixtures; good_pr "$head_a"
    run_watch
    want_rc 0; want_out "reviewed PR 7 at aaaaaaa"
}

test_watch_finds_review_on_page_two() {
    new_fixtures; good_pr "$head_a"; old_reviews_page
    run_watch
    want_rc 0; want_out "reviewed PR 7"
}

test_watch_at_cap_exits_4_without_retrigger() {
    new_fixtures; good_pr "$head_a"; old_reviews 3
    run_watch
    want_rc 4; want_out "review round limit reached (3)"
    [ "$clock" = 1000 ] || die "waited until $clock instead of exiting at once"
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered at the cap"
}

test_watch_gives_up_when_pr_view_keeps_failing() {
    new_fixtures; good_pr "$head_a"; touch "$fx/pr-view.fail"
    printf '[{"user":{"login":"%s"},"body":"skipped: out of date with main"}]\n' "$bot" \
        >"$fx/api/issues_7_comments.1.json"
    run_watch
    want_rc 2; want_out "TIMEOUT"; want_out "out of date with main"
    [ "$clock" -ge 2800 ] && [ "$clock" -le 2830 ] || die "gave up at $clock, want 2800..2830"
}

test_watch_retrigger_keeps_overall_deadline() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    run_watch
    want_rc 2; want_out "posted /prrb on PR 7"; want_out "TIMEOUT"
    [ "$(grep -c '^pr comment 7 .*--body /prrb' "$fx/calls.log")" = 1 ] || die "re-triggered more than once"
    [ "$clock" -ge 2800 ] && [ "$clock" -le 2830 ] || die "gave up at $clock, want 2800..2830"
}

# --- watchdog.sh ---

test_watch_reviews_api_error_never_retriggers() {
    new_fixtures; good_pr "$head_a"; touch "$fx/api/pulls_7_reviews.fail"
    run_watch
    want_rc 2; want_out "TIMEOUT"; want_out "could not read the reviews"
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered with the round count unknown"
    [ "$clock" -ge 2800 ] && [ "$clock" -le 2830 ] || die "gave up at $clock, want 2800..2830"
}

test_watch_reactions_api_error_never_retriggers() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    touch "$fx/api/issues_7_reactions.fail"
    run_watch
    want_rc 2
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered with the reactions unknown"
}

test_watch_ignores_prefix_sharing_reaction() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"user":{"login":"%s"},"content":"eyes"}]\n' "$impostor" \
        >"$fx/api/issues_7_reactions.1.json"
    run_watch
    # The bot hasn't reacted, so the re-trigger is due after ~90 s (the first
    # poll past 90 s is at 1120), not the ~6 min it waits after a bot reaction.
    [ "$(cat "$fx/comment-times" 2>/dev/null)" = 1120 ] \
        || die "re-triggered at $(cat "$fx/comment-times" 2>/dev/null), want 1120"
}

test_watch_bot_reaction_delays_retrigger() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    printf '[{"user":{"login":"%s"},"content":"eyes"}]\n' "$bot" \
        >"$fx/api/issues_7_reactions.1.json"
    run_watch
    [ "$(cat "$fx/comment-times" 2>/dev/null)" = 1390 ] \
        || die "re-triggered at $(cat "$fx/comment-times" 2>/dev/null), want 1390"
}

test_watch_never_retriggers_while_behind_main() {
    new_fixtures; good_pr "$head_a"
    echo '[]' >"$fx/api/pulls_7_reviews.1.json"
    printf '{"headRefOid":"%s","mergeStateStatus":"BEHIND"}\n' "$head_a" >"$fx/pr-view.json"
    run_watch
    want_rc 2; want_out "behind main: merge origin/main and push"
    ! grep -q '^pr comment' "$fx/calls.log" || die "re-triggered while behind main"
}

test_watch_stalled_gh_still_ends_at_deadline() {
    new_fixtures; good_pr "$head_a"; block_gh
    # Real clock with a 2 s deadline: a stalled gh must not hold the watcher
    # past it, and the diagnostics get only their own few seconds.
    started=$SECONDS
    out=$(cd "$repo" && AGENTIC_WATCH_TIMEOUT=2 timeout 15 "$tools/watch-review.sh" 7 2>&1)
    rc=$?
    want_rc 2; want_out "TIMEOUT"
    [ $((SECONDS - started)) -le 8 ] || die "took $((SECONDS - started)) s, want about 5"
    want_no_blocked_left
}

test_watchdog_covers_worktrees_outside_main_checkout() {
    new_fixtures
    # Not local: the EXIT trap runs after the function returns. The fakes'
    # output goes to /dev/null so they don't hold the runner's pipe open.
    fake_pids=()
    trap 'kill "${fake_pids[@]}" 2>/dev/null' EXIT
    local inside outside_sibling outside_prefix
    # Fake test binaries: argv[0] names a test binary, the process is a sleep.
    (cd "$work/wt" && exec -a "$work/wt/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    inside=$!; fake_pids+=("$inside")
    (cd "$work/wt2" && exec -a "$work/wt2/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    outside_prefix=$!; fake_pids+=("$outside_prefix")
    (cd "$work/elsewhere" && exec -a "$work/elsewhere/target/debug/deps/fake_test" sleep 30) >/dev/null 2>&1 &
    outside_sibling=$!; fake_pids+=("$outside_sibling")
    sleep 1.2 # so every fake has run longer than the 0 s limit
    out=$(cd "$work/wt" && AGENTIC_WATCHDOG_TEST=0 AGENTIC_WATCHDOG_GATE=0 \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
    want_rc 3; want_out "pid=$inside "
    grep -qE "pid=($outside_prefix|$outside_sibling) " <<<"$out" && die "reported a process outside"
    kill "$inside"; wait "$inside" 2>/dev/null
    out=$(cd "$work/wt" && AGENTIC_WATCHDOG_TEST=0 AGENTIC_WATCHDOG_GATE=0 \
        timeout 15 "$tools/watchdog.sh" --once 2>&1)
    rc=$?
    want_rc 0
}

# --- gate.sh ---

test_gate_timeout_returns_124() {
    new_fixtures; fake_just 'exec sleep 30'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_FMT=1s \
        "$tools/gate.sh" fmt 2>&1)
    rc=$?
    want_rc 124; want_out "Treat this as a HANG"
}

# fake_just <body>: puts a `just` running <body> first on PATH for this test.
fake_just() {
    mkdir -p "$fx/bin"
    printf '#!/usr/bin/env bash\n%s\n' "$1" >"$fx/bin/just"
    chmod +x "$fx/bin/just"
}

test_gate_returns_the_gate_exit_status() {
    new_fixtures; fake_just 'exit 3'
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" "$tools/gate.sh" lint 2>&1)
    rc=$?
    want_rc 3; want_no_out "HANG"
}

# leaky_gate <then>: a fake gate that starts a child which ignores TERM and
# inherits the lock's fd, then runs <then> (wait, exit 0, kill itself...).
# The child's output goes to /dev/null so it can't hold the test's pipe open.
leaky_gate() {
    fake_just "bash -c 'trap \"\" TERM; exec sleep 30' >/dev/null 2>&1 &
echo \$! >\"\$GH_FIXTURES/child.pid\"
$1"
    trap '[ -s "$fx/child.pid" ] && kill -KILL "$(cat "$fx/child.pid")" 2>/dev/null' EXIT
}

# run_leaky_gate: runs the lint gate with a 1 s limit and 1 s grace.
run_leaky_gate() {
    started=$SECONDS
    (cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_LINT=1s AGENTIC_GATE_KILL_AFTER=1 \
        "$tools/gate.sh" lint) >"$fx/out" 2>&1
    rc=$?
    out=$(cat "$fx/out")
}

# want_nothing_left: gate.sh returned quickly, its TERM-ignoring child is gone
# and the lock is free.
want_nothing_left() {
    [ $((SECONDS - started)) -le 6 ] || die "took $((SECONDS - started)) s"
    [ -s "$fx/child.pid" ] || die "the fake gate didn't start its child"
    ! kill -0 "$(cat "$fx/child.pid")" 2>/dev/null || die "the TERM-ignoring child outlived the gate"
    flock -n "$repo/.git/agentic/gates.lock" true || die "gates.lock is still held"
}

test_gate_timeout_stops_term_ignoring_child_and_frees_lock() {
    new_fixtures; leaky_gate 'wait'
    run_leaky_gate
    want_rc 124; want_out "Treat this as a HANG"; want_nothing_left
}

test_gate_killed_leader_stops_child_and_frees_lock() {
    new_fixtures; leaky_gate 'kill -KILL $$'
    run_leaky_gate
    want_rc 124; want_out "Treat this as a HANG"; want_nothing_left
}

test_gate_passing_leader_stops_leftover_child_and_frees_lock() {
    new_fixtures; leaky_gate 'exit 0'
    run_leaky_gate
    want_rc 0; want_out "leaving 1 process(es) behind"; want_no_out "HANG"; want_nothing_left
}

test_gate_lock_wait_is_bounded() {
    new_fixtures; fake_just 'exit 0'
    mkdir -p "$repo/.git/agentic"
    # One process takes the lock and becomes the sleep, so killing $holder
    # releases the lock.
    # shellcheck disable=SC2016 # $1 is expanded by the inner bash
    bash -c 'exec 9>"$1"; flock 9; exec sleep 30' _ "$repo/.git/agentic/gates.lock" \
        >/dev/null 2>&1 &
    lock_holder=$! # not local: the EXIT trap runs after the function returns
    trap 'kill "$lock_holder" 2>/dev/null' EXIT
    sleep 0.2
    out=$(cd "$repo" && PATH="$fx/bin:$PATH" AGENTIC_TIMEOUT_LINT=1s "$tools/gate.sh" lint 2>&1)
    rc=$?
    kill "$lock_holder"; wait "$lock_holder" 2>/dev/null
    want_rc 124; want_out "Treat this as a HANG"
}

test_gate_narrow_builds_offline() {
    new_fixtures
    mkdir -p "$fx/bin"
    # A fake cargo that records the SQLX_OFFLINE it was given.
    cat >"$fx/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${SQLX_OFFLINE-unset}" >"$GH_FIXTURES/sqlx_offline"
EOF
    chmod +x "$fx/bin/cargo"
    out=$(cd "$repo" && env -u SQLX_OFFLINE DATABASE_URL=postgres://db.example.invalid/x \
        PATH="$fx/bin:$PATH" "$tools/gate.sh" narrow --lib 2>&1)
    rc=$?
    want_rc 0
    [ "$(cat "$fx/sqlx_offline")" = true ] \
        || die "cargo got SQLX_OFFLINE=$(cat "$fx/sqlx_offline"), want true"
}

# --- runner ---

passed=0
failed=0
for test_name in $(declare -F | awk '$3 ~ /^test_/ {print $3}'); do
    t0=$(date +%s%N)
    if result=$("$test_name" 2>&1); then
        passed=$((passed + 1))
        echo "ok   $test_name ($(( ($(date +%s%N) - t0) / 1000000 )) ms)"
    else
        failed=$((failed + 1))
        echo "FAIL $test_name"
        printf '%s\n' "$result"
    fi
done
echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
