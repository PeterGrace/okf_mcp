# M7 Task 4: TLS to Postgres (2026-11-03T19:16:45Z)

Issue #143, Task 4 of `docs/superpowers/plans/2026-11-03-okf-mcp-m7-deployment.md`
(epic #139).

## Added

- `tests/tls.rs`: two database tests against `postgres:17-alpine` started with
  `ssl=on`. A CA and a server certificate for `127.0.0.1` are generated with rcgen at
  test time and copied into the container; a start script installs the key as the
  `postgres` user's, then hands over to `docker-entrypoint.sh`.
  - `verify_full_connects_encrypted_to_a_server_its_ca_signed`: `sslmode=verify-full`
    with the signing CA connects, and `pg_stat_ssl` shows the session encrypted.
  - `verify_full_refuses_a_server_another_ca_signed`: the same mode with another CA is
    refused with rustls's `UnknownIssuer`.
- `rcgen = "0.14.10"` as a dev-dependency. No certificate or key is checked in.

## Changed

- `Cargo.toml`: sqlx gains `tls-rustls-ring-webpki` (TLS in pure Rust, so the binary
  links no OpenSSL; the runtime image still has Debian's `openssl` package through
  `ca-certificates`). Its comment says the deployment turns TLS on with `PGSSLMODE` and
  `PGSSLROOTCERT`, and that the default `sslmode=prefer` now tries TLS. `Cargo.lock`
  follows.
- Design, "Configuration": TLS to Postgres is set by `PGSSLMODE=verify-full` and
  `PGSSLROOTCERT`, which sqlx reads beneath the URL; a URL's `sslmode` or `sslrootcert`
  overrides them. `db::connect` is unchanged.
- Plan: dated `Corrected (issue #143)` notes in Task 4 (the refusal assertion, the
  `Cargo.toml` comment, the design paragraph) and Task 7 Step 7 (the TLS check also
  confirms the CNPG `uri` names no `sslmode`); Decision 8 says the binary, not the
  image, is OpenSSL-free.

## Notes

- The plan's refusal test asserted only `is_err()`, which any failed connection would
  pass. It now requires `UnknownIssuer` in the error. sqlx 0.9.0 reports the rustls
  verdict as `sqlx::Error::Io`, not `sqlx::Error::Tls`, so the test reads the message.
- The start script worked as planned: the entrypoint runs `sh -c` as root, and busybox
  `install -o postgres -g postgres -m 600` installs the key Postgres requires it to own.
- No connection code changed, but connection behaviour did: with a TLS backend compiled
  in, sqlx's default `sslmode=prefer` tries TLS, without verifying the certificate, on
  every connection that sets no mode (local runs, the CLI, the plain database tests).
- A developer shell with `PGSSLMODE=verify-full` exported breaks the plain database
  tests: their testcontainers Postgres has no TLS, and sqlx reads the variable beneath
  their URLs. Unset it (or set `PGSSLMODE=prefer`) before `cargo test`. Task 6's
  operator docs carry this too.
- The environment path itself (`PGSSLMODE`/`PGSSLROOTCERT` with no URL parameters) is
  not tested here: a test would set process-wide variables that race with the other
  tests in the binary. Task 7 Step 7 checks it on the live server's own sessions.
