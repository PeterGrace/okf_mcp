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
# The limit counts time spent waiting for the lock, so a gate stuck behind a
# hung holder also fails instead of waiting forever.
#
# Exit codes: the gate's own, or 124 on timeout (with a hint on stderr).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

gate=${1:?usage: gate.sh fmt|lint|test|test-ui|narrow [args...]}
shift

lock="$(agentic_state_dir)/gates.lock"
kill_after=60

case "$gate" in
    fmt)     limit=${AGENTIC_TIMEOUT_FMT:-5m};     locked=0; cmd=(just fmt "$@") ;;
    lint)    limit=${AGENTIC_TIMEOUT_LINT:-30m};   locked=1; cmd=(just lint "$@") ;;
    test)    limit=${AGENTIC_TIMEOUT_TEST:-75m};   locked=1; cmd=(just test "$@") ;;
    test-ui) limit=${AGENTIC_TIMEOUT_TEST_UI:-60m}; locked=1; cmd=(just test-ui "$@") ;;
    narrow)  limit=${AGENTIC_TIMEOUT_NARROW:-15m}; locked=0; cmd=(cargo test "$@") ;;
    *) echo "gate.sh: unknown gate '$gate'" >&2; exit 2 ;;
esac

if [ "$locked" = 1 ]; then
    cmd=(flock "$lock" "${cmd[@]}")
fi

status=0
timeout -k "$kill_after" "$limit" "${cmd[@]}" || status=$?
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
