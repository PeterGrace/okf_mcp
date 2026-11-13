#!/usr/bin/env bash
# watchdog.sh
#
# Coordinator-side hang detector. Run it in the background for the whole epic.
# Every 5 minutes it looks for test or gate processes whose working directory
# is inside this repository (main checkout or any worktree), and exits 3, which
# notifies the coordinator, when one has run longer than its limit:
#   test binaries (target/.../debug/deps/*)   AGENTIC_WATCHDOG_TEST   default 1200 s
#   cargo test|clippy|build, just test|lint   AGENTIC_WATCHDOG_GATE   default 4800 s
# It reports each process's pid, elapsed time, directory and command line.
# After handling an alert, start it again.
#
# It catches what a PR-review watcher cannot: an agent stuck before it has
# opened a PR. (A timeout-less race test once stalled a task for 12 hours.)
set -u
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
source "$here/lib.sh"

root=$(agentic_main_checkout)
test_limit=${AGENTIC_WATCHDOG_TEST:-1200}
gate_limit=${AGENTIC_WATCHDOG_GATE:-4800}
interval=${AGENTIC_WATCHDOG_INTERVAL:-300}

while :; do
    found=""
    while read -r pid elapsed args; do
        cwd=$(readlink "/proc/$pid/cwd" 2>/dev/null) || continue
        case "$cwd" in "$root"|"$root"/*) ;; *) continue ;; esac
        limit=$gate_limit
        case "$args" in */debug/deps/*) limit=$test_limit ;; esac
        if [ "$elapsed" -gt "$limit" ]; then
            found+="pid=$pid elapsed=${elapsed}s cwd=$cwd cmd=${args:0:200}"$'\n'
        fi
    done < <(ps -eo pid=,etimes=,args= \
        | grep -E '/debug/deps/|cargo (test|clippy|build)|just (test|lint|test-ui)' \
        | grep -v -E 'grep|watchdog\.sh')
    if [ -n "$found" ]; then
        echo "WATCHDOG $(date -u +%FT%TZ): long-running test processes in $root:"
        printf '%s' "$found"
        exit 3
    fi
    sleep "$interval"
done
