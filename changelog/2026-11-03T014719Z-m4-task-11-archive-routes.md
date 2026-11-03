# M4 Task 11: the archive routes (2026-11-03T01:47:19Z)

Issue #74, Task 11 of `docs/superpowers/plans/2026-11-02-okf-mcp-m4-read-only-server.md`
(milestone 4, epic #64).

## Added

- `src/http/archives.rs`: two keyed routes, behind the bearer layer:
  - `GET /catalogs/{name}/export`: M3's `store::export` as `application/gzip`
    with `Content-Disposition: attachment; filename="<name>.tar.gz"`, for a key with
    `read` on the catalog.
  - `POST /catalogs/{name}/import[?replace=true]`: `archive::read_tarball` (on a
    blocking thread) and `store::import` as the key's actor, for a key with `admin`
    on the catalog; answers with the `ImportReport` JSON after
    `CatalogState::refresh` has swapped the new snapshot in, so the next read sees it
    without waiting for a poll.
  - Statuses: 404 for a catalog that does not exist or that the key does not reach
    (`CatalogNotSelected`), 403 for a reachable catalog the key's level does not allow
    (`Forbidden`), 400 for a refused archive (`ImportRefused`, with every problem),
    409 for a non-empty catalog without `?replace=true` (`Refused`, its message
    reworded from the CLI's `--replace` to `?replace=true`), 413 past `body_limit`.
  - `body_limit`: `max_import_bytes` plus 0.1% and 64 KiB of gzip slack, saturating
    at `usize::MAX`; `read_tarball` still enforces the exact limit on the
    decompressed stream. One unit test.
  - The export sits behind `timeout_layer()` (503 after `REQUEST_TIMEOUT`). The
    import has no timeout layer (review of PR #86): one firing after the import
    commits would answer 503 for an import that happened, and a retry would get 409.
    The handler instead bounds reading and checking the upload with
    `tokio::time::timeout(REQUEST_TIMEOUT, ..)` (503, nothing imported); the import
    and the refresh after it have no deadline.
  - The import checks the key (`Principal::require`) before it reads the body (review
    of PR #86): it takes the unread `Request` and calls `Bytes::from_request` after
    the check, so a key that may not import cannot make the server buffer up to
    `body_limit` per request. `Bytes::from_request` still honours the route's
    `DefaultBodyLimit` (413). Unit test `the_key_is_checked_before_the_body_is_read`
    (an oversized body: 403 for a reader, 413 for an admin).
- `tests/server.rs`: three end-to-end tests (export and its 404s; import's 403, 404,
  200, 409, replace, 400 and the snapshot read at once through `get_concept`; 413).

## Changed

- `src/http/mod.rs`: `REQUEST_TIMEOUT` and `timeout_layer` docs say which routes they
  bound (`/healthz` and the export; an import's upload phase only). `mod archives;` and `.merge(archives::routes(&app))` above
  `route_layer`, so the archive routes need a key.
- `src/config.rs`: `max_import_bytes`' doc comment says the import route caps the body
  at this size plus a little slack for gzip (`body_limit`) (issue #74's first comment).
- The plan: Task 11 lists `src/config.rs` and carries a dated `Corrected (issue #74)`
  note (doc comments, extra assertions, Arrange-Act-Assert; no API change).

## Notes

- Export is built in memory (a `Vec<u8>`) and returned with a `Content-Length`, as the
  CLI does; the real catalog's export is about 1 MB.
- A refresh that fails after the import committed is logged at error level and the
  report is still returned: answering with an error would make a retry hit 409. The
  poll loop picks the change up. No test forces this path, since the import and the
  refresh share one pool.
- An import into a catalog the app has not loaded yet (created after the last poll)
  answers without a refresh; the next poll loads it.
- Logs name the catalog and the key's id, never the key.
- The import's 503 path (an upload not read within `REQUEST_TIMEOUT`) has no test: it
  needs a body that never finishes, which the crate's dependencies give no cheap way to
  build.
