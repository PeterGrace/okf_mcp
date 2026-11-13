#!/usr/bin/env bash
# gate.sh <gate> [extra args...]
#
# Runs one project gate under the cross-worktree lock and a hard timeout.
# Several agents share one machine, so heavy gates must not run at once, and a
# hung test must never stall a task: a timeout exit (124) means "a test hung",
# never "pass" or an ordinary failure.
#
# Gates and their limits (override a limit with AGENTIC_TIMEOUT_<GATE>, e.g.
# AGENTIC_TIMEOUT_TEST=90m):
#   fmt      just fmt                     5m   (no lock)
#   lint     just lint                    30m
#   test     just test                    75m
#   test-ui  just test-ui                 60m
#   narrow   cargo test <args...>         15m  (no lock; for iterating)
#
# The limit counts time spent waiting for the lock (flock -w), so a gate stuck
# behind a hung holder also fails instead of waiting forever.
#
# The gate runs in its own session (setsid), so its whole process group can be
# stopped: on timeout the group gets TERM, then KILL after a grace period
# (AGENTIC_GATE_KILL_AFTER, default 60 s), and gate.sh returns only once every
# process in it is gone. A test that ignores TERM can't outlive the reported
# timeout or keep holding the lock it inherited.
#
# Exit codes: the gate's own, or 124 on timeout (with a hint on stderr).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

gate=${1:?usage: gate.sh fmt|lint|test|test-ui|narrow [args...]}
shift

lock="$(agentic_state_dir)/gates.lock"
kill_after=${AGENTIC_GATE_KILL_AFTER:-60}

case "$gate" in
    fmt)     limit=${AGENTIC_TIMEOUT_FMT:-5m};     locked=0; cmd=(just fmt "$@") ;;
    lint)    limit=${AGENTIC_TIMEOUT_LINT:-30m};   locked=1; cmd=(just lint "$@") ;;
    test)    limit=${AGENTIC_TIMEOUT_TEST:-75m};   locked=1; cmd=(just test "$@") ;;
    test-ui) limit=${AGENTIC_TIMEOUT_TEST_UI:-60m}; locked=1; cmd=(just test-ui "$@") ;;
    narrow)  limit=${AGENTIC_TIMEOUT_NARROW:-15m}; locked=0; cmd=(cargo test "$@") ;;
    *) echo "gate.sh: unknown gate '$gate'" >&2; exit 2 ;;
esac

# A limit such as 90, 90s, 30m or 2h, in seconds.
case "$limit" in
    *[0-9]s | *[0-9]) unit=1 ;;
    *[0-9]m) unit=60 ;;
    *[0-9]h) unit=3600 ;;
    *) echo "gate.sh: bad limit '$limit' (use N, Ns, Nm or Nh)" >&2; exit 2 ;;
esac
number=${limit%[smh]}
[[ "$number" =~ ^[0-9]+$ ]] || { echo "gate.sh: bad limit '$limit'" >&2; exit 2; }
limit_s=$((number * unit))

if [ "$locked" = 1 ]; then
    # A lock wait that runs out exits 124 too: it is stuck behind a hang.
    cmd=(flock -E 124 -w "$limit_s" "$lock" "${cmd[@]}")
fi

# True while any process of the gate's group is alive.
group_alive() { kill -0 -- "-$pgid" 2>/dev/null; }

# stop_group: TERM the whole group, KILL it after the grace period, and return
# once it is gone.
stop_group() {
    local tick
    kill -TERM -- "-$pgid" 2>/dev/null || return 0
    for ((tick = 0; tick < kill_after * 10; tick++)); do
        group_alive || return 0
        sleep 0.1
    done
    kill -KILL -- "-$pgid" 2>/dev/null || return 0
    # KILL can't be ignored; allow a few seconds for the dead to be reaped.
    for ((tick = 0; tick < 50; tick++)); do
        group_alive || return 0
        sleep 0.1
    done
}

# A background job of a non-interactive shell is not a group leader, so setsid
# makes the job itself the leader of a new session: its pid is the group id.
setsid "${cmd[@]}" &
pgid=$!
trap 'stop_group; exit 130' INT TERM
deadline=$((SECONDS + limit_s))
timed_out=0
while kill -0 "$pgid" 2>/dev/null; do
    if [ "$SECONDS" -ge "$deadline" ]; then
        timed_out=1
        break
    fi
    sleep 0.5
done
status=0
if [ "$timed_out" = 1 ]; then
    stop_group
    wait "$pgid" 2>/dev/null || true
    status=124
else
    wait "$pgid" || status=$?
fi
trap - INT TERM
if [ "$status" = 124 ] || [ "$status" = 137 ]; then
    cat >&2 <<EOF
gate.sh: '$gate' hit its ${limit} limit (exit $status). Treat this as a HANG, not a failure:
  - run the suspect tests alone with: tools/agentic/gate.sh narrow <filter> -- --test-threads=1
  - in the test's Postgres container, check lock waits:
      SELECT pid, state, wait_event_type, left(query, 120) FROM pg_stat_activity;
  - any test that holds a lock or waits on another task must wrap the wait in
    tokio::time::timeout and fail with a message naming what it waited for.
EOF
    exit 124
fi
exit "$status"
