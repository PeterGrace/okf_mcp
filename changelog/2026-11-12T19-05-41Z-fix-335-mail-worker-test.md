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
  claims more while sends are still in flight. A run's 30 jobs were claimed
  in three blocks of 10 (logged lock counts were 10/20, 20/10, 15/15 and
  23/7), so one worker could win every block. Before the fix, the test failed
  1 time in 3.
- Holding back each send until both workers had started one would not have
  fixed this: the first worker keeps claiming while its sends wait, so it can
  still claim all 30.
- Test-only change; no production code changed.
