# One Postgres server per test run (issue #304, 2026-11-12T14:18:06Z)

## Why

Every DB-backed test started its own Postgres container (`tests/support/pg.rs`)
and migrated it from scratch. A full `cargo test` started 585 containers, about
32 at once, and `cargo nextest` would have done the same: a container per test
whatever the runner. Interrupted runs left containers behind; 34 were found and
removed before this change, 9 of them still running after 11 hours. This
supersedes #275, whose one-container-per-binary design is still one container
per test under nextest.

## What changed

- `TEST_PG_URL` (a server URL, no database) names a server every test shares.
  Each test gets a database of its own: `TestDb::start()` an empty one,
  `TestDb::start_migrated()` (used by `db::test_db`, `Server::start`, the smoke
  and roundtrip tests) a `CREATE DATABASE ... TEMPLATE` copy of
  `okf_tpl_<hash of the migrations>`. The template is built once per server
  under a session advisory lock, and marked `IS_TEMPLATE` only when complete,
  so one left by a killed builder is rebuilt.
- A dropped `TestDb` drops its database `WITH (FORCE)` on a helper thread with
  its own runtime (`Drop` cannot await). Each process drops `test_<secs>_*`
  databases older than an hour, left by killed runs.
- Without `TEST_PG_URL`, each `TestDb` starts its own container and uses its
  `postgres` database, as before.
- `scripts/test-pg.sh up|down|run`: one `postgres:17-alpine` container sized
  for the CPU count (`max_connections` = jobs x 30 + 100, `--shm-size` = jobs x
  32 + 256 MiB), data on tmpfs, `fsync`, `synchronous_commit` and
  `full_page_writes` off. `run` removes it on exit, failure, Ctrl-C or a TERM
  sent to the script alone (the command runs in the background so the trap
  fires at once), and reuses an existing `TEST_PG_URL` without starting
  anything. `just test` and `just test-ui` use it.
- Three tests counted lock waits across the whole server (`pg_locks` in
  `until_locks_are_awaited` and `src/write.rs`, `pg_stat_activity` in
  `src/store/oauth/codes.rs`); they now count only their own database's.
- New `tests/test_database.rs`: 4 processes race for a template (exactly one
  builds it), a half-built template is rebuilt, a 2-hour-old database is
  dropped while a fresh one is kept, a dropped `TestDb` drops its database, and
  the name and hash helpers.
- No CI change: no workflow runs the tests.

## Measurements

32-core workstation. Another session was benchmarking on the same machine
throughout, so wall times are noisy; the container counts are exact (`docker
events --filter label=okf-mcp.test-start --filter event=create`).

| | before (`main` 2e8d34a, `cargo test`) | after (`just test`) |
|---|---|---|
| wall time, 3 runs | 246.1, 305.2, 329.2 s | 74.9, 96.4, 100.7 s |
| median | **305.2 s** | **96.4 s** |
| Postgres containers started | 585-590 | 6 |
| peak running at once | 32-33 | 3 |
| tests passed | 1337 | 1344 |

`cargo nextest run` with the shared server: 1318 passed in 31.7 s, 6
containers, peak 3. The 6 are the shared server, `tests/tls.rs`'s own
TLS-configured servers, and the two `src/db.rs` tests of the container
start-failure cleanup. No container was left after any run.

Without `TEST_PG_URL` (plain `cargo test`, a container per test as before):
1344 passed in 267.0 s, 588 containers, peak 32, within the baseline's range.
A final `just test` on the finished branch (after the review fixes) took
70.3 s, 6 containers, peak 3.

## Seen along the way (not caused by this change, not fixed here)

- `investigator_claude_stdin_timeout_cancel_reap` failed once with "fake did
  not start" (a 5 s bound) under load, and the #258 health-tiles journey failed
  once under nextest. Both passed on rerun.
- Two `tests/investigator.rs` journal tests fail with `Err(Permission)` when
  the checkout is under `/tmp`.
