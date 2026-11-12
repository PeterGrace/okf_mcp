# Fix #335: deterministic two-worker mail queue test

## Added

- Test helpers in `src/mail/queue.rs`: `starve` (holds a worker pool's
  connections other than its LISTEN's, so that worker cannot claim a job),
  `wait_listening` and `enqueue_claimed_by`.

## Changed

- `two_workers_never_send_the_same_mail_twice` now waits until both workers
  are LISTENing, then gives each worker one mail of its own by starving the
  other worker while that mail is claimed. The other 28 mails are enqueued
  with both workers free to race for them. The exactly-once and all-`Done`
  checks are unchanged, and the "both workers took mail" check no longer
  depends on scheduling.
- The `start_named` test helper is now `start_on`, which takes the worker's
  own pool so a test can keep a clone of it.

## Notes

- Root cause: apalis claims up to `batch_size` (10) jobs per fetch, and
  keeps claiming while sends are still in flight. The logged splits of 30
  jobs over 15 instrumented runs were 10/20 (8 runs), 20/10 (4), 17/13,
  15/15 and 23/7, so one worker could win every fetch. #335 reports the old
  test failing 1 time in 3.
- Holding back each send until both workers had started one would likely
  not have fixed this: the first worker keeps claiming while its sends wait,
  so it could still claim all 30.
- `wait_listening` counts only sessions on the test's own database, so
  workers from tests running in parallel are not counted.
- Test-only change; no production code changed.
