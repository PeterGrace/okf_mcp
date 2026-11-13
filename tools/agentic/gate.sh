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
# The gate runs in its own session (setsid, via lib.sh's agentic_run_bounded),
# so its whole process group can be stopped. On timeout the group gets TERM,
# then KILL after a grace period (AGENTIC_GATE_KILL_AFTER, default 60 s). On
# every other exit too (pass, failure, a killed gate, or INT/TERM to gate.sh),
# whatever is left of the group is stopped the same way. gate.sh returns only once the group is empty,
# so nothing it started (a test that ignores TERM, a stray background job)
# outlives it or keeps holding the lock it inherited.
#
# Exit codes:
#   the gate's own, when it exits by itself; if it left processes behind they
#     are stopped and a warning on stderr says how many;
#   124 on timeout, or when the gate was killed (137), with a hint on stderr;
#   130 or 143 when gate.sh itself gets INT or TERM (it stops the gate, then
#     dies of the same signal).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

gate=${1:?usage: gate.sh fmt|lint|test|test-ui|narrow [args...]}
shift

# Build against the committed sqlx query data, as the justfile does, so a
# DATABASE_URL in the caller's environment never steers a gate (the narrow
# gate runs cargo directly and would otherwise miss the justfile's setting).
# A value the caller set explicitly is kept.
export SQLX_OFFLINE="${SQLX_OFFLINE-true}"

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

# lib.sh's agentic_run_bounded runs the gate in its own process group and
# empties that group on every exit; the grace period here is the gate's.
agentic_kill_after=$kill_after
status=0
agentic_run_bounded "$limit_s" "${cmd[@]}" || status=$?
if [ "$status" = 124 ] || [ "$status" = 137 ]; then
    if [ "$agentic_timed_out" = 1 ]; then
        reason="hit its ${limit} limit"
    else
        reason="was killed or timed out (exit $status)"
    fi
    cat >&2 <<EOF
gate.sh: '$gate' $reason. Treat this as a HANG, not a failure:
  - run the suspect tests alone with: tools/agentic/gate.sh narrow <filter> -- --test-threads=1
  - in the test's Postgres container, check lock waits:
      SELECT pid, state, wait_event_type, left(query, 120) FROM pg_stat_activity;
  - any test that holds a lock or waits on another task must wrap the wait in
    tokio::time::timeout and fail with a message naming what it waited for.
EOF
    exit 124
fi
exit "$status"
