# Supervisor tests measure virtual time from the pause

Issue #198. `investigator_supervisor_child_timeout_is_not_a_model_retry`
failed under heavy machine load (`f.clock.start.elapsed() < 92s`) and
passed alone.

## Cause

`TestClock::start` is a tokio `Instant` taken in `Fixture::new`, while the
clock is still real. `FakeClient::list_tools` freezes tokio time only after
`run_once` has finished the real CLI readiness step, and that step spawns
the fake CLI twice through `run_process`, which holds the process-wide
`PROCESS_LOCK`. With many tests running at once, each test's readiness waits
in real time behind the others' spawns. So `start.elapsed()` was that real
wait plus the virtual time the test meant to bound, and the bounds left only
2 s of slack (92 s for the 90 s child timeout, 22 s for the 20 s deadlines).

The issue's guess (tokio auto-advancing virtual time while waiting on the
child) does not apply: the supervisor does no real I/O after the pause, and
readiness completes before it.

## Changed

- `TestClock` records `paused_at` the first time `FakeClient::list_tools`
  pauses tokio time, and `TestClock::virtual_elapsed` measures from there.
- The three elapsed bounds (`..._lower_server_deadline_stops_child`,
  `..._local_deadline_is_monotonic`, `..._child_timeout_is_not_a_model_retry`)
  use `virtual_elapsed`. The bounds are unchanged, and they still catch a
  child that runs to its 200 s sleep or is retried.

## Verified

With a 3 s `sleep` injected into the fake CLI's `--version`, the child
timeout test fails with `start.elapsed()` and passes with `virtual_elapsed`.

## Not covered

The issue also saw 9 doctests fail under the same load. That is a separate
symptom and is not addressed here.
