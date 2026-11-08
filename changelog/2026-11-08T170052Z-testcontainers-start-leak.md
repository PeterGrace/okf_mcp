# Remove a test Postgres container whose start failed

Issue #187. Under load a test's Postgres start could fail with
`StartupTimeout`, and the container was left behind. Docker can also refuse
a start ("endpoint with name ... already exists in network bridge", seen in
PR #114), which leaves the container behind the same way. Leaked
`postgres:17-alpine` containers piled up and loaded the next run, and they
had to be removed by hand.

## Cause

testcontainers 0.27.3 (`AsyncRunner::start`) creates the container first,
then runs `start_container` and the readiness wait inside
`tokio::time::timeout`. When the timeout fires, or `start_container`
returns an error, before `ContainerAsync` exists, nothing owns the container,
so no `Drop` removes it. A failure after `ContainerAsync` exists is handled:
its `Drop` removes the container. The leaked containers sit in the
`Created` state.

## Changed

- `tests/support/pg.rs`: each start labels its container
  `okf-mcp.test-start=<pid>-<counter>`, a value no other start uses, even
  across concurrent test binaries. `start_labelled` force-removes every
  container with that label (anonymous volumes too) when the start fails,
  and then returns the start's error. A 404 or 409 from Docker (the
  container is already gone, or testcontainers' own `Drop` is removing it)
  is not an error. `start_with_retry` retries a failed start once with a
  fresh label, and panics if the retry fails too. It takes a function that
  builds the request (from `postgres_17()`), because a request is consumed
  by its start. `TestDb::start` uses it.
- `tests/tls.rs` (from review): the TLS tests used to start their customized
  container (copied certificate and key, a custom command) directly, without
  the label or the cleanup. They now build it with `tls_request` and start it
  through `start_with_retry`. No test starts Postgres any other way now.
- The Docker calls go through testcontainers' `bollard` re-export, so no
  new dependency.

## Tests

- `a_timed_out_start_leaks_without_cleanup_and_remove_labelled_reaps_it`:
  a plain testcontainers start with a zero startup timeout fails and leaves
  exactly one labelled container. This is the bug, made deterministic.
  `remove_labelled` then removes it.
- `a_timed_out_start_labelled_leaves_no_container`: the same forced
  timeout through `start_labelled` returns `StartupTimeout` and leaves no
  labelled container.
- `tests/tls.rs` `a_timed_out_tls_start_leaves_no_container`: the same for
  the customized TLS request, whose files Docker copies in before the
  timeout fires.
- After a full `just test` run, `docker ps -a --filter
  label=okf-mcp.test-start` lists nothing.

## Not done

- No session-start reaper for containers that older runs or other branches
  leaked: those don't carry the new label. A process killed mid-run still
  leaves its containers behind, because no code runs to remove them.
