#!/usr/bin/env bash
# One Postgres server for a whole test run (issue #304). Every test creates
# a database of its own on it (tests/support/pg.rs), instead of starting a
# container per test.
#
#   scripts/test-pg.sh up NAME     start container NAME; print its TEST_PG_URL
#   scripts/test-pg.sh down NAME   remove container NAME (fine if it is gone)
#   scripts/test-pg.sh run CMD...  run CMD with TEST_PG_URL set: reuse the
#                                  server already named there, else start one
#                                  and remove it when CMD exits, even on failure
#
# To keep one server across runs:
#   export TEST_PG_URL=$(scripts/test-pg.sh up okf-mcp-test-pg)
#
# The server is sized for the run's concurrency (TEST_PG_JOBS, default the
# CPU count): each concurrent test holds its own pool (4) plus, for server
# tests, the app's pool (16) and a few direct connections. Its data lives in
# RAM, without fsync: a test server's data is worth nothing after the run.
set -euo pipefail

readonly IMAGE=postgres:17-alpine
# The label every test container carries (tests/support/pg.rs START_LABEL),
# so `docker ps --filter label=okf-mcp.test-start` counts this one too.
readonly LABEL=okf-mcp.test-start
readonly READY_TIMEOUT_SECONDS=60

usage() {
    echo "usage: $0 up NAME | down NAME | run CMD..." >&2
    exit 2
}

up() {
    local name=$1
    local jobs=${TEST_PG_JOBS:-$(nproc)}
    local max_connections=$((jobs * 30 + 100))
    local shm_megabytes=$((jobs * 32 + 256))
    docker run --detach --name "$name" --label "$LABEL=server-$name" \
        --env POSTGRES_PASSWORD=postgres \
        --publish 127.0.0.1::5432 \
        --tmpfs /var/lib/postgresql/data \
        --shm-size "${shm_megabytes}m" \
        "$IMAGE" \
        -c fsync=off -c synchronous_commit=off -c full_page_writes=off \
        -c max_connections="$max_connections" >/dev/null
    # The image's init runs a server on a Unix socket only; TCP answers once
    # the real server is up.
    local waited=0
    until docker exec "$name" pg_isready --quiet --host 127.0.0.1 --username postgres; do
        if ((waited >= READY_TIMEOUT_SECONDS)); then
            echo "postgres in $name was not ready after ${READY_TIMEOUT_SECONDS}s" >&2
            docker logs "$name" >&2 || true
            return 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    local address
    address=$(docker port "$name" 5432/tcp | head -n 1)
    echo "postgres://postgres:postgres@$address"
}

down() {
    local name=$1
    if docker container inspect "$name" >/dev/null 2>&1; then
        docker rm --force --volumes "$name" >/dev/null
    fi
}

run() {
    if [[ -n ${TEST_PG_URL:-} ]]; then
        exec "$@"
    fi
    local name="okf-mcp-test-pg-$$"
    # Set before `up`, so a server that fails to become ready is removed too.
    # INT and TERM exit through the EXIT trap.
    trap "down '$name'" EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    TEST_PG_URL=$(up "$name")
    export TEST_PG_URL
    "$@"
}

(($# >= 1)) || usage
command=$1
shift
case $command in
up | down)
    (($# == 1)) || usage
    "$command" "$1"
    ;;
run)
    (($# >= 1)) || usage
    run "$@"
    ;;
*) usage ;;
esac
