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

- `Cargo.toml`: sqlx gains `tls-rustls-ring-webpki` (TLS in pure Rust, so the image
  needs no OpenSSL); its comment says the deployment turns TLS on with `PGSSLMODE` and
  `PGSSLROOTCERT`. `Cargo.lock` follows.
- Design, "Configuration": TLS to Postgres is set by `PGSSLMODE=verify-full` and
  `PGSSLROOTCERT`, which sqlx reads beneath the URL. `db::connect` is unchanged.
- Plan, Task 4: a dated `Corrected` note and the shipped refusal assertion.

## Notes

- The plan's refusal test asserted only `is_err()`, which any failed connection would
  pass. It now requires `UnknownIssuer` in the error. sqlx 0.9.0 reports the rustls
  verdict as `sqlx::Error::Io`, not `sqlx::Error::Tls`, so the test reads the message.
- The start script worked as planned: the entrypoint runs `sh -c` as root, and busybox
  `install -o postgres -g postgres -m 600` installs the key Postgres requires it to own.
