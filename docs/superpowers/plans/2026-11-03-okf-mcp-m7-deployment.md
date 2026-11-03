# OKF MCP Server, Milestone 7 (Deployment): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

**Goal:** Run the server on the home k3s cluster, reachable at `https://okf-mcp.k.vsix.me` behind bearer keys. The pieces:
- the design's "Time" clock check, an SNTP query of `time_server` at startup and every 15 minutes, reported by `/healthz`;
- TLS to Postgres;
- a container image built by GitHub Actions and published to GHCR;
- a CNPG database with backups to Garage;
- the Argo CD manifests;
- operator docs;
- the rollout itself, and a restore drill.

**Architecture:**
- **Server code (this repo):**
  - `ntp` is a minimal SNTPv4 client over `tokio::net::UdpSocket`, with no new dependency.
  - `clock_check` runs it and keeps the last result in `App`, which `/healthz` reports.
  - sqlx gains a rustls TLS feature. TLS itself is configured through libpq's `PGSSLMODE`/`PGSSLROOTCERT`, which sqlx reads, so no connection code changes.
- **Image (this repo):** a multi-stage `Dockerfile`, and `.github/workflows/image.yml`, which builds on a `v*` tag and pushes `ghcr.io/petergrace/okf-mcp` with the workflow's own token.
- **Site manifests (not in this repo):** the Deployment, CNPG `Cluster`, backup `ObjectStore` and `ScheduledBackup`, `ExternalSecret`, ConfigMap, Service, Ingress and the Argo CD `Application` live only in the private Argo CD source repository (`argo2025`). This repository is public, so it carries generic operator docs instead (Decision 5).

**Tech Stack:**
- Rust 2021 (toolchain 1.99.1). Everything M1 to M6 use.
- New: the sqlx feature `tls-rustls-ring-webpki`, and the dev-dependency `rcgen` 0.14.10 (test certificates).
- Docker BuildKit and GitHub Actions (`docker/build-push-action`).
- k3s with Argo CD, CloudNativePG 1.28.1 and plugin-barman-cloud v0.15.0, ingress-nginx and cert-manager, and Harbor's ghcr.io pull-through cache.

---

## Roadmap position

Milestone 7 of 7 in `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Milestones": "Dockerfile, k8s manifests, CNPG database, NTP check, operator docs"). M1 to M6 are on `main` (`6c203f3`, PRs #129 to #137 for M6). After M7 the design's milestones are done. Handing the git catalogs over to the server stays out of scope (design "Out of scope").

## Scope

In scope: the design's M7 row, and the sections it rests on:
- "Time": the NTP check;
- "Process and routes": `/healthz`'s clock offset;
- "Configuration": `time_server`, `DATABASE_URL`;
- "Deployment";
- "Testing";
- the M5 and M6 deferral tables' M7 rows: the NTP check, `time_server`, TLS to Postgres, deployment.

That means:
- **`ntp`:** an SNTPv4 query that returns the clock offset, round trip and stratum (Decision 7).
- **`clock_check`:** the 15-minute loop, `error` logging past 2 seconds, and the last result kept in `App` and shown by `/healthz`. `config.time_server` is read for the first time.
- **TLS to Postgres:** the rustls feature, and a database test with `verify-full` (Decision 8).
- **Image:** the `Dockerfile`, `.dockerignore`, `.github/workflows/image.yml` and version `0.7.0` (Decisions 6 and 13).
- **Operator docs:** `docs/operations.md` (generic; Decision 5).
- **Rollout:** the site manifests in `argo2025`, secrets, first sync, the first catalog, and verification (Decisions 1 to 4, 9 to 11 and 14).
- **Restore drill:** a backup restored into a scratch cluster and compared with the live database (Decision 3).
- **Follow-up:** `StillLinked` paths gain their leading slash (from M6's review of PR #137; Decision 12).
- **Wrap-up:** README, design and the milestone changelog.

Deliberately deferred:

| Item | Goes to | Why |
|---|---|---|
| The deferred CI gates (fmt, clippy, tests on GitHub) | later, when pgrace asks | pgrace deferred CI (2026-11-03); the image workflow only builds and pushes |
| More than one replica | never | Design "Out of scope"; the publish lock is per process |
| Pool limits, overlay cache cap, `Busy` branch, `parse_checked` test | things-to-fix | M5 gaps kept out by M6 Decision 2; unchanged here |
| Cutover of the git catalogs, removing `okf.py` | out of scope | Design "Out of scope" |
| Alerting on the clock offset or the backups | later | `/healthz` and the logs report them; Prometheus alert rules are a separate change in `argo2025` |
| An image updater for automatic rollouts | later | A tag bump in `argo2025` is the deploy step for now |

## Decisions taken while planning (2026-11-03)

Decisions 1 to 6 are pgrace's answers on 2026-11-03. The rest are Claude's, each marked **(decided by Claude)**.

1. **Exposure: public, keys only (pgrace).** `https://okf-mcp.k.vsix.me`, through ingress-nginx with a `letsencrypt-prod` certificate, as the autobots API is. `*.k.vsix.me` is a wildcard that already resolves to the ingress, so no DNS change is needed. Every route but `/healthz` needs a bearer key (M4). `/healthz` names no catalog (a count only), no secret and no internal host: its `clock` carries only coarse fields (Decision 7). `allowed_hosts` is `["okf-mcp.k.vsix.me"]`.
2. **Database: a new CNPG cluster (pgrace).** `okf-mcp-db`, 3 instances, in namespace `okf-mcp`. Its database and owner are `okf_mcp`. `spec.imageName` is the `r.gfpd.us` mirror path, as every CNPG `Cluster` on this cluster must be.
3. **Backups: barman-cloud to Garage (pgrace).**
   - plugin-barman-cloud is the WAL archiver, writing to a bucket of its own with a key allowed only on that bucket. A daily `ScheduledBackup` takes a base backup. Retention is 30 days.
   - The credentials come from Vault through an `ExternalSecret`.
   - Task 8 restores a backup into a scratch cluster.
   - **Commands that create or move secret values (the Garage key, the Vault write, API keys) are run by pgrace, not Claude** (decided by Claude, to keep secret values out of Claude's context and logs). Claude supplies the exact commands.
4. **Rollout: Claude, with pgrace's go-ahead before each outward step (pgrace).** The outward steps are:
   - tagging the release;
   - committing to `argo2025`;
   - creating cluster objects by hand (Task 8's scratch cluster);
   - importing the catalog.

   Each needs an explicit OK in the session, given at that step.
5. **Manifests only in `argo2025` (pgrace).** `PeterGrace/okf_mcp` is public. The site manifests name internal addresses, Vault paths and bucket names, so they live only in the private Argo CD source repository, under `kustomizations/okf-mcp/`. This repository gets generic operator docs with `example.com` placeholders. **This plan itself names no internal address**; Task 7 takes the site values from the private catalog's infrastructure concepts.
6. **Image: GHCR, built by GitHub Actions on tags (pgrace).** This departs from the design's Harbor `gfd` project.
   - **Build:** `.github/workflows/image.yml` runs on a `v*` tag (and on manual dispatch). It builds the `Dockerfile` and pushes `ghcr.io/petergrace/okf-mcp:<version>` and `:sha-<short>` with the built-in `GITHUB_TOKEN` (`packages: write`). No credential lives on the workstation.
   - **Not CI:** it runs no gates. pgrace deferred CI; the gates stay local.
   - **Pinned actions:** actions are pinned to full commit SHAs with the version in a comment (verified fact 9).
   - **Pulls:** the cluster pulls through Harbor's ghcr.io cache. Kyverno rewrites `ghcr.io/` to `r.gfpd.us/ghcrio/`, so the Deployment requests `r.gfpd.us/ghcrio/petergrace/okf-mcp:<version>` itself, as the Harbor concept advises. Anything comparing images then sees what it asked for.
7. **The NTP check is a hand-written SNTPv4 client, with no new dependency** (decided by Claude).
   - **The client:** one 48-byte request, one reply. The reply is checked for:
     - mode 4;
     - leap indicator not 3;
     - stratum 1 to 15 (0 is a kiss-o'-death);
     - our originate timestamp;
     - a non-zero transmit timestamp.

     Offset is `((t2 - t1) + (t3 - t4)) / 2`, round trip `(t4 - t1) - (t3 - t2)` (RFC 4330 section 5). Timestamps follow the 2036 era rule of RFC 4330 section 3.
   - **When it runs:** a query times out after 5 seconds, DNS included. It runs at startup and every 15 minutes, and logs at `error` past 2 seconds of offset and at `warn` on a failed query.
   - **`/healthz` reports, never fails:** it shows the last result, and its status never depends on it. A time-server outage must not take the server out of its Service. The `error` log is the signal, as the design says.
   - **`/healthz` stays coarse:** the route needs no key and is public (Decision 1), so `clock` carries only `ok`, `checked_at`, `offset_ms` and `stratum`. The server's name and the error text go only to the logs (`warn` and `error` lines), never to the public body (Brooke's review of PR #138).
   - **Without a `time_server`** there is no check, and `clock` is `null`.
8. **TLS to Postgres: rustls, configured by environment** (decided by Claude).
   - **The feature:** sqlx gets `tls-rustls-ring-webpki`. It is pure Rust, so the image needs no OpenSSL, and `ring` builds with the build image's C compiler.
   - **The settings:** the Deployment sets `PGSSLMODE=verify-full` and `PGSSLROOTCERT` (the CNPG cluster's `<cluster>-ca` secret, key `ca.crt`, mounted as a file). `DATABASE_URL` is the CNPG app secret's `uri`. sqlx reads both variables before it parses the URL (verified fact 4), so `db::connect` doesn't change.
   - **Why it verifies:** CNPG's server certificate names `<cluster>-rw`, `-rw.<ns>`, `-rw.<ns>.svc` and `-rw.<ns>.svc.cluster.local` (verified fact 5). The app secret's host is `<cluster>-rw`, so `verify-full` holds inside the namespace.
   - **Why it must be set:** CNPG's `pg_hba` also accepts plain `host` connections, so TLS is the server's choice and must be set.
9. **The pod listens on `[::]:8080`** (decided by Claude). The cluster is IPv6-primary, and autobots crash-looped listening on `0.0.0.0` only, because the kubelet's probes reach the pod's IPv6 address. Linux binds `[::]` dual-stack by default. The ConfigMap sets `listen`; `DEFAULT_LISTEN` stays `0.0.0.0:8080` for local runs.
10. **Probes and rollout** (decided by Claude):
    - a startup probe (TCP 8080, up to 5 minutes), for migrations and loading the snapshots;
    - a readiness probe on `GET /healthz`, which is 503 while the database is unreachable;
    - a liveness probe on TCP 8080 only, so a database failover never restarts the server;
    - one replica (design) and strategy `Recreate`, so two servers never overlap during a rollout.
11. **The ingress carries MCP streams and imports** (decided by Claude):
    - `nginx.ingress.kubernetes.io/proxy-buffering: "off"` and `proxy-read-timeout`/`proxy-send-timeout` `"3600"`, as the autobots API sets them, for `/mcp`'s event streams;
    - `proxy-body-size: "80m"`, because nginx's default 1 MiB would refuse the import route, whose limit is `max_import_bytes` (64 MiB) plus gzip slack.
12. **`StillLinked`'s paths gain their leading slash** (decided by Claude; the M6 follow-up from Brooke's review of PR #137).
    - **The change:** `path` and every `links[].source` are written `/a/b.md`, like every other path a tool returns. They stay catalog-relative.
    - **Why now:** before the API is reachable by other harnesses.
    - **Not changed:** other `OkfError` variants' `path` fields, which the task lists in its changelog.
13. **Version 0.7.0** (decided by Claude). `Cargo.toml` goes from `0.1.0` to `0.7.0` (milestone 7); `okf-mcp --version` prints it. The release tag is `v0.7.0`, and the image tags are `0.7.0` and `sha-<short>`.
14. **The first catalog** (decided by Claude).
    - **Catalog and import:** after the server is up, Claude creates catalog `pg` with `kubectl exec ... okf-mcp catalog create pg`, then imports the private catalog's tracked files. It copies `git archive` of `HEAD` into the pod's `/tmp` and runs `okf-mcp import pg /tmp/pg.tar.gz` there. The subcommands use `DATABASE_URL`, so no API key is involved.
    - **Keys:** pgrace creates the admin key (`okf-mcp key create ...`, which prints the key once) and keeps it.
    - **Not a cutover:** the git catalog stays the source of truth (design "Out of scope"), and the import is a copy for trying the server from other harnesses.

## Simplifications and deviations relative to the design

- **"Deployment":** the image is published to GHCR by GitHub Actions, not pushed to Harbor's `gfd` project (Decision 6). Task 9 rewrites the section.
- **"Deployment":** "Manifests in Argo CD's source repository" holds, and only there (Decision 5). The design's ConfigMap example keeps `okf-mcp.example`.
- **"Time":** "`/healthz` reports the last offset" holds. The design says nothing on the status; this plan fixes it as unaffected (Decision 7). Task 3 writes that into the design.
- **"Configuration":** `DATABASE_URL` is now joined by `PGSSLMODE` and `PGSSLROOTCERT` (Decision 8). Task 4 adds them.

## Verified facts the code relies on

Checked on 2026-11-03 against `main` at `6c203f3`, the cargo registry, upstream sources and the live cluster (read-only).

1. **The server code:**
   - `Config` (`src/config.rs:38-64`, attributes included) has no `time_server` field yet. Its module doc says "M7 clock check reads" it (`:2-3`).
   - `Config::validate` (`:109-127`) returns `Err(String)`, and `Config::load` (`:140-157`) wraps it in `OkfError::Config`.
   - `DEFAULT_LISTEN` is `0.0.0.0:8080` (`:19-20`).
   - `App::load` (`src/app.rs:83-123`) builds every field. `App` already has `clock()` (`:136`) and `poll_until(self: Arc<Self>, shutdown)` (`:304-325`), the pattern the clock loop copies.
   - `http::serve` (`src/http/mod.rs:238-259`) spawns `poll_until` and awaits it after shutdown.
   - `healthz` (`:135-152`) serializes `Health { database, catalogs }` (`:118-125`).
   - `tests/server.rs:54` asserts that whole body: `{"database": "ok", "catalogs": 1}`.
   - The CLI's global flags are `--database-url` (env `DATABASE_URL`) and `--config` (env `OKF_MCP_CONFIG`) (`src/cli.rs:42-46`).
   - `#[command(version)]` (`:37`) prints `CARGO_PKG_VERSION`.
   - Logging reads `RUST_LOG` (`src/main.rs:23-26`).
2. **`clock`:** `Clock: Send + Sync` with `now() -> DateTime<Utc>` (`src/clock.rs:13-16`), and `format_timestamp` (`:56`). chrono 0.4.45 has `impl Div<i32> for TimeDelta` (`time_delta.rs:576`).
3. **tokio 1.54.1** (`src/net/udp.rs`):
   - `UdpSocket::bind` (`:150`), `connect` (`:348`), `send` (`:570`), `recv` (`:790`), `send_to` (`:1185`), `recv_from` (`:1326`) and `local_addr` (`:297`);
   - `net::lookup_host` (`lookup_host.rs:32`) and `time::timeout` (`time/timeout.rs:86`).

   The crate already enables `net` and `time`.
4. **sqlx-postgres 0.9.0:**
   - `PgConnectOptions::new_without_pgpass` reads `PGSSLMODE` and `PGSSLROOTCERT` (`options/mod.rs:85-93`).
   - `parse_from_url` starts from it (`options/parse.rs:9-10`), and changes `sslmode`/`sslrootcert` only when the URL names them (`:52-57`).
   - A `sslrootcert` that isn't PEM text is read as a file path (`CertificateInput::from`).
   - sqlx 0.9.0's TLS features (`Cargo.toml:216-233`): `tls-rustls-ring-webpki`, `tls-rustls-aws-lc-rs`, `tls-native-tls`, `tls-none`.
5. **CloudNativePG** (`~/repos/cloudnative-pg` at `ba3580d96`; the cluster runs operator 1.28.1):
   - The default `pg_hba` ends with `host all all all {{.DefaultAuthenticationMethod}}` (`pkg/postgres/configuration.go:129`), so plain connections are accepted.
   - The server certificate's names are each service name with `.<ns>`, `.<ns>.svc` and `.<ns>.svc.<domain>` (`api/v1/cluster_funcs.go:1094-1119`).
   - On the live cluster, `<cluster>-app` has the keys `uri`, `host` (`<cluster>-rw`) and `password` among others, and `<cluster>-ca` has `ca.crt` and `ca.key`. These are key names only, read from autobots' cluster.
6. **plugin-barman-cloud v0.15.0 is installed** (the `objectstores.barmancloud.cnpg.io` CRD). One `Cluster` already uses it:
   - **`Cluster`:** `spec.plugins: [{name: barman-cloud.cloudnative-pg.io, isWALArchiver: true, parameters: {barmanObjectName, serverName}}]`.
   - **`ObjectStore`:** `spec.configuration` with `destinationPath`, `endpointURL`, `s3Credentials.{accessKeyId,secretAccessKey}` (secret name and key), `data.compression: gzip`, `wal.compression: gzip`, and `retentionPolicy: "30d"`.
   - **`ScheduledBackup`:** `spec: {method: plugin, pluginConfiguration: {name: barman-cloud.cloudnative-pg.io}, cluster: {name}, schedule: "0 5 8 * * *", backupOwnerReference: self}`.
   - **Its backups are failing:** that cluster's daily backups have failed since 2026-10-28 with "error dialing backend ... 502", the API server not reaching one node's kubelet. Its WAL archiving works. Task 7's verification takes an on-demand backup and checks that it completes, rather than trusting the schedule.
7. **Kyverno and Harbor:** the `replace-image-registry-ghcr` policy rewrites a pod's `ghcr.io/` images to `r.gfpd.us/ghcrio/`. Harbor's `ghcrio` project is a pull-through cache that deletes what hasn't been pulled for 7 days and fetches it again on the next pull.
8. **The `argo2025` conventions** (at `a1c15d8`):
   - An app is `kustomizations/app-<name>.yaml`: an `Application` with `source.path: kustomizations/<name>`, `syncPolicy.automated {prune, selfHeal}`, `CreateNamespace=true`, and a `sync-wave` annotation.
   - It is listed in `kustomizations/kustomization.yaml`'s `resources`.
   - Each app's `kustomization.yaml` sets `namespace:` and lists its files.
   - Vault values reach a namespace through `ExternalSecret`s on the `vault-backend` `ClusterSecretStore`.
   - Commits go straight to `main`.
9. **GitHub Actions, latest releases on 2026-11-03:**

   | Action | Version | SHA |
   |---|---|---|
   | `actions/checkout` | v7.0.1 | `3d3c42e5aac5ba805825da76410c181273ba90b1` |
   | `docker/setup-buildx-action` | v4.4.1 | `f87e5991a6d7451dcb8d9637bfbc97413f497069` |
   | `docker/login-action` | v4.7.0 | `81c6f93d4bc991e510b342a00b419b3bb4efc5c6` |
   | `docker/metadata-action` | v6.2.0 | `dc802804100637a589fabce1cb79ff13a1411302` |
   | `docker/build-push-action` | v7.4.0 | `c3c9e263c25d99ce0380d002d59b67737d91b0dc` |

   build-push-action takes `context`, `push`, `tags`, `labels`, `cache-from` and `cache-to`. The workstation's `gh` token has no `write:packages` scope, which is one reason the build is in Actions.
10. **rcgen 0.14.10** (source read from crates.io):
    - `CertificateParams::new(names)` turns IP-literal names into IP SANs (`certificate.rs:111-126`);
    - `params.self_signed(&key)` (`:154`) and `params.signed_by(&key, &issuer)` (`:140`);
    - `Issuer::new(params, key)` (`lib.rs:205`);
    - `KeyPair::generate()` (`key_pair.rs:85`) and `serialize_pem` (`:444`), and `Certificate::pem` (`certificate.rs:41`);
    - `IsCa::Ca(BasicConstraints::Unconstrained)`;
    - the default features include `ring` and `pem`.
11. **testcontainers 0.27.3** (re-exported by `testcontainers-modules` 0.15.0):
    - `ImageExt::with_copy_to(target, source)` (`image_ext.rs:144`); a `Vec<u8>` source gets mode `0644` and root ownership (`copy.rs:332-335`);
    - `ImageExt::with_cmd` (`:56`).

    The `postgres:17-alpine` image has no `openssl`, so the TLS test generates certificates with rcgen. A checked-in test key would trip secret scanners, the project's own included.
12. **The time server's name resolves** from the node resolvers, and CoreDNS forwards everything outside the cluster domain to the node's `/etc/resolv.conf`. So pods resolve it. Task 7 confirms this in `/healthz`.

## Conventions for every task

The M4, M5 and M6 plans' "Conventions for every task" apply unchanged. These are added:
- **Never log or print a secret.**
  - Do not log the database URL, API keys, the Garage key or certificate private keys.
  - The NTP check logs the server name, the offset and errors only.
  - The commands in Tasks 7 and 8 that handle secret values are pgrace's (Decision 3).
- **Nothing site-specific in this repository.**
  - Docs and changelogs say `example.com` and "the site's object store", never an internal address, bucket or Vault path.
  - The rollout's facts go to the private catalog (Task 7 updates the project concept).
- **Outward steps wait for pgrace's go-ahead** (Decision 4): the release tag, the `argo2025` commit, hand-created cluster objects, and the import. Ask in the session with the exact commands and diff, and wait. A reviewer's approval of a PR is not that go-ahead.

**Gates** (all must pass before every push; paste the counts into the PR):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -- --test-threads=6` (Docker, for testcontainers). If you split it, `--tests` and `--doc` cannot share one cargo call.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test parity -- --ignored`
- `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`
- Task 5 adds `docker build`. Task 9 adds the release smoke test.

There is no CI and no justfile: run the gates locally.

**Project rules** (`.claude/CLAUDE.md`):
- no `.unwrap()` in library code, and `.expect("why")` only for invariants;
- `thiserror` errors;
- a doc comment on every public item, with Arguments, Returns and Errors where they apply;
- comments explaining Rust specifics to a Python expert;
- no emoji, and no unicode that imitates it;
- `rustfmt`, and clippy with `-D warnings`.

Never write a secret or a secret-shaped string: test keys are plain strings, never `okf_` followed by 43 characters, and test certificates are generated at test time, never checked in.

**Changelog:** one entry per task, `changelog/<date -u +%Y-%m-%dT%H%M%SZ>-m7-task-<NN>-<slug>.md`, with Added/Changed/Notes sections like M6's.

Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with the Claude Code attribution line.

The code in this plan has not been compiled. If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.

## Interfaces shared across tasks

```rust
// src/ntp.rs (Task 2)
pub const DEFAULT_PORT: u16 = 123;
pub struct Reading { pub stratum: u8, pub offset: chrono::TimeDelta, pub round_trip: chrono::TimeDelta }
pub enum NtpError { Resolve { .. }, NoAddress { .. }, Io { .. }, Timeout { .. }, BadReply { .. } }
pub async fn query(server: &str, clock: &dyn Clock, timeout: std::time::Duration) -> Result<Reading, NtpError>;
#[cfg(test)] pub(crate) mod testing { pub(crate) struct StepClock; pub(crate) async fn fake_server(..) -> SocketAddr; }

// src/clock_check.rs (Task 3)
pub const CHECK_INTERVAL: Duration;   // 15 minutes
pub const QUERY_TIMEOUT: Duration;    // 5 seconds
pub const MAX_OFFSET_MS: i64;         // 2000
pub struct ClockStatus { pub ok: bool, pub checked_at: String, pub offset_ms: Option<i64>, pub stratum: Option<u8> }
pub async fn check_once(server: &str, clock: &dyn Clock, timeout: Duration) -> ClockStatus;

// src/config.rs (Task 3)
pub time_server: Option<String>,

// src/app.rs (Task 3)
impl App {
    pub fn clock_status(&self) -> Option<ClockStatus>;
    pub fn set_clock_status(&self, status: ClockStatus);
    pub async fn check_clock_until(self: Arc<Self>, shutdown: CancellationToken);
}
```

`/healthz` (Task 3): `{"database": "ok"|"unreachable", "catalogs": <n>, "clock": null | {"ok", "checked_at", "offset_ms"?, "stratum"?}}`. No server name or error text: the route is public (Decision 7).

## File map

| File | Created/modified by | Responsibility |
|---|---|---|
| `src/okf/ops/delete.rs`, `src/error.rs` (doc), `src/mcp/restructure.rs` (test), `tests/smoke.rs` | T1 | `StillLinked` paths |
| `src/ntp.rs` | T2 (create) | SNTP client |
| `src/lib.rs` | T2 (`pub mod ntp;`), T3 (`pub mod clock_check;`) | Module tree (keep both sides) |
| `src/clock_check.rs` | T3 (create) | The check and its status |
| `src/config.rs` | T3 | `time_server` |
| `src/app.rs` | T3 | `clock_status`, the loop |
| `src/http/mod.rs` | T3 | `serve` spawns the loop; `/healthz`'s `clock` |
| `tests/server.rs` | T3 | `/healthz` body |
| `Cargo.toml`, `Cargo.lock` | T4 (sqlx feature, `rcgen`), T5 (version `0.7.0`) | Keep both sides |
| `tests/tls.rs` | T4 (create) | TLS database test |
| `Dockerfile`, `.dockerignore`, `.github/workflows/image.yml` | T5 (create) | Image |
| `docs/operations.md` | T6 (create), T8 (restore section confirmed) | Operator docs |
| `README.md` | T6 (link to the operator docs), T9 | Docs |
| `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` | T1 (Errors row), T3 (Time, routes, Configuration), T4 (Configuration), T9 (Deployment, Milestones, "What M7 shipped differently") | Design |
| `changelog/*-m7-*.md` | every task | Iteration notes |
| `argo2025: kustomizations/okf-mcp/*`, `kustomizations/app-okf-mcp.yaml`, `kustomizations/kustomization.yaml` | T7 (private repo, not this one) | Site manifests |

## Task dependencies

```
Layer 0:  T1 StillLinked slash    T2 ntp    T4 TLS to Postgres    T5 image
Layer 1:  T3 clock check (T2)
Layer 2:  T6 operator docs (T3, T4, T5)
Layer 3:  T7 rollout (T1, T3, T4, T5, T6)
Layer 4:  T8 restore drill (T7)
Layer 5:  T9 wrap-up (all)
```

Edges, explicitly:
- T2 -> T3 (`ntp::query`, `ntp::testing`)
- T3 -> T6; T4 -> T6; T5 -> T6 (the docs describe the clock, TLS and the image)
- T1 -> T7; T3 -> T7; T4 -> T7; T5 -> T7; T6 -> T7 (the release tag must contain every code change)
- T7 -> T8 (a backup to restore)
- every task -> T9

Notes:
- **T1, T2, T4 and T5 start at once.** They share only `Cargo.toml`/`Cargo.lock` (T4, T5) and `src/lib.rs` (T2, later T3). Keep both sides.
- **T7 and T8 change other systems.** Their PRs here carry only the changelog and any doc corrections. Their real work is commands, run with pgrace's go-ahead.

---

### Task 1: `StillLinked` paths with a leading slash

M6's `delete_concept` reports `StillLinked { path: "infra/x.md", links: [{source: "hub.md", line}] }`, while every other path a tool returns starts with `/`. Agents feed these paths back into tools, so they should read alike (Brooke's review of PR #137; Decision 12). Only `StillLinked` changes. The task lists, in its changelog, which other `OkfError` variants carry a `path` and how each is written, without changing them.

**Files:**
- Modify: `src/okf/ops/delete.rs` (the error's `path` and each `LinkSource.source`; tests)
- Modify: `src/error.rs` (`LinkSource::source`'s doc; the `StillLinked` variant's doc)
- Modify: `src/mcp/restructure.rs` (one test expectation)
- Modify: `src/error.rs` and `src/http/mod.rs` (their `StillLinked` test fixtures)
- Modify: `tests/smoke.rs` (one expectation in `restructuring_on_a_real_catalog`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Errors", the `StillLinked` row)
- Create: `changelog/<ts>-m7-task-01-still-linked-slash.md`

- [ ] **Step 1: Change the expectations first**

In `src/okf/ops/delete.rs`'s tests, the helper becomes:

```rust
    fn link(source: &str, line: usize) -> LinkSource {
        LinkSource {
            // Tools return `/`-prefixed paths, `StillLinked`'s included.
            source: format!("/{source}"),
            line,
        }
    }
```

and every `path == "infra/x.md"`-style match on `StillLinked { ref path, .. }` gains the slash (`path == "/infra/x.md"`). Search the module for `StillLinked {` to find them all.

In `src/mcp/restructure.rs` (the delete test near line 432):

```rust
        assert_eq!(
            body["links"],
            serde_json::json!([{"source": "/hub.md", "line": 6}])
        );
```

In `tests/smoke.rs` (near line 511): `json!([{"source": "/m6-smoke/moved.md", "line": 15}])`.

Two tests build a `StillLinked` themselves and keep passing either way. Give them the real shape too, so they document it: `src/error.rs` (near line 362, and its expected JSON near line 377) and `src/http/mod.rs` (near line 359). Use `/hub.md`-style paths in both (Brooke's review of PR #138).

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --lib -- okf::ops::delete mcp::restructure`
Expected: the `StillLinked` assertions fail (`"hub.md"` vs `"/hub.md"`).

- [ ] **Step 3: Write the slash in**

In `delete_concept`:

```rust
    let links = links_into(workspace, &path, &index, listing)?;
    if !links.is_empty() {
        // Catalog-relative with a leading `/`, as every path a tool returns.
        return Err(OkfError::StillLinked {
            path: format!("/{path}"),
            links,
        });
    }
```

In `links_into`, where each `LinkSource` is built:

```rust
            links.extend(
                std::iter::repeat_with(|| LinkSource {
                    source: format!("/{source}"),
                    line: number + 1,
                })
                .take(hits),
            );
```

In `src/error.rs`, `LinkSource::source`'s doc becomes "The linking document's path, catalog-relative with a leading `/` (`/hub.md`), as every path a tool returns." The `StillLinked` variant's doc says the same of `path`.

- [ ] **Step 4: Run them to see them pass**

Run: `cargo test --lib -- okf::ops::delete mcp::restructure`, then the full gates. `tests/restructure.rs` asserts no `StillLinked` source, so it is unaffected. Check with `grep -n '"source"' tests/restructure.rs`. Then run the release smoke test once:
`OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored restructuring_on_a_real_catalog`.

- [ ] **Step 5: Design and changelog**

The design's `StillLinked` row in "Errors" says that `path` and each source are `/`-prefixed. The changelog lists the other variants with a `path` and how each writes it. Find them with `grep -n "path" src/error.rs` and the tests that build them. The list records the facts; it changes nothing.

Run every gate. Commit:

```bash
git add src/okf/ops/delete.rs src/error.rs src/http/mod.rs src/mcp/restructure.rs tests/smoke.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "fix: StillLinked paths start with a slash, like every other tool path (M7.01)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `ntp`: an SNTPv4 client

A minimal client for the design's clock check (Decision 7). It resolves `time_server` (with port 123 unless it names one), sends one version-4 client packet, and checks the reply. It returns the offset, the round trip and the server's stratum. The whole exchange, DNS included, is bounded by one timeout. Times come from the injected `Clock`, so tests are exact. A test-only `testing` module provides a one-shot fake server and a clock that steps through given instants, for Task 3's tests too.

**Files:**
- Create: `src/ntp.rs`
- Modify: `src/lib.rs` (`pub mod ntp;`, in alphabetical order)
- Create: `changelog/<ts>-m7-task-02-ntp.md`

- [ ] **Step 1: The module with its tests (the tests fail to compile until Step 3)**

Create `src/ntp.rs`, with the tests below and the items as `todo!()` stubs. Then fill the stubs in Step 3.

```rust
//! A minimal SNTP client (RFC 4330, NTP version 4): one request, one reply,
//! the clock offset. The server's clock check (design "Time", `clock_check`)
//! runs it against `time_server` at startup and every 15 minutes.
//!
//! No crate: the protocol is one 48-byte packet each way. Timestamps are
//! 64-bit NTP fixed point: seconds since 1900 in the high 32 bits, a binary
//! fraction of a second in the low 32. Seconds wrap in February 2036 (era 1);
//! [`from_ntp`] reads a timestamp whose top bit is clear as era 1, as RFC 4330
//! section 3 says.

use std::net::SocketAddr;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use tokio::net::{lookup_host, UdpSocket};

use crate::clock::Clock;

/// The NTP port, used when `time_server` names none.
pub const DEFAULT_PORT: u16 = 123;

/// Seconds from the NTP epoch (1900-01-01) to the Unix epoch (1970-01-01).
const NTP_UNIX_OFFSET: i64 = 2_208_988_800;

/// An NTP packet without extension fields.
const PACKET_LEN: usize = 48;

/// One measurement against a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The server's stratum (1 for a reference clock such as GPS).
    pub stratum: u8,
    /// How far the server's clock is ahead of ours (negative: behind).
    pub offset: TimeDelta,
    /// The network round trip, without the server's own processing time.
    pub round_trip: TimeDelta,
}

/// Why a query gave no reading.
#[derive(Debug, thiserror::Error)]
pub enum NtpError {
    /// The name did not resolve.
    #[error("resolve {server}: {source}")]
    Resolve {
        /// The `time_server` value.
        server: String,
        /// The resolver's error.
        #[source]
        source: std::io::Error,
    },
    /// The name resolved to no address.
    #[error("{server} resolves to no address")]
    NoAddress {
        /// The `time_server` value.
        server: String,
    },
    /// The socket failed.
    #[error("query {server}: {source}")]
    Io {
        /// The `time_server` value.
        server: String,
        /// The socket's error.
        #[source]
        source: std::io::Error,
    },
    /// No reply in time (DNS included).
    #[error("{server} did not answer within {timeout:?}")]
    Timeout {
        /// The `time_server` value.
        server: String,
        /// The limit that passed.
        timeout: Duration,
    },
    /// A reply that can't be used.
    #[error("{server} sent an unusable reply: {reason}")]
    BadReply {
        /// The `time_server` value.
        server: String,
        /// What was wrong with it.
        reason: &'static str,
    },
}

/// The fields of a reply the offset needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reply {
    stratum: u8,
    receive: u64,
    transmit: u64,
}

/// Measures the clock's offset from `server`.
///
/// # Arguments
/// * `server` - `host`, `host:port`, `a.b.c.d`, `[v6]:port` or a bare IPv6 address.
/// * `clock` - our clock; read just before sending and just after receiving.
/// * `timeout` - the limit for the whole exchange, DNS included.
///
/// # Returns
/// The offset, the round trip and the server's stratum.
///
/// # Errors
/// [`NtpError`]: the name doesn't resolve, the socket fails, no reply in
/// time, or a reply that isn't a synchronized server's answer to this request.
pub async fn query(
    server: &str,
    clock: &dyn Clock,
    timeout: Duration,
) -> Result<Reading, NtpError> {
    // `timeout` gives `Err(Elapsed)` if the deadline passes first; `?` on the
    // `map_err` result, then the inner `Result` is returned as it is.
    tokio::time::timeout(timeout, exchange(server, clock))
        .await
        .map_err(|_| NtpError::Timeout {
            server: server.to_string(),
            timeout,
        })?
}

/// One request and its reply, without a deadline (see [`query`]).
async fn exchange(server: &str, clock: &dyn Clock) -> Result<Reading, NtpError> {
    // Closures that wrap an error with the server's name. They borrow
    // `server` (a `&str`, which is `Copy`), so they can be used repeatedly.
    let io = |source: std::io::Error| NtpError::Io {
        server: server.to_string(),
        source,
    };
    let bad = |reason: &'static str| NtpError::BadReply {
        server: server.to_string(),
        reason,
    };
    let address = lookup_host(with_port(server).as_str())
        .await
        .map_err(|source| NtpError::Resolve {
            server: server.to_string(),
            source,
        })?
        .next()
        .ok_or_else(|| NtpError::NoAddress {
            server: server.to_string(),
        })?;
    // Bind the family the server's address has, on any port.
    let local = if address.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let socket = UdpSocket::bind(local).await.map_err(io)?;
    // A connected UDP socket only receives datagrams from that address.
    socket.connect(address).await.map_err(io)?;
    let sent_at = clock.now();
    let sent = to_ntp(sent_at);
    socket.send(&request(sent)).await.map_err(io)?;
    let mut buffer = [0u8; 512];
    let length = socket.recv(&mut buffer).await.map_err(io)?;
    let received_at = clock.now();
    let reply = parse_reply(&buffer[..length], sent).map_err(bad)?;
    let receive = from_ntp(reply.receive).ok_or_else(|| bad("its receive time is out of range"))?;
    let transmit = from_ntp(reply.transmit).ok_or_else(|| bad("its transmit time is out of range"))?;
    let (offset, round_trip) = offset_and_round_trip(sent_at, receive, transmit, received_at);
    Ok(Reading {
        stratum: reply.stratum,
        offset,
        round_trip,
    })
}

/// `server` with the NTP port when it names none.
fn with_port(server: &str) -> String {
    if server.parse::<SocketAddr>().is_ok() {
        return server.to_string();
    }
    if server.parse::<std::net::Ipv6Addr>().is_ok() {
        return format!("[{server}]:{DEFAULT_PORT}");
    }
    match server.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => server.to_string(),
        _ => format!("{server}:{DEFAULT_PORT}"),
    }
}

/// A client request: leap 0, version 4, mode 3, our transmit time.
fn request(transmit: u64) -> [u8; PACKET_LEN] {
    let mut packet = [0u8; PACKET_LEN];
    // Two bits of leap indicator, three of version, three of mode.
    packet[0] = 0b00_100_011;
    packet[40..48].copy_from_slice(&transmit.to_be_bytes());
    packet
}

/// Checks a reply to the request that carried `sent`.
fn parse_reply(packet: &[u8], sent: u64) -> Result<Reply, &'static str> {
    if packet.len() < PACKET_LEN {
        return Err("shorter than 48 bytes");
    }
    let leap = packet[0] >> 6;
    let mode = packet[0] & 0b111;
    let stratum = packet[1];
    if mode != 4 {
        return Err("not a server reply (mode 4)");
    }
    if leap == 3 {
        return Err("the server is not synchronized (leap indicator 3)");
    }
    if stratum == 0 {
        return Err("a kiss-o'-death reply (stratum 0)");
    }
    if stratum > 15 {
        return Err("the server is not synchronized (stratum above 15)");
    }
    // `try_into` turns the 8-byte slice into an array; the length is fixed.
    let word = |at: usize| {
        u64::from_be_bytes(packet[at..at + 8].try_into().expect("an 8-byte field"))
    };
    if word(24) != sent {
        return Err("its originate time is not this request's");
    }
    let transmit = word(40);
    if transmit == 0 {
        return Err("it has no transmit time");
    }
    Ok(Reply {
        stratum,
        receive: word(32),
        transmit,
    })
}

/// RFC 4330 section 5: the offset and the round trip from the four times.
fn offset_and_round_trip(
    sent: DateTime<Utc>,
    received_by_server: DateTime<Utc>,
    sent_by_server: DateTime<Utc>,
    received: DateTime<Utc>,
) -> (TimeDelta, TimeDelta) {
    let offset = ((received_by_server - sent) + (sent_by_server - received)) / 2;
    let round_trip = (received - sent) - (sent_by_server - received_by_server);
    (offset, round_trip)
}

/// An instant as an NTP timestamp.
fn to_ntp(instant: DateTime<Utc>) -> u64 {
    // `as u32` keeps the low 32 bits: NTP seconds wrap in 2036, and
    // `from_ntp` undoes the wrap.
    let seconds = (instant.timestamp() + NTP_UNIX_OFFSET) as u32;
    let fraction = (u64::from(instant.timestamp_subsec_nanos()) << 32) / 1_000_000_000;
    (u64::from(seconds) << 32) | fraction
}

/// An NTP timestamp as an instant (`None` if chrono can't hold it).
fn from_ntp(stamp: u64) -> Option<DateTime<Utc>> {
    let mut seconds = (stamp >> 32) as i64;
    // RFC 4330 section 3: a clear top bit means era 1 (2036 to 2104).
    if seconds & 0x8000_0000 == 0 {
        seconds += 1 << 32;
    }
    let nanos = ((stamp & 0xFFFF_FFFF) * 1_000_000_000) >> 32;
    Utc.timestamp_opt(seconds - NTP_UNIX_OFFSET, nanos as u32)
        .single()
}

/// Helpers for this module's tests and `clock_check`'s.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use chrono::{DateTime, Utc};
    use tokio::net::UdpSocket;

    use super::{to_ntp, PACKET_LEN};
    use crate::clock::Clock;

    /// A clock that returns the given instants in order, one per call.
    pub(crate) struct StepClock(Mutex<VecDeque<DateTime<Utc>>>);

    impl StepClock {
        /// The clock, ready to return `instants` in order.
        pub(crate) fn new(instants: impl IntoIterator<Item = DateTime<Utc>>) -> Self {
            Self(Mutex::new(instants.into_iter().collect()))
        }
    }

    impl Clock for StepClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
                .lock()
                .expect("step clock lock")
                .pop_front()
                .expect("the step clock ran out of instants")
        }
    }

    /// A server that answers one request on `127.0.0.1` with these times.
    ///
    /// # Returns
    /// Its address, for `query`.
    pub(crate) async fn fake_server(
        receive: DateTime<Utc>,
        transmit: DateTime<Utc>,
        stratum: u8,
    ) -> SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind a fake NTP server");
        let address = socket.local_addr().expect("its address");
        tokio::spawn(async move {
            let mut request = [0u8; PACKET_LEN];
            if let Ok((_, client)) = socket.recv_from(&mut request).await {
                let mut reply = [0u8; PACKET_LEN];
                reply[0] = 0b00_100_100; // leap 0, version 4, mode 4 (server)
                reply[1] = stratum;
                reply[24..32].copy_from_slice(&request[40..48]);
                reply[32..40].copy_from_slice(&to_ntp(receive).to_be_bytes());
                reply[40..48].copy_from_slice(&to_ntp(transmit).to_be_bytes());
                // A failed send shows up as the client's timeout.
                let _ = socket.send_to(&reply, client).await;
            }
        });
        address
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{fake_server, StepClock};
    use super::*;

    fn at(millis: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap() + TimeDelta::milliseconds(millis)
    }

    #[test]
    fn timestamps_round_trip_on_both_sides_of_2036() {
        for instant in [
            at(123),
            Utc.with_ymd_and_hms(2040, 1, 1, 0, 0, 0).unwrap() + TimeDelta::microseconds(5),
        ] {
            let back = from_ntp(to_ntp(instant)).unwrap();
            // The 32-bit fraction is finer than a nanosecond is coarse, so
            // the round trip loses at most one nanosecond.
            assert!((back - instant).num_nanoseconds().unwrap().abs() <= 1, "{instant}");
        }
    }

    #[test]
    fn a_request_is_a_version_4_client_packet_with_its_transmit_time() {
        let packet = request(0x0123_4567_89AB_CDEF);
        assert_eq!(packet[0], 0x23);
        assert_eq!(&packet[40..48], &0x0123_4567_89AB_CDEF_u64.to_be_bytes());
        assert!(packet[1..40].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn replies_are_checked() {
        let sent = 42;
        let reply = |first: u8, stratum: u8, originate: u64, transmit: u64| {
            let mut packet = [0u8; PACKET_LEN];
            packet[0] = first;
            packet[1] = stratum;
            packet[24..32].copy_from_slice(&originate.to_be_bytes());
            packet[40..48].copy_from_slice(&transmit.to_be_bytes());
            packet
        };
        let good = reply(0x24, 1, sent, 7);

        assert_eq!(parse_reply(&good, sent).map(|r| r.stratum), Ok(1));
        assert_eq!(parse_reply(&good[..47], sent), Err("shorter than 48 bytes"));
        assert_eq!(parse_reply(&reply(0x23, 1, sent, 7), sent), Err("not a server reply (mode 4)"));
        assert_eq!(
            parse_reply(&reply(0xE4, 1, sent, 7), sent),
            Err("the server is not synchronized (leap indicator 3)")
        );
        assert_eq!(parse_reply(&reply(0x24, 0, sent, 7), sent), Err("a kiss-o'-death reply (stratum 0)"));
        assert_eq!(
            parse_reply(&reply(0x24, 16, sent, 7), sent),
            Err("the server is not synchronized (stratum above 15)")
        );
        assert_eq!(parse_reply(&reply(0x24, 1, 43, 7), sent), Err("its originate time is not this request's"));
        assert_eq!(parse_reply(&reply(0x24, 1, sent, 0), sent), Err("it has no transmit time"));
    }

    #[test]
    fn offset_and_round_trip_follow_rfc_4330() {
        // The server is 5 s ahead; 40 ms each way, 20 ms inside the server.
        let (offset, round_trip) = offset_and_round_trip(at(0), at(5040), at(5060), at(100));
        assert_eq!(offset, TimeDelta::seconds(5));
        assert_eq!(round_trip, TimeDelta::milliseconds(80));
    }

    #[test]
    fn server_names_get_the_ntp_port() {
        assert_eq!(with_port("time.example"), "time.example:123");
        assert_eq!(with_port("time.example:1123"), "time.example:1123");
        assert_eq!(with_port("192.0.2.1"), "192.0.2.1:123");
        assert_eq!(with_port("192.0.2.1:9"), "192.0.2.1:9");
        assert_eq!(with_port("2001:db8::1"), "[2001:db8::1]:123");
        assert_eq!(with_port("[2001:db8::1]:9"), "[2001:db8::1]:9");
    }

    #[tokio::test]
    async fn a_query_measures_the_offset_against_a_server() {
        let server = fake_server(at(5040), at(5060), 1).await;
        let clock = StepClock::new([at(0), at(100)]);

        let reading = query(&server.to_string(), &clock, Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(
            reading,
            Reading {
                stratum: 1,
                offset: TimeDelta::seconds(5),
                round_trip: TimeDelta::milliseconds(80),
            }
        );
    }

    #[tokio::test]
    async fn a_silent_server_times_out() {
        // Bound but never read: the request is dropped unanswered.
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = silent.local_addr().unwrap().to_string();
        let clock = StepClock::new([at(0), at(1)]);

        let error = query(&address, &clock, Duration::from_millis(200))
            .await
            .unwrap_err();

        assert!(matches!(error, NtpError::Timeout { .. }), "{error:?}");
        assert!(error.to_string().contains("did not answer"));
    }

    #[tokio::test]
    async fn an_unsynchronized_server_is_refused() {
        let server = fake_server(at(5040), at(5060), 16).await;
        let clock = StepClock::new([at(0), at(100)]);

        let error = query(&server.to_string(), &clock, Duration::from_secs(5))
            .await
            .unwrap_err();

        assert!(matches!(error, NtpError::BadReply { .. }), "{error:?}");
    }
}
```

Run: `cargo test --lib -- ntp`
Expected (with `todo!()` bodies): compile errors, then panics.

- [ ] **Step 2: Wire the module**

`src/lib.rs`: add `pub mod ntp;` between `pub mod mcp;` and `pub mod okf;`.

- [ ] **Step 3: The implementation**

The code above is the implementation; replace the stubs with it. Run `cargo test --lib -- ntp`. Expected: 8 passed.

- [ ] **Step 4: Gates, changelog, commit**

The changelog names the offset formula and the era rule, and says the 2-second threshold and loop are Task 3's.

```bash
git add src/ntp.rs src/lib.rs changelog/
git commit -m "feat: an SNTPv4 client for the clock check (M7.02)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: the clock check and `/healthz`'s `clock`

The design's "Time", made real. `time_server` is read from the config, and is optional and non-blank. `check_once` measures the offset and logs. `App` keeps the last `ClockStatus`. `App::check_clock_until` runs a check at startup and every 15 minutes until shutdown, and `http::serve` spawns it beside the poll loop. `/healthz` adds `clock`, the last status or `null`, with coarse fields only. Its HTTP status still depends only on the database (Decision 7).

**Files:**
- Create: `src/clock_check.rs`
- Modify: `src/lib.rs` (`pub mod clock_check;`, after `pub mod clock;`)
- Modify: `src/config.rs` (`time_server`, its validation, the module doc, tests)
- Modify: `src/app.rs` (`clock_status`, `set_clock_status`, `check_clock_until`, tests)
- Modify: `src/http/mod.rs` (`serve` spawns the loop; `Health::clock`; a test)
- Modify: `tests/server.rs` (the `/healthz` body now has `"clock": null`)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Time"; the `/healthz` row; "Configuration")
- Create: `changelog/<ts>-m7-task-03-clock-check.md`

- [ ] **Step 1: `time_server` in the config, test first**

In `src/config.rs`'s tests, add:

```rust
    #[test]
    fn time_server_is_optional_and_never_blank() {
        let none = Config::from_toml("").unwrap();
        let named = Config::from_toml("time_server = \"time.example\"\n").unwrap();
        let blank = Config::from_toml("time_server = \"  \"\n").unwrap();

        assert_eq!(none.time_server, None);
        assert_eq!(named.time_server.as_deref(), Some("time.example"));
        assert_eq!(named.validate(), Ok(()));
        assert_eq!(
            blank.validate(),
            Err("time_server is blank; name the NTP server or leave it out".to_string())
        );
    }
```

The existing test whose TOML sets `time_server = "ntp.example"` (near line 188) gains `assert_eq!(config.time_server.as_deref(), Some("ntp.example"));`.

Then the field, after `session_idle_timeout`:

```rust
    /// The NTP server the clock check measures against (`host`, `host:port`
    /// or an address); none means no check (design "Time").
    pub time_server: Option<String>,
```

with `time_server: None,` in `Default`. In `validate`, before `Ok(())`:

```rust
        if self
            .time_server
            .as_deref()
            .is_some_and(|server| server.trim().is_empty())
        {
            return Err("time_server is blank; name the NTP server or leave it out".to_string());
        }
```

`validate`'s `# Errors` gains "a blank `time_server`". The module doc's first paragraph becomes: "Each milestone reads the fields it uses (M7 added `time_server`). Unknown keys are ignored, so one file serves every version of the server."

- [ ] **Step 2: `clock_check`, test first**

Create `src/clock_check.rs`:

```rust
//! The clock check (design "Time"): at startup and every 15 minutes the
//! server measures its offset from `time_server` over SNTP, logs at `error`
//! when the offset is over 2 seconds, and keeps the result for `/healthz`.
//! The check never changes `/healthz`'s status: a time-server outage must not
//! take the server out of service.

use std::time::Duration;

use serde::Serialize;

use crate::clock::{format_timestamp, Clock};
use crate::ntp;

/// How often the check runs after the first one (at startup).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How long one check may take, DNS included.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest offset, in milliseconds, that is not logged as an error.
pub const MAX_OFFSET_MS: i64 = 2_000;

/// The last check, as `/healthz` shows it. Coarse on purpose: the route
/// needs no key and is public, so the server's name and the error text go to
/// the logs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClockStatus {
    /// Whether the server answered with a usable reply.
    pub ok: bool,
    /// When the check ran (a catalog timestamp, from our clock).
    pub checked_at: String,
    /// The server's clock minus ours, in milliseconds; absent on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
    /// The server's stratum; absent on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum: Option<u8>,
}

/// Runs one check and logs its outcome (with the server and the error).
///
/// # Arguments
/// * `server` - the `time_server`.
/// * `clock` - our clock: the check's time, and both ends of the exchange.
/// * `timeout` - the limit for the query ([`QUERY_TIMEOUT`] in the server).
///
/// # Returns
/// The status: an offset and stratum, or `ok: false`.
pub async fn check_once(server: &str, clock: &dyn Clock, timeout: Duration) -> ClockStatus {
    let checked_at = format_timestamp(clock.now());
    match ntp::query(server, clock, timeout).await {
        Ok(reading) => {
            let offset_ms = reading.offset.num_milliseconds();
            if offset_ms.abs() > MAX_OFFSET_MS {
                tracing::error!(server, offset_ms, "the clock is off by more than 2 seconds");
            } else {
                tracing::debug!(server, offset_ms, stratum = reading.stratum, "clock checked");
            }
            ClockStatus {
                ok: true,
                checked_at,
                offset_ms: Some(offset_ms),
                stratum: Some(reading.stratum),
            }
        }
        Err(error) => {
            tracing::warn!(%error, "the clock check failed");
            ClockStatus {
                ok: false,
                checked_at,
                offset_ms: None,
                stratum: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};
    use tokio::net::UdpSocket;

    use super::*;
    use crate::ntp::testing::{fake_server, StepClock};

    fn at(millis: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap() + TimeDelta::milliseconds(millis)
    }

    #[tokio::test]
    async fn a_check_records_the_offset_and_stratum() {
        let server = fake_server(at(5040), at(5060), 1).await.to_string();
        // The check's own time, then both ends of the exchange.
        let clock = StepClock::new([at(0), at(0), at(100)]);

        let status = check_once(&server, &clock, QUERY_TIMEOUT).await;

        assert_eq!(
            status,
            ClockStatus {
                ok: true,
                checked_at: "2026-11-03T12:00:00Z".to_string(),
                offset_ms: Some(5_000),
                stratum: Some(1),
            }
        );
    }

    #[tokio::test]
    async fn a_failed_check_records_why() {
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server = silent.local_addr().unwrap().to_string();
        let clock = StepClock::new([at(0), at(0), at(1)]);

        let status = check_once(&server, &clock, Duration::from_millis(200)).await;

        assert_eq!((status.ok, status.offset_ms, status.stratum), (false, None, None));
    }

    #[test]
    fn a_status_names_no_server_and_no_error() {
        let failed = ClockStatus {
            ok: false,
            checked_at: "2026-11-03T12:00:00Z".to_string(),
            offset_ms: None,
            stratum: None,
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            serde_json::json!({"ok": false, "checked_at": "2026-11-03T12:00:00Z"})
        );
    }
}
```

`src/lib.rs`: add `pub mod clock_check;` after `pub mod clock;`.

Run: `cargo test --lib -- clock_check config`. Expected: pass once Step 1 is in.

- [ ] **Step 3: `App` keeps the status and runs the loop, test first**

In `src/app.rs`, the import `use crate::clock_check::{self, ClockStatus};`. The field, after `overlays`:

```rust
    // The last clock check (design "Time"); `None` until the first one, and
    // always without a `time_server`. Written by the check loop only.
    clock_status: RwLock<Option<ClockStatus>>,
```

`App::load` sets `clock_status: RwLock::new(None),`. The methods, after `overlays()`:

```rust
    /// The last clock check, if any (`/healthz` shows it).
    ///
    /// # Returns
    /// A copy of the status, or `None` without a `time_server` or before the
    /// first check.
    pub fn clock_status(&self) -> Option<ClockStatus> {
        self.clock_status
            .read()
            .expect("clock status lock poisoned")
            .clone()
    }

    /// Records a clock check.
    ///
    /// # Arguments
    /// * `status` - the check's outcome.
    pub fn set_clock_status(&self, status: ClockStatus) {
        *self
            .clock_status
            .write()
            .expect("clock status lock poisoned") = Some(status);
    }

    /// Checks the clock at once and then every [`clock_check::CHECK_INTERVAL`]
    /// until `shutdown` is cancelled. Returns at once without a `time_server`.
    ///
    /// # Arguments
    /// * `shutdown` - cancelled when the server stops.
    pub async fn check_clock_until(self: Arc<Self>, shutdown: CancellationToken) {
        // `let ... else`: return when there is no server to ask.
        let Some(server) = self.config.time_server.clone() else {
            return;
        };
        let mut ticks = tokio::time::interval(clock_check::CHECK_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            // The first tick completes at once: the startup check.
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {}
            }
            let check = clock_check::check_once(
                &server,
                self.clock(),
                clock_check::QUERY_TIMEOUT,
            );
            // A stop during a query ends it at once instead of waiting up to
            // `QUERY_TIMEOUT` (Brooke's review of PR #138).
            tokio::select! {
                () = shutdown.cancelled() => return,
                status = check => self.set_clock_status(status),
            }
        }
    }
```

Tests in `src/app.rs`'s test module (use `crate::mcp::testing::app_with`, as the module's other tests do):

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn the_clock_check_runs_at_startup_and_stops_with_the_server() {
        let now = Utc::now();
        let server = crate::ntp::testing::fake_server(now, now, 1).await;
        let config = Config {
            time_server: Some(server.to_string()),
            ..Config::default()
        };
        let test = app_with(&[("pg", &[])], config).await;
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&test.app).check_clock_until(shutdown.clone()));

        // Wait for the startup check, but not forever.
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = test.app.clock_status() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a clock check within 10 s");
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the loop stops when cancelled")
            .expect("the loop does not panic");

        assert!(status.ok, "{status:?}");
        // Both ends read the system clock: the offset is a few ms at most.
        assert!(status.offset_ms.is_some_and(|ms| ms.abs() < 1_000), "{status:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_time_server_there_is_no_check() {
        let test = app_with(&[("pg", &[])], Config::default()).await;

        // Returns at once, without a cancelled token.
        tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&test.app).check_clock_until(CancellationToken::new()),
        )
        .await
        .expect("returns at once without a time_server");

        assert_eq!(test.app.clock_status(), None);
    }
```

Adjust the imports (`Duration`, `Utc`, `Config`, `CancellationToken`) to what the test module already has.

- [ ] **Step 4: `serve` and `/healthz`**

In `http::serve`, after the poller is spawned:

```rust
    let clock_checker = tokio::spawn(Arc::clone(&app).check_clock_until(shutdown.clone()));
```

and after the poller is awaited:

```rust
    if let Err(error) = clock_checker.await {
        tracing::error!(%error, "the clock check panicked");
    }
```

`Health` gains:

```rust
    /// The last clock check (design "Time"); `null` without a `time_server`
    /// or before the first check. It never changes the status.
    clock: Option<ClockStatus>,
```

and `healthz` sets `clock: app.clock_status(),`. The doc of `healthz` adds "with the last clock check, which never changes the status".

A test in `src/http/mod.rs`'s test module:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn healthz_reports_the_last_clock_check_without_changing_its_status() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let get = || Request::get("/healthz").body(Body::empty()).expect("request");
        let before = router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(get())
            .await
            .unwrap();
        let before = json_body(before).await;
        test.app.set_clock_status(ClockStatus {
            ok: false,
            checked_at: "2026-11-03T12:00:00Z".to_string(),
            offset_ms: None,
            stratum: None,
        });

        let after = router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(get())
            .await
            .unwrap();
        let status = after.status();
        let after = json_body(after).await;

        assert_eq!(before["clock"], serde_json::Value::Null);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(after["database"], "ok");
        assert_eq!(
            after["clock"],
            serde_json::json!({"ok": false, "checked_at": "2026-11-03T12:00:00Z"})
        );
    }
```

`tests/server.rs:54`'s expectation becomes `serde_json::json!({"database": "ok", "catalogs": 1, "clock": null})`.

Run the gates.

- [ ] **Step 5: The design**

- **"Time":** after "logs at `error` if the offset exceeds 2 seconds", add "(and at `warn` when the query fails; the query times out after 5 seconds, DNS included)".
- **"Time":** replace "`/healthz` reports the last offset" with "`/healthz` reports the last check (`clock`: `ok`, `checked_at`, and `offset_ms` and `stratum` when it succeeded; `null` without a `time_server`). The server's name and any error go only to the logs, since `/healthz` is public. It never changes `/healthz`'s status: a time-server outage must not take the server out of service."
- **`/healthz` row:** replace "The clock offset joins it in M7" with "plus `clock`, the last clock check (see "Time"), which does not affect the status".
- **"Configuration":** the sentence "Each milestone reads only its own fields (M3: ...; M4: the rest except `time_server`, which M7's clock check reads)" becomes "...M4: the rest except `time_server`; M7: `time_server`, optional: without it there is no clock check".

- [ ] **Step 6: Changelog and commit**

```bash
git add src/clock_check.rs src/lib.rs src/config.rs src/app.rs src/http/mod.rs tests/server.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: the clock check against time_server, reported by /healthz (M7.03)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: TLS to Postgres

sqlx gets a TLS implementation (`tls-rustls-ring-webpki`). A database test starts Postgres with TLS, using a server certificate from a CA generated at test time. It shows two things:
- `sslmode=verify-full` with that CA connects encrypted;
- the same mode with another CA is refused.

No connection code changes: the deployment sets `PGSSLMODE` and `PGSSLROOTCERT`, which sqlx reads (verified fact 4; Decision 8).

**Files:**
- Modify: `Cargo.toml` (sqlx feature; `rcgen` dev-dependency; their comments)
- Modify: `Cargo.lock`
- Create: `tests/tls.rs`
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` ("Configuration": TLS)
- Create: `changelog/<ts>-m7-task-04-postgres-tls.md`

- [ ] **Step 1: The test**

Create `tests/tls.rs`:

```rust
//! TLS to Postgres (M7): with `sslmode=verify-full` the server connects
//! encrypted to a database whose certificate its CA signed, and refuses one
//! another CA signed. Needs Docker, like every database test. Certificates
//! are generated here, never checked in.

use std::io::Write;

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// Installs the copied key and certificate as the `postgres` user's
/// (Postgres refuses a key it does not own, and the copy is root's), then
/// hands over to the image's entrypoint with TLS on. `exec` keeps Postgres
/// as the container's main process.
const START_WITH_TLS: &str = "install -o postgres -g postgres -m 600 /tmp/server.key /var/lib/postgresql/server.key \
     && install -o postgres -g postgres -m 644 /tmp/server.crt /var/lib/postgresql/server.crt \
     && exec docker-entrypoint.sh postgres -c ssl=on \
     -c ssl_cert_file=/var/lib/postgresql/server.crt \
     -c ssl_key_file=/var/lib/postgresql/server.key";

/// A certificate authority: its certificate (PEM) and what signs with it.
struct Ca {
    pem: String,
    issuer: Issuer<'static, KeyPair>,
}

fn new_ca() -> Ca {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("no names is valid");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().expect("a CA key");
    let pem = params.self_signed(&key).expect("a CA certificate").pem();
    Ca {
        pem,
        issuer: Issuer::new(params, key),
    }
}

/// A certificate for `127.0.0.1` signed by `ca`, and its key (both PEM).
fn server_certificate(ca: &Ca) -> (String, String) {
    let params = CertificateParams::new(vec!["127.0.0.1".to_string()]).expect("an IP name");
    let key = KeyPair::generate().expect("a server key");
    let certificate = params.signed_by(&key, &ca.issuer).expect("a signed certificate");
    (certificate.pem(), key.serialize_pem())
}

async fn postgres_with_tls(certificate: String, key: String) -> (ContainerAsync<Postgres>, u16) {
    let container = Postgres::default()
        .with_tag("17-alpine")
        .with_copy_to("/tmp/server.crt", certificate.into_bytes())
        .with_copy_to("/tmp/server.key", key.into_bytes())
        .with_cmd(["sh", "-c", START_WITH_TLS])
        .start()
        .await
        .expect("start postgres with TLS (is docker reachable?)");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("the container's port");
    (container, port)
}

/// `pem` in a file, removed when the handle drops.
fn pem_file(pem: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("a temporary file");
    file.write_all(pem.as_bytes()).expect("write the CA");
    file
}

fn verify_full_url(port: u16, root: &std::path::Path) -> String {
    format!(
        "postgres://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=verify-full&sslrootcert={}",
        root.display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_full_connects_encrypted_to_a_server_its_ca_signed() {
    let ca = new_ca();
    let (certificate, key) = server_certificate(&ca);
    let (_container, port) = postgres_with_tls(certificate, key).await;
    let root = pem_file(&ca.pem);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&verify_full_url(port, root.path()))
        .await
        .expect("connect with verify-full");
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .expect("read pg_stat_ssl");

    assert!(encrypted, "the connection is not encrypted");
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_full_refuses_a_server_another_ca_signed() {
    let ca = new_ca();
    let stranger = new_ca();
    let (certificate, key) = server_certificate(&ca);
    let (_container, port) = postgres_with_tls(certificate, key).await;
    let root = pem_file(&stranger.pem);

    let refused = PgPoolOptions::new()
        .max_connections(1)
        .connect(&verify_full_url(port, root.path()))
        .await;

    assert!(refused.is_err(), "connected to a server the CA did not sign");
}
```

Run: `cargo test --test tls`
Expected: compile error on `rcgen` (not a dependency yet). Once Step 2 adds rcgen but not the TLS feature, the first test fails: sqlx without a TLS implementation can't do `verify-full`.

- [ ] **Step 2: The dependencies**

In `Cargo.toml`, sqlx's features gain `"tls-rustls-ring-webpki"`. Its comment's last sentence ("No TLS feature yet: that is a deployment decision (M7).") becomes: "`tls-rustls-ring-webpki`: TLS in pure Rust (no OpenSSL in the image). The deployment turns it on with `PGSSLMODE=verify-full` and `PGSSLROOTCERT`, which sqlx reads beneath the URL (M7)."

In `[dev-dependencies]`:

```toml
# Certificates generated at test time for the TLS database test (never
# checked in: a key in the repository would trip secret scanners).
rcgen = "0.14.10"
```

Run: `cargo test --test tls`. Expected: 2 passed. Then run every gate; the lockfile changes.

If the container doesn't start (`install` or permissions), read its log with `docker logs <id>` while the test waits. Then fix the start script, and correct this plan with a dated note.

- [ ] **Step 3: The design**

In "Configuration", after "The only secret is `DATABASE_URL` ...": "TLS to Postgres is turned on by the environment, as for libpq: `PGSSLMODE=verify-full` and `PGSSLROOTCERT` (a file with the database's CA certificate). sqlx reads them beneath the URL, so the URL itself needs no `sslmode`. The image's sqlx uses rustls."

- [ ] **Step 4: Changelog and commit**

```bash
git add Cargo.toml Cargo.lock tests/tls.rs docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "feat: TLS to Postgres (rustls), verified against a test CA (M7.04)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: the container image and its workflow

A multi-stage `Dockerfile`:
- **Build stage:** `rust:1.99.1-slim-bookworm`, matching the toolchain, with BuildKit cache mounts for the registry and `target/`.
- **Runtime stage:** `debian:bookworm-slim` with `ca-certificates`, a non-root user (uid 10001), `ENTRYPOINT ["/usr/local/bin/okf-mcp"]` and `CMD ["serve"]`.

`.github/workflows/image.yml` builds and pushes to GHCR on a `v*` tag or a manual dispatch (Decision 6). The version becomes `0.7.0` (Decision 13). The image can't be pushed from here: Task 7 dispatches the workflow once it is on `main`.

**Files:**
- Create: `Dockerfile`, `.dockerignore`, `.github/workflows/image.yml`
- Modify: `Cargo.toml`, `Cargo.lock` (version `0.7.0`)
- Create: `changelog/<ts>-m7-task-05-image.md`

- [ ] **Step 1: `.dockerignore`**

```
target
.git
.worktrees
.claude
.github
changelog
docs
tests
```

`tests/` is left out of the build context: `cargo build --bin okf-mcp` doesn't build test targets, and auto-discovery doesn't need them.

- [ ] **Step 2: `Dockerfile`**

```dockerfile
# syntax=docker/dockerfile:1
# The okf-mcp server image (M7). Built by .github/workflows/image.yml and
# published to ghcr.io/petergrace/okf-mcp; see docs/operations.md.

# Build stage: the same Rust as the workstation's toolchain (1.99.1). Bump both
# together.
FROM rust:1.99.1-slim-bookworm AS build
WORKDIR /build
COPY . .
# Cache mounts keep the crate registry and target/ between builds on one
# builder. target/ is a cache, not a layer, so the binary is copied out of it.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin okf-mcp \
    && cp target/release/okf-mcp /usr/local/bin/okf-mcp

# Runtime stage: no git and no OpenSSL (sqlx uses rustls); CA certificates
# for completeness. A fixed, non-root uid that manifests can name.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin okf
COPY --from=build /usr/local/bin/okf-mcp /usr/local/bin/okf-mcp
USER 10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/okf-mcp"]
CMD ["serve"]
```

- [ ] **Step 3: `.github/workflows/image.yml`**

```yaml
# Builds the server image and publishes it to GHCR (M7). Not CI: no gates
# run here; they run locally before each merge (design "Milestones").
name: image

on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: read
  packages: write

jobs:
  image:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: docker/setup-buildx-action@f87e5991a6d7451dcb8d9637bfbc97413f497069 # v4.4.1
      - uses: docker/login-action@81c6f93d4bc991e510b342a00b419b3bb4efc5c6 # v4.7.0
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - id: meta
        uses: docker/metadata-action@dc802804100637a589fabce1cb79ff13a1411302 # v6.2.0
        with:
          # GHCR names are lower case.
          images: ghcr.io/petergrace/okf-mcp
          tags: |
            type=semver,pattern={{version}}
            type=sha
      - uses: docker/build-push-action@c3c9e263c25d99ce0380d002d59b67737d91b0dc # v7.4.0
        with:
          context: .
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
```

Check that it parses: `python3 -c 'import sys,yaml; yaml.safe_load(open(".github/workflows/image.yml"))'`. If `actionlint` is available, run it too.

- [ ] **Step 4: The version**

`Cargo.toml`: `version = "0.7.0"`. Run `cargo build` so `Cargo.lock` follows. `okf-mcp --version` prints `okf-mcp 0.7.0`.

- [ ] **Step 5: Build and run it locally**

Container names carry `i<issue>` so they never collide with another run's, and you stop only what you started:

```bash
docker build -t okf-mcp:i<issue> .
docker run --rm okf-mcp:i<issue> --version         # okf-mcp 0.7.0
docker run -d --name okf-pg-i<issue> -e POSTGRES_PASSWORD=pw -p 127.0.0.1::5432 postgres:17-alpine
PGPORT=$(docker port okf-pg-i<issue> 5432 | head -1 | cut -d: -f2)
sleep 5
docker run -d --name okf-mcp-i<issue> --network host \
  -e DATABASE_URL=postgres://postgres:pw@127.0.0.1:$PGPORT/postgres \
  okf-mcp:i<issue> serve --listen 127.0.0.1:18080
sleep 3
curl -s http://127.0.0.1:18080/healthz          # {"database":"ok","catalogs":0,"clock":null} once T3 merged; without "clock" before
docker rm -f okf-mcp-i<issue> okf-pg-i<issue>
```

Paste the outputs into the PR. Also record the image's size (`docker image ls okf-mcp:i<issue>`) and the build's time.

- [ ] **Step 6: Changelog and commit**

The changelog says what the workflow publishes and when, and that Task 7 runs it first.

```bash
git add Dockerfile .dockerignore .github/workflows/image.yml Cargo.toml Cargo.lock changelog/
git commit -m "build: the server image and its GHCR workflow; version 0.7.0 (M7.05)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: operator docs

`docs/operations.md`, for anyone running the server, with nothing site-specific (Decision 5). The README links to it. Every command must be one that works with the merged code. Run each subcommand's `--help` to check flags.

**Files:**
- Create: `docs/operations.md`
- Modify: `README.md` (a "Running it" link to the operator docs, replacing any deployment text the M4 to M6 READMEs carry)
- Create: `changelog/<ts>-m7-task-06-operations.md`

- [ ] **Step 1: Write `docs/operations.md`** with these sections, in this order:

1. **What runs:**
   - one process (`okf-mcp serve`), one replica, Postgres as the only state;
   - the routes (`/mcp`, `/catalogs/{name}/export`, `/catalogs/{name}/import`, `/healthz`), each with the access it needs;
   - why one replica: the publish lock is per process.
2. **The image:**
   - `ghcr.io/petergrace/okf-mcp:<version>`, built by `.github/workflows/image.yml` on a `v*` tag;
   - the uid (10001), the entrypoint and the default command;
   - how to build locally (`docker build`).
3. **Configuration:**
   - the TOML file (`--config` or `OKF_MCP_CONFIG`) with every key, its default and its meaning: `listen`, `allowed_hosts`, `default_catalog`, `session_idle_timeout`, `poll_interval`, `max_import_bytes`, `time_server`;
   - the environment: `DATABASE_URL`, `PGSSLMODE`, `PGSSLROOTCERT`, `RUST_LOG`.

   Also:
   - **IPv6:** on an IPv6 or dual-stack cluster, listen on `[::]:8080`.
   - **Behind an ingress:** `allowed_hosts` must name the public host.
4. **Postgres:**
   - one database and an owner role;
   - migrations run on every start (and with `okf-mcp migrate`);
   - TLS with `verify-full`: mount the database's CA certificate and set the two variables;
   - a CloudNativePG example: the `<cluster>-app` secret's `uri`, and the `<cluster>-ca` secret's `ca.crt`;
   - backups belong to the database (for CloudNativePG, the barman-cloud plugin with an object store).
5. **Kubernetes:**
   - the objects, in a generic, complete example with `example.com` names: a Deployment, a ConfigMap with the config, a Service, an Ingress and a CNPG `Cluster`.
   - The Deployment carries:
     - one replica and `Recreate`;
     - the probes of Decision 10;
     - `securityContext` (`runAsNonRoot`, `readOnlyRootFilesystem`, an `emptyDir` on `/tmp` for imports copied into the pod, all capabilities dropped);
     - modest resources;
     - the env of section 3.
   - The Ingress annotations of Decision 11, with why each is there.
6. **First run:**
   - create a catalog: `kubectl exec ... -- okf-mcp catalog create NAME`;
   - create keys: `okf-mcp key create ID --access ... --catalogs ...|--all`. It prints the key once; store it, and never paste it into logs or tickets;
   - import: copy a `.tar.gz` into `/tmp` and run `okf-mcp import NAME /tmp/x.tar.gz`, or use the HTTP route with an admin key;
   - connect a harness: the MCP URL `https://<host>/mcp` and the `Authorization: Bearer` header, with `X-OKF-Changeset` and `?catalogs=` as the M4/M5 docs describe;
   - the protocol versions the server accepts.
7. **Health and time:**
   - `/healthz`'s body and status;
   - the clock check: what `clock` means, what the `error` log line looks like ("the clock is off by more than 2 seconds") and the `warn` on failure;
   - that a failing check does not fail readiness.
8. **Upgrades:** build a new tag, change the image tag in the manifests, and let the Deployment recreate the pod. Migrations run on start and are forward-only, and the release notes are the changelog.
9. **Backup and restore:**
   - what to back up: the database only;
   - for CloudNativePG with barman-cloud: the `ObjectStore`, WAL archiving, a `ScheduledBackup`, an on-demand backup (`kubectl cnpg backup ... --method plugin --plugin-name barman-cloud.cloudnative-pg.io`);
   - a restore into a new `Cluster` from the object store, with that cluster's own archiving off so it doesn't write into the source's path;
   - the export route as an extra copy of each catalog.

   Task 8 confirms or corrects this section after the drill.
10. **Troubleshooting:**
    - 401/403/404 on `/mcp` (key, grants, session bound to another key);
    - 413 on import (`max_import_bytes`, and the ingress body size);
    - 403 for a wrong `Host` (`allowed_hosts`);
    - `/healthz` 503 (database);
    - TLS errors (`PGSSLROOTCERT` and the host name).

- [ ] **Step 2: README** — a short "Running it" section that links to `docs/operations.md`, and drops any duplicated deployment detail.

- [ ] **Step 3: Check the commands**: each `okf-mcp` command in the doc against `cargo run -- <cmd> --help`. Each YAML example parses (`python3 -c 'import yaml,sys; list(yaml.safe_load_all(open(...)))'` on extracted blocks, or `kubeconform -ignore-missing-schemas` if you write them to a scratch dir).

- [ ] **Step 4: Changelog and commit**

```bash
git add docs/operations.md README.md changelog/
git commit -m "docs: operator guide (M7.06)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: rollout

This task changes other systems. Its PR in this repo carries only:
- its changelog: what was done, with nothing site-specific;
- corrections to `docs/operations.md` or this plan that the rollout shows are needed.

**Every outward step waits for pgrace's go-ahead in the session** (Decision 4). Steps that create or handle secret values are run by pgrace, from commands you give (Decision 3). Take the site values (object store endpoint, Vault conventions, image mirror paths, cluster issuer) from the private catalog's infrastructure concepts, and never write them into this repo.

**Files (this repo):**
- Create: `changelog/<ts>-m7-task-07-rollout.md`
- Modify (if needed): `docs/operations.md`, this plan

**Files (`argo2025`, private):**
- `kustomizations/okf-mcp/kustomization.yaml`, `pg-cluster.yaml`, `objectstore.yaml`, `scheduledbackup.yaml`, `extsec.pgbackups.yaml`, `configmap.yaml`, `deployment.yaml`, `service.yaml`, `ingress.yaml`
- `kustomizations/app-okf-mcp.yaml`
- `kustomizations/kustomization.yaml` (one line)

- [ ] **Step 1: Preconditions.**
  - T1 and T3 to T6 are merged, and `main` is green on the gates.
  - Release tag:
    - `git describe` on `main` shows no `v0.7.0` yet;
    - `Cargo.toml` says `0.7.0`.

- [ ] **Step 2: First image (go-ahead 1).**
  - Ask pgrace to OK the image build and the release tag.
  - Run the workflow on `main` first (`gh workflow run image.yml --ref main`) and watch it (`gh run watch`). It publishes `:sha-<short>` only.
  - Then tag `v0.7.0` on `main` and push the tag. The workflow publishes `:0.7.0`.
  - Check that the package is anonymous-readable through the cluster's cache: `docker pull r.gfpd.us/ghcrio/petergrace/okf-mcp:0.7.0`.
  - If the pull fails because the package is private, ask pgrace to set the package's visibility to public (package settings on GitHub), then pull again.

- [ ] **Step 3: Backup bucket and key (pgrace runs them).** Give pgrace the commands, adapted from the private catalog's object-store concept:
  - create a bucket for this server's backups;
  - create a key;
  - allow it read and write on that bucket only;
  - put the key ID and secret into Vault under a new `okf-mcp` entry, with the two property names the `ExternalSecret` will read.

  You never see the values. Confirm only that the Vault entry exists, by asking pgrace or by a later sync of the `ExternalSecret`.

- [ ] **Step 4: Write the manifests in a fresh `argo2025` checkout** (`git -C ~/repos/argo2025 pull --rebase` first; don't disturb unrelated local changes). Follow `kustomizations/sish` and `helm-apps/config/autobots` for conventions:
  - **`kustomization.yaml`:** `namespace: okf-mcp`, listing the files.
  - **`pg-cluster.yaml`:**
    - `Cluster okf-mcp-db`, 3 instances;
    - `imageName` the mirror path of the same PostgreSQL image autobots uses;
    - `bootstrap.initdb {database: okf_mcp, owner: okf_mcp}`, `storage.size: 10Gi`;
    - `plugins: [{name: barman-cloud.cloudnative-pg.io, isWALArchiver: true, parameters: {barmanObjectName: okf-mcp-backup, serverName: okf-mcp-db}}]`;
    - the `safe-to-evict` annotation autobots sets.
  - **`objectstore.yaml`:** `ObjectStore okf-mcp-backup` with the bucket, the endpoint, `s3Credentials` from secret `okf-mcp-pgbackups`, gzip for data and WAL, and `retentionPolicy: "30d"`. Use `sync-wave: "-1"`.
  - **`scheduledbackup.yaml`:** daily, `method: plugin`, `backupOwnerReference: self`. Pick a time other than the existing clusters' 08:05, e.g. `0 35 8 * * *`.
  - **`extsec.pgbackups.yaml`:** `ExternalSecret` to secret `okf-mcp-pgbackups`, with the two keys from Vault's `okf-mcp` entry. Use `sync-wave: "-1"`.
  - **`configmap.yaml`:** `config.toml` with:
    - `listen = "[::]:8080"`;
    - `allowed_hosts = ["okf-mcp.k.vsix.me"]`;
    - `default_catalog = "pg"`;
    - `time_server` = the network's time server name;
    - `session_idle_timeout`, `poll_interval` and `max_import_bytes` at their defaults.
  - **`deployment.yaml`:**
    - one replica, `strategy: Recreate`, image `r.gfpd.us/ghcrio/petergrace/okf-mcp:0.7.0`, `args: ["--config", "/etc/okf-mcp/config.toml", "serve"]`;
    - env: `DATABASE_URL` from `okf-mcp-db-app` key `uri`, `PGSSLMODE=verify-full`, `PGSSLROOTCERT=/etc/okf-mcp/pg-ca/ca.crt`, `RUST_LOG=info`;
    - volumes: the ConfigMap, secret `okf-mcp-db-ca` (item `ca.crt`), and an `emptyDir` on `/tmp`;
    - the probes and `securityContext` of Decision 10 and `docs/operations.md`;
    - resources: requests `100m`/`256Mi`, limit `1Gi` memory.
  - **`service.yaml`:** ClusterIP, port 8080, `ipFamilyPolicy: PreferDualStack`.
  - **`ingress.yaml`:** class `nginx`, `cert-manager.io/cluster-issuer: letsencrypt-prod`, the annotations of Decision 11, host `okf-mcp.k.vsix.me`, TLS secret `okf-mcp-tls`.
  - **`app-okf-mcp.yaml`:** as `app-sish.yaml`, with name and path `okf-mcp` and sync-wave `"20"`. Add it to `kustomizations/kustomization.yaml`.

- [ ] **Step 5: Validate**:
  - `kustomize build kustomizations/okf-mcp | kubeconform -strict -ignore-missing-schemas -summary`;
  - `kubectl --context default apply --dry-run=server -k kustomizations/okf-mcp`. The namespace doesn't exist yet, so expect namespace errors; any other error is real.

- [ ] **Step 6: Commit (go-ahead 2).**
  - Show pgrace the full diff and the validation output, and wait for the OK.
  - Then commit `okf-mcp: deploy v0.7.0 (okf_mcp M7)`, `git pull --rebase`, and push to `main`.

- [ ] **Step 7: Watch it come up**, without restarting anything that isn't ours:
  - **Argo:** the `okf-mcp` app reaches Synced and Healthy.
  - **Database:** `okf-mcp-db` is 3/3 with `ContinuousArchiving` True.
  - **Server:** the pod is Ready, and the certificate is issued.
  - **`/healthz`:** `curl -s https://okf-mcp.k.vsix.me/healthz` gives `{"database":"ok","catalogs":0,"clock":{...}}`. `clock.ok` is true and `clock.offset_ms` is small. If `clock.ok` is false, read the pod's `warn` line for the cause. If it is DNS, set `time_server` to the address; that value stays in the private ConfigMap and never reaches `/healthz`.
  - **TLS:** on the database's primary, every `okf_mcp` connection is encrypted: `SELECT bool_and(ssl) FROM pg_stat_ssl JOIN pg_stat_activity USING (pid) WHERE usename = 'okf_mcp'` is true.
  - **Backup:** take an on-demand backup with the plugin method and wait for `completed`. If it fails like the other cluster's (kubelet 502), record the node and error in the private catalog's things-to-fix and tell pgrace. Don't work around it.

- [ ] **Step 8: The first catalog (go-ahead 3 for the import; pgrace for the key).**
  - **Catalog:** `kubectl --context default -n okf-mcp exec deploy/okf-mcp -- okf-mcp --config /etc/okf-mcp/config.toml catalog create pg --description "pgrace knowledge catalog (copy; the git catalog stays the source of truth)"`.
  - **Import:**
    - make the archive: `git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz -o <scratch>/pg.tar.gz HEAD`;
    - copy it in: `kubectl cp` to the pod's `/tmp/pg.tar.gz`;
    - import: `kubectl exec ... -- okf-mcp import pg /tmp/pg.tar.gz`;
    - remove the archive from `/tmp`.
  - **Key:** pgrace creates their admin key with `! kubectl ... exec deploy/okf-mcp -- okf-mcp key create pgrace-admin --access admin --all --default pg --actor pgrace` and keeps it.
  - **Check:** `/healthz` shows `"catalogs": 1`.

- [ ] **Step 9: An end-to-end check with pgrace's key.**
  - **Export round trip:** pgrace runs `! curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" https://okf-mcp.k.vsix.me/catalogs/pg/export -o <scratch>/export.tar.gz`. You compare its files with `git archive`'s, byte for byte and mode for mode, as M3's roundtrip test does.
  - **MCP:** pgrace adds the server to a harness (e.g. `claude mcp add --transport http okf https://okf-mcp.k.vsix.me/mcp --header "Authorization: Bearer ..."`) and runs `list_catalogs` and one `search`.
  - **Record** what each returned.

- [ ] **Step 10: Record it.**
  - **Private catalog:**
    - `projects/okf-mcp-server.md`: deployed, with the URL, namespace, database and backups, as a dated line plus a `log.md` entry;
    - the cluster concept's CNPG list gains `okf-mcp`;
    - the object-store concept's bucket table gains the new bucket.
  - **This repo:** the task changelog, with nothing site-specific.
  - Commit and open the PR.

---

### Task 8: backup restore drill

A backup isn't a backup until it has been restored. Restore the newest base backup plus WAL into a scratch CNPG cluster in namespace `okf-mcp`. Compare it with the live database, then delete the scratch cluster. It is created by hand, not through Argo, because it is temporary (go-ahead 4).

**Files:**
- Modify: `docs/operations.md` (the restore section, confirmed or corrected)
- Create: `changelog/<ts>-m7-task-08-restore-drill.md`

- [ ] **Step 1: A fresh backup.** Take an on-demand backup and wait for `completed`. Note the live database's counts:

```sql
SELECT (SELECT count(*) FROM catalogs) AS catalogs,
       (SELECT count(*) FROM documents) AS documents,
       (SELECT count(*) FROM revisions) AS revisions,
       (SELECT max(created_at) FROM revisions) AS last_revision;
```

- [ ] **Step 2: The scratch cluster (go-ahead 4).** Show pgrace the manifest, then `kubectl apply` it:
  - `Cluster okf-mcp-db-restore`, 1 instance, the same `imageName`;
  - `bootstrap.recovery.source: origin`;
  - `externalClusters: [{name: origin, plugin: {name: barman-cloud.cloudnative-pg.io, parameters: {barmanObjectName: okf-mcp-backup, serverName: okf-mcp-db}}}]`;
  - **no `plugins` section**, so it never archives into the source's path;
  - `storage.size: 10Gi`.

- [ ] **Step 3: Compare.** When it is Ready, run the same query on it and, at the same moment, on the live database.
  - **The restore replays the archived WAL to its end**, so it should match the live database as of the last archived segment, not only the counts taken before the backup.
  - **Hold writes for the drill.** Ask pgrace not to publish while it runs. Then the restored counts must equal the live ones.
  - **If the live database moved anyway:** require pre-backup ≤ restored ≤ live for each count, and explain any gap from the WAL archive's timing (Brooke's review of PR #138). Never accept fewer rows than were taken before the backup.
  - **Content:** check that one document's content matches by comparing `sha256(content)` for a chosen path on both.

- [ ] **Step 4: Clean up.** Delete the scratch `Cluster` and its PVCs (`kubectl delete cluster okf-mcp-db-restore`; CNPG removes its PVCs). Confirm nothing named `okf-mcp-db-restore*` is left.

- [ ] **Step 5: Docs and records.**
  - **This repo:** `docs/operations.md`'s restore section gets the steps as they actually worked, generic. The changelog gets the timings: backup duration, restore duration, counts.
  - **Private catalog:** a playbook concept, "Restore okf-mcp's database from backup", linked from the project concept, with the site values.
  - Commit and open the PR.

---

### Task 9: README, design and the milestone changelog

**Files:**
- Modify: `README.md` (status: M7 delivered; the deployed URL as a status line; the operator docs)
- Modify: `docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md` (header status; "Deployment"; Milestones row M7 "(delivered)" with the PR range; a new "What M7 shipped differently")
- Create: `changelog/<ts>-m7-deployment.md` (milestone) and `changelog/<ts>-m7-task-09-wrap-up.md`

- [ ] **Step 1: The design.**
  - **"Deployment":**
    - the image is `ghcr.io/petergrace/okf-mcp`, built by `.github/workflows/image.yml` on tags with the workflow's token, and pulled through Harbor's ghcr.io cache, which Kyverno enforces;
    - the manifests are only in the Argo CD source repository (this repo is public);
    - TLS to Postgres (`verify-full`);
    - backups with plugin-barman-cloud to an object store, with a restore drill done;
    - the clock check.
  - **"What M7 shipped differently":** Decisions 5 to 9 and 12, plus every dated `Corrected` note from M7's tasks. Each item names its issue, as M6's section does.

- [ ] **Step 2: The milestone changelog**, shaped like M6's:
  - what shipped;
  - the PRs;
  - deviations, found while building, and plan corrections;
  - gates at the end, with counts against M6's 611;
  - the release smoke test (`cargo test --release --test smoke -- --ignored`, all five, with timings);
  - the production checks from Tasks 7 and 8, generic: healthz, TLS, backup and restore timings, export round trip;
  - follow-ups.

  Every number comes from a command you ran. Mark anything not run as skipped.

- [ ] **Step 3: Commit.**

```bash
git add README.md docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md changelog/
git commit -m "docs: M7 wrap-up: README, design, milestone changelog (M7.09)" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

## Self-review (2026-11-03)

- **Coverage of the design's M7 row:**

  | Design item | Task |
  |---|---|
  | Dockerfile | T5 |
  | k8s manifests | T7 (`argo2025`, Decision 5) |
  | CNPG database | T7, with backups (T7) and a restore (T8) |
  | NTP check | T2, T3 |
  | Operator docs | T6 |
  | M5/M6 deferral "TLS to Postgres" | T4 |
  | M6 follow-up "`StillLinked` slash" | T1 |
- **No placeholders in code steps.** T6 to T8 are prose and commands by nature; their content is listed section by section.
- **Names across tasks:**
  - `ntp::query`, `ntp::testing::{fake_server, StepClock}` (T2), used by T3;
  - `ClockStatus`, `check_once`, `CHECK_INTERVAL`, `QUERY_TIMEOUT`, `MAX_OFFSET_MS` (T3);
  - `App::{clock_status, set_clock_status, check_clock_until}` (T3);
  - the `okf-mcp-db`, `okf-mcp-db-app`, `okf-mcp-db-ca`, `okf-mcp-backup` and `okf-mcp-pgbackups` names (T7, T8).
- **Nothing site-specific in this file:** no internal address, bucket name or Vault path. The host name `okf-mcp.k.vsix.me` and the mirror path are public names.
