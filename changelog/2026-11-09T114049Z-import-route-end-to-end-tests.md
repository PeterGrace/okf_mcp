# Import route: tests for a slow upload and a failed refresh

Issue #190, a follow-up from M4 (Task 11's notes in
`changelog/2026-11-03T021533Z-m4-read-only-server.md`). The import route had
two documented behaviours with no test that drove them through the route.

## Added

Two tests in `src/http/archives.rs`. They send requests through the route
with `oneshot` and change no production code.

- `a_slow_upload_is_503_and_imports_nothing`. The request body is a
  `ReaderStream` over an in-memory pipe (`tokio::io::duplex`). Half of a
  valid archive is written up front and the rest only after
  `REQUEST_TIMEOUT` plus a second. The test pauses tokio's clock once the
  database is up, so the 120-second deadline passes at once. It checks for
  the 503 `Timeout` JSON and that the late write fails, which shows the
  route stopped reading. Then it resumes the clock and checks that the
  catalog is still at generation 0, with no live documents and an empty
  snapshot.
- `a_refresh_failing_after_a_committed_import_still_reports_the_import`.
  The fault is in the database, as in `src/app.rs`'s `break_catalog` tests,
  so the route needs no seam. A trigger on the import's generation bump
  stores a `../escape.md` row in the same transaction. The import commits,
  and building the snapshot afterwards fails. The test checks that the
  route answers 200 with the import's report. The database is one
  generation on, but the snapshot was not swapped in. Retrying the same
  upload gets 409 with the `?replace=true` hint, as the handler's comment
  says. After a manual repair, one `App::poll` pass swaps in the import.

`tokio-util`'s `io` feature (for `ReaderStream`) is now declared in
`[dev-dependencies]`. The build already enabled it through another
dependency.

## Checked

Each test fails when the behaviour it covers is broken. With the upload
deadline raised tenfold, the slow upload is no longer answered 503. With
the refresh error returned rather than logged, the import is answered 500.

## Note

The slow-upload test resumes the clock before it reads the database. With
the clock paused during database I/O, the runtime can jump past sqlx's own
timeouts. The tenfold-deadline run showed this: it got a 500, not an
import.
