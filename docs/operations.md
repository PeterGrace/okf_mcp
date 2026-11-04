# Running okf-mcp

This guide is for anyone running the server: what it is made of, how to
configure it, a complete Kubernetes example, the first run, health, upgrades,
backups and troubleshooting. Every name in it is a placeholder: the host is
`okf-mcp.example.com`, the namespace and database are `okf-mcp`, and "the
site's object store" is whatever S3-compatible store your backups go to.

The design (`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`) says
why the server is built this way; the README describes the tools, the
changesets and the routes in detail.

## 1. What runs

- **One process**, `okf-mcp serve`, in **one replica**. Postgres is the only
  state: catalogs, revisions, changesets and API keys all live in the database.
  The pod needs no persistent volume.
- **Why one replica:** `publish` takes each catalog's publish lock inside the
  process, and the poll loop and snapshot cache are per process too. Two
  replicas could publish to one catalog at the same time. Don't scale it out.
- **Routes:**

  | Route | Access | Does |
  |---|---|---|
  | `GET /healthz` | none | The database's state, the catalog count and the last clock check (section 7) |
  | `POST/GET/DELETE /mcp` | any key (`read`, `write` or `admin`) | MCP streamable HTTP; each tool checks its own level and the key's catalog grants |
  | `GET /catalogs/{name}/export` | `read` on the catalog | The catalog as a `.tar.gz` |
  | `POST /catalogs/{name}/import[?replace=true]` | `admin` on the catalog | Imports an uploaded `.tar.gz` |

  Keys are bearer tokens (`Authorization: Bearer <key>`). Only `/healthz` is
  open, and it names no catalog, host or secret.
- **The command line** (`okf-mcp catalog`, `key`, `import`, `export`,
  `migrate`) talks to the database directly, not to the server. The running
  server notices catalogs, imports and key changes within `poll_interval`.

## 2. The image

- **Name:** `ghcr.io/petergrace/okf-mcp:<version>`, for example `:0.7.0`.
  Every build is also tagged `:sha-<short commit>`. There is no `:latest`:
  manifests pin a version.
- **Built by** `.github/workflows/image.yml` when a `v*` tag is pushed (`v0.7.0`
  gives `:0.7.0`), and on a manual dispatch (which publishes `:sha-<short>`
  only). The workflow runs no tests; they run before each merge.
- **Contents:** Debian bookworm-slim with `ca-certificates`, and the binary at
  `/usr/local/bin/okf-mcp`. No git. The binary uses rustls and links no
  OpenSSL.
- **User:** uid `10001`, non-root (`USER 10001`, numeric, so
  `runAsNonRoot` can check it).
- **Entrypoint and command:** `ENTRYPOINT ["/usr/local/bin/okf-mcp"]`,
  `CMD ["serve"]`. Override the command to run a subcommand
  (`["migrate"]`), or pass flags before it (`["--config", "/etc/okf-mcp/config.toml", "serve"]`).
  The image exposes port 8080.
- **Base images float on purpose.** The Dockerfile names `rust:1.99.1-slim-bookworm`
  and `debian:bookworm-slim` by tag, not digest, so a new build picks up
  Debian's security fixes. A pushed release keeps its digest. Never rebuild an
  old tag: to ship a security fix, release a new version.

Build locally (BuildKit, which recent Docker uses by default):

```sh
docker build -t okf-mcp:dev .
docker run --rm okf-mcp:dev --version
```

## 3. Configuration

### The configuration file

TOML, named by `--config` or `OKF_MCP_CONFIG`. Every key is optional, and
unknown keys are ignored, so one file serves every version.

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"0.0.0.0:8080"` | The address `serve` listens on. `--listen` or `OKF_MCP_LISTEN` overrides it |
| `allowed_hosts` | `["localhost", "127.0.0.1", "::1"]` | The `Host` names (optionally `host:port`) that `/mcp` accepts: rmcp's DNS-rebinding check. Behind an ingress, the public host name |
| `default_catalog` | none | The catalog a key with no default of its own reads and writes |
| `session_idle_timeout` | `"2h"` | An idle MCP session is closed after this; an open changeset with no change within it is `idle` in `list_changesets` |
| `poll_interval` | `"10s"` | How often the server looks for catalog, import and key changes made by the subcommands |
| `max_import_bytes` | `"64MiB"` | The largest uncompressed import (`MiB` is binary, `MB` decimal, or a number of bytes). The import route caps the request body at this plus 0.1% and 64 KiB of gzip slack |
| `time_server` | none | The NTP server for the clock check (`host`, `host:port` or an address). Without it there is no check |

The server refuses to start (and every subcommand refuses to run) with a zero
`poll_interval` or `session_idle_timeout`, an empty `allowed_hosts` or a blank
entry in it, or a `time_server` that is blank or has surrounding whitespace.

```toml
listen = "[::]:8080"
allowed_hosts = ["okf-mcp.example.com"]
default_catalog = "notes"
time_server = "time.example.com"
session_idle_timeout = "2h"
poll_interval = "10s"
max_import_bytes = "64MiB"
```

- **IPv6:** on an IPv6 or dual-stack cluster, listen on `[::]:8080`. Linux
  binds it dual-stack, so IPv4 still works. With the default `0.0.0.0:8080`,
  probes and traffic that reach the pod's IPv6 address are refused, and the
  pod never becomes ready.
- **Behind an ingress:** `allowed_hosts` must name the public host. With the
  default loopback list, every `/mcp` request through the ingress gets 403.

### The environment

| Variable | Meaning |
|---|---|
| `DATABASE_URL` | The Postgres URL (or `--database-url`). Never logged; `--help` hides its value |
| `OKF_MCP_CONFIG` | The configuration file (or `--config`) |
| `OKF_MCP_LISTEN` | Overrides `listen` (or `serve --listen`) |
| `PGSSLMODE` | TLS to Postgres; `verify-full` in production (section 4) |
| `PGSSLROOTCERT` | The CA certificate file that `verify-full` checks the database's certificate against |
| `RUST_LOG` | The log filter. **Leave it unset:** the default is `warn,okf_mcp=info,tower_http=info`, which logs the server's own events and one line per request. Setting `info` everywhere adds noise from every dependency |

Logs go to stderr. They never carry the database URL, an API key or a
document's text.

Set `OKF_MCP_CONFIG` in the container's environment rather than passing
`--config` as an argument: then `kubectl exec ... -- okf-mcp <subcommand>`
reads the same file as the server, with no extra flags.

## 4. Postgres

- **One database and its owner role**, for example `okf_mcp` owned by
  `okf_mcp`. The server needs no superuser and no extension. The pool opens up
  to 8 connections.
- **Migrations** are embedded in the binary and run on every start, by `serve`
  and by every subcommand. `okf-mcp migrate` applies them and exits, for a
  separate deploy step if you want one. They are forward-only: a binary older
  than the schema refuses to start.

### TLS

Turn on verified TLS with two environment variables, which sqlx reads
beneath the URL:

```sh
PGSSLMODE=verify-full
PGSSLROOTCERT=/etc/okf-mcp/pg-ca/ca.crt   # the database's CA certificate, mounted as a file
```

- `verify-full` checks the certificate against the CA and the URL's host name
  against the certificate's names.
- **The URL must not name `sslmode` or `sslrootcert`.** Parameters in the URL
  override the environment, so `?sslmode=prefer` in the URL would silently
  turn verification off. Check the URL for those parameter names without
  printing it, since it carries the password.
- **Spell it exactly.** sqlx ignores a `PGSSLMODE` it cannot parse (say,
  `verify_full`) and falls back to the default below, without a warning.
- **Without `PGSSLMODE`**, sqlx's default is `sslmode=prefer`: it tries TLS
  without verifying the certificate, and falls back to plain text if the
  server has no TLS. That is fine on a laptop, not in production.
- **Developers:** with `PGSSLMODE=verify-full` exported in your shell, the
  plain database tests fail, because their throwaway Postgres has no TLS and
  sqlx reads the variable beneath their URLs. Unset it, or set
  `PGSSLMODE=prefer`, before `cargo test`.

### CloudNativePG

With a CloudNativePG `Cluster` named `okf-mcp-db`:

- `DATABASE_URL` is the `okf-mcp-db-app` secret's `uri` key. Its host is
  `okf-mcp-db-rw`, which the server certificate names, so `verify-full` holds
  inside the namespace.
- `PGSSLROOTCERT` is the `okf-mcp-db-ca` secret's `ca.crt` key, mounted as a
  file. Mount only that item: the secret also holds the CA's private key,
  which the server never needs.
- CloudNativePG's default `pg_hba` also accepts plain connections, so TLS is
  the client's choice: set `PGSSLMODE`.

### Backups

Backups belong to the database, not the server. With CloudNativePG, use the
barman-cloud plugin: WAL archiving and base backups to the site's object
store (section 9).

## 5. Kubernetes

A complete, generic example. Apply it with `kubectl apply -n okf-mcp -f`, or
put it in your GitOps repository. It assumes:

- the CloudNativePG operator and the barman-cloud plugin are installed;
- ingress-nginx is the ingress controller, and cert-manager issues
  certificates through a `ClusterIssuer` named `letsencrypt`;
- a secret `okf-mcp-backup-creds` in the namespace holds the object store's
  `ACCESS_KEY_ID`, `SECRET_ACCESS_KEY` and `REGION` (create it however your site
  manages secrets).

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: okf-mcp-db
spec:
  instances: 3
  bootstrap:
    initdb:
      database: okf_mcp
      owner: okf_mcp
  storage:
    size: 10Gi
  plugins:
    - name: barman-cloud.cloudnative-pg.io
      isWALArchiver: true
      parameters:
        barmanObjectName: okf-mcp-backup
        serverName: okf-mcp-db
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: okf-mcp-config
data:
  config.toml: |
    listen = "[::]:8080"
    allowed_hosts = ["okf-mcp.example.com"]
    default_catalog = "notes"
    time_server = "time.example.com"
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: okf-mcp
spec:
  replicas: 1
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app: okf-mcp
  template:
    metadata:
      labels:
        app: okf-mcp
    spec:
      securityContext:
        runAsNonRoot: true
        runAsUser: 10001
        runAsGroup: 10001
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: okf-mcp
          image: ghcr.io/petergrace/okf-mcp:0.7.0
          args: ["serve"]
          ports:
            - name: http
              containerPort: 8080
          env:
            - name: OKF_MCP_CONFIG
              value: /etc/okf-mcp/config.toml
            - name: DATABASE_URL
              valueFrom:
                secretKeyRef:
                  name: okf-mcp-db-app
                  key: uri
            - name: PGSSLMODE
              value: verify-full
            - name: PGSSLROOTCERT
              value: /etc/okf-mcp/pg-ca/ca.crt
          startupProbe:
            tcpSocket:
              port: http
            periodSeconds: 5
            failureThreshold: 60
          readinessProbe:
            httpGet:
              path: /healthz
              port: http
            periodSeconds: 10
            timeoutSeconds: 5
          livenessProbe:
            tcpSocket:
              port: http
            periodSeconds: 20
          resources:
            requests:
              cpu: 100m
              memory: 256Mi
            limits:
              memory: 1Gi
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: ["ALL"]
          volumeMounts:
            - name: config
              mountPath: /etc/okf-mcp/config.toml
              subPath: config.toml
              readOnly: true
            - name: pg-ca
              mountPath: /etc/okf-mcp/pg-ca
              readOnly: true
            - name: tmp
              mountPath: /tmp
      volumes:
        - name: config
          configMap:
            name: okf-mcp-config
        - name: pg-ca
          secret:
            secretName: okf-mcp-db-ca
            items:
              - key: ca.crt
                path: ca.crt
        - name: tmp
          emptyDir: {}
---
apiVersion: v1
kind: Service
metadata:
  name: okf-mcp
spec:
  selector:
    app: okf-mcp
  ports:
    - name: http
      port: 8080
      targetPort: http
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: okf-mcp
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt
    nginx.ingress.kubernetes.io/proxy-buffering: "off"
    nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
    nginx.ingress.kubernetes.io/proxy-send-timeout: "3600"
    nginx.ingress.kubernetes.io/proxy-body-size: "80m"
spec:
  ingressClassName: nginx
  tls:
    - hosts: ["okf-mcp.example.com"]
      secretName: okf-mcp-tls
  rules:
    - host: okf-mcp.example.com
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: okf-mcp
                port:
                  name: http
```

The objects for backups (`ObjectStore`, `ScheduledBackup`) are in section 9.

**The Deployment, and why:**

- **One replica and `Recreate`:** the old pod stops before the new one starts,
  so two servers never publish at once (section 1). An upgrade has a short
  outage.
- **Startup probe:** TCP on 8080, for up to 5 minutes (60 x 5 s). The server
  opens its port only after it has run the migrations and loaded every
  catalog's snapshot.
- **Readiness probe:** `GET /healthz`, which answers 503 while the database
  does not answer within 2 seconds, so the pod leaves the Service during a
  database outage. The clock check never changes it.
- **Liveness probe:** TCP only, so a database failover never restarts the
  server.
- **`securityContext`:** non-root, a read-only root filesystem and every
  capability dropped. The server writes nothing to disk; the `emptyDir` on
  `/tmp` is where an archive copied into the pod for `okf-mcp import`, or
  written by `okf-mcp export`, goes (section 6).
- **No `RUST_LOG`:** the binary's default is the intended level (section 3).
- The `kubectl cp` in section 6 needs `tar` in the container, which the image
  has.

**The Service:** it sets no `ipFamilyPolicy`. On a dual-stack cluster the
server listens on both families (`listen = "[::]:8080"`), so any policy works,
and a cluster may enforce its own (one that makes every Service
`RequireDualStack` leaves a manifest asking for `PreferDualStack` permanently
out of sync in a GitOps tool). Leaving the field out lets the cluster decide.

**The Ingress annotations, and why:**

- `proxy-buffering: "off"`: `/mcp` answers with server-sent event streams.
  With buffering on, nginx holds events back until its buffer fills, and
  clients see nothing.
- `proxy-read-timeout` and `proxy-send-timeout` `"3600"`: a client's `GET /mcp`
  stream stays open for as long as the session lives. nginx's default of 60 s
  would cut it.
- `proxy-body-size: "80m"`: nginx's default of 1 MiB would refuse almost every
  import. The import route accepts up to `max_import_bytes` plus gzip slack
  (a little over 64 MiB with the default), so the ingress must allow more
  than that. Raise both together.

## 6. First run

The subcommands run inside the pod, where `DATABASE_URL`, the TLS variables
and `OKF_MCP_CONFIG` are already set.

**Create a catalog:**

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp catalog create notes --description "Team notes"
```

**Create keys.** Access is `read`, `write` or `admin`; grant named catalogs
with `--catalogs a,b` or every catalog, present and future, with `--all`:

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create alice-laptop --access write --catalogs notes --default notes --actor alice
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create ops-admin --access admin --all
```

`key create` prints the key once; only its SHA-256 is stored. Put it straight
into a password manager or a secret store. Never paste it into logs, tickets,
chat or shell history (a leading space keeps a command out of history in most
shells). `okf-mcp key list|revoke|grant|ungrant` manage keys afterwards; a
revoked key stops working within `poll_interval`.

**Import a catalog**, from a `git archive` tarball (exactly the tracked files)
or any `.tar.gz` that follows the import rules. Either copy it into the pod's
`/tmp`:

```sh
git -C ~/notes archive --format=tar.gz -o notes.tar.gz HEAD
kubectl -n okf-mcp cp notes.tar.gz "$(kubectl -n okf-mcp get pod -l app=okf-mcp -o name | cut -d/ -f2)":/tmp/notes.tar.gz
kubectl -n okf-mcp exec deploy/okf-mcp -- okf-mcp import notes /tmp/notes.tar.gz
kubectl -n okf-mcp exec deploy/okf-mcp -- rm /tmp/notes.tar.gz
```

or stream it on stdin (`-`):

```sh
git -C ~/notes archive --format=tar.gz HEAD |
    kubectl -n okf-mcp exec -i deploy/okf-mcp -- okf-mcp import notes -
```

or post it to the HTTP route with an `admin` key (`?replace=true` to replace
a non-empty catalog):

```sh
git -C ~/notes archive --format=tar.gz HEAD |
    curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" --data-binary @- \
        https://okf-mcp.example.com/catalogs/notes/import
```

The server picks up a catalog created or imported from the command line
within `poll_interval` (`/healthz`'s `catalogs` counts it once it is loaded);
an import through the HTTP route is visible at once.

`okf-mcp export` inside the pod must write under `/tmp` (or to `-`, stdout),
the only writable path:

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- okf-mcp export notes - > notes.tar.gz
```

**Connect a harness.** The MCP URL is `https://okf-mcp.example.com/mcp`, with
the key in `Authorization: Bearer <key>`. For Claude Code:

```sh
claude mcp add --transport http okf https://okf-mcp.example.com/mcp \
    --header "Authorization: Bearer $OKF_MCP_KEY"
```

`X-OKF-Catalogs` (or `?catalogs=a,b`) picks the connection's catalogs, and
`X-OKF-Changeset` (or `?changeset=label`) names its changeset. The README's
"Connecting a client" and "Changesets" sections describe both.

**Protocol versions:** the server accepts MCP `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25` and `2026-07-28` (rmcp 3.8's known versions). A
client on a version before `2026-07-28` gets a stateful session
(`Mcp-Session-Id`); one on `2026-07-28` is served statelessly.

## 7. Health and time

`GET /healthz` needs no key:

```json
{"database": "ok", "catalogs": 2, "clock": {"ok": true, "checked_at": "2026-11-03T12:00:00Z", "offset_ms": 3, "stratum": 2}}
```

- **Status:** 200 when the database answers `SELECT 1` within 2 seconds;
  otherwise 503 with `"database": "unreachable"`. Nothing else changes it.
- **`catalogs`:** how many catalogs are loaded (a count, never names).
- **`clock`:** the last clock check, or `null` without a `time_server` (or
  before the first check). It is coarse on purpose, since the route is
  public:
  - `ok`: whether the time server gave a usable reply;
  - `checked_at`: when the check ran, by the server's clock;
  - `offset_ms`: the time server's clock minus ours, in milliseconds (positive
    means ours is behind); absent when `ok` is false;
  - `stratum`: the time server's stratum; absent when `ok` is false.

  The server's name and any error text appear only in the logs.

**The clock check** queries `time_server` over SNTP at startup and every 15
minutes, with a 5-second limit that includes the DNS lookup. Every timestamp
the server writes (`generated`, `reviewed`, log dates, revisions) comes from
its own clock, so the check is there to catch a drifting node:

- an offset over 2 seconds is logged at `error`: "the clock is off by more
  than 2 seconds", with the `server` and `offset_ms` fields. Fix the node's
  time synchronisation; the server doesn't adjust its clock;
- a failed query (no answer, a refused reply, a name that doesn't resolve) is
  logged at `warn`: "the clock check failed", with the error, which names the
  server;
- a good check is logged only at `debug`.

**A failing clock check never fails readiness**: a time-server outage must not
take the server out of service. Alert on the `error` log line, not on
`/healthz`'s status.

## 8. Upgrades

1. Release a new version: bump `version` in `Cargo.toml`, merge, and push a
   `v<version>` tag. The image workflow publishes `:<version>`.
2. Change the image tag in the Deployment (in your GitOps repository, if you
   use one).
3. The Deployment recreates the pod: the old one stops, the new one runs any
   new migrations, loads the catalogs and becomes ready.

- **Migrations are forward-only.** Once a new version has migrated the
  database, an older binary refuses to start against it. To roll back past a
  migration, restore the database from a backup taken before the upgrade
  (section 9), or fix forward with a new version.
- **Release notes** are the `changelog/` directory: one dated entry per change,
  and one per milestone.
- **Changesets survive** an upgrade: they are in the database. Open MCP
  sessions do not; clients reconnect.

## 9. Backup and restore

**What to back up:** the database, and only the database. The pod holds no
state.

### CloudNativePG with the barman-cloud plugin

The `Cluster` in section 5 already names the plugin as its WAL archiver
(`plugins`), so every WAL segment goes to the object store. Add the object
store and a daily base backup:

```yaml
apiVersion: barmancloud.cnpg.io/v1
kind: ObjectStore
metadata:
  name: okf-mcp-backup
spec:
  retentionPolicy: "30d"
  configuration:
    destinationPath: s3://okf-mcp-backups/
    endpointURL: https://s3.example.com
    s3Credentials:
      accessKeyId:
        name: okf-mcp-backup-creds
        key: ACCESS_KEY_ID
      secretAccessKey:
        name: okf-mcp-backup-creds
        key: SECRET_ACCESS_KEY
      region:
        name: okf-mcp-backup-creds
        key: REGION
    wal:
      compression: gzip
    data:
      compression: gzip
---
apiVersion: postgresql.cnpg.io/v1
kind: ScheduledBackup
metadata:
  name: okf-mcp-db-daily
spec:
  schedule: "0 30 3 * * *"
  backupOwnerReference: self
  cluster:
    name: okf-mcp-db
  method: plugin
  pluginConfiguration:
    name: barman-cloud.cloudnative-pg.io
```

The schedule has six fields, seconds first: this one runs at 03:30 every day.

**Set the region.** barman-cloud signs its requests for a region, the AWS default
when none is given. An S3-compatible store with a region of its own refuses
requests signed for any other one, often with a bare `400 Bad Request` on the
bucket check, and the cluster's `ContinuousArchiving` condition reports
`ContinuousArchivingFailing`. `s3Credentials.region` takes the region from a
secret key, as above; the value itself is not secret.

Give the backup credentials access to this bucket only.

**Check it works.** The cluster's `ContinuousArchiving` condition should be
`True`. Then take an on-demand backup with the
[cnpg kubectl plugin](https://cloudnative-pg.io/documentation/current/kubectl-plugin/)
and wait for it to reach `completed`:

```sh
kubectl cnpg backup okf-mcp-db -n okf-mcp \
    --method plugin --plugin-name barman-cloud.cloudnative-pg.io
kubectl -n okf-mcp get backups
```

kubectl refuses flags placed before a plugin's name (`kubectl -n okf-mcp cnpg
...` fails with "flags cannot be placed before plugin name"), so they go after
`cnpg backup`.

Don't trust the schedule alone: check that a recent backup completed.

### Restore

Restore into a **new** `Cluster`, bootstrapped from the object store. It
replays the newest base backup and then the archived WAL to its end:

```yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: okf-mcp-db-restore
spec:
  instances: 1
  storage:
    size: 10Gi
  bootstrap:
    recovery:
      source: origin
      database: okf_mcp
      owner: okf_mcp
  externalClusters:
    - name: origin
      plugin:
        name: barman-cloud.cloudnative-pg.io
        parameters:
          barmanObjectName: okf-mcp-backup
          serverName: okf-mcp-db
```

- **`database` and `owner` name the source's database and owner.** Without
  them CloudNativePG names both `app`, and the restored cluster's `-app`
  secret points at a database and user that hold none of the restored data.
- **No `plugins` section:** the restored cluster must not archive into the
  source's path (`serverName: okf-mcp-db`), or it would write over the
  backups it came from. If it is to become the live database, give it its
  own `plugins` entry with a new `serverName` once it is up.
- **The image:** if your cluster needs `imageName` on every `Cluster` (an
  image mirror, say), give the restored one the same `imageName` as the source.
- **Before the backup,** hold every write: publishes, imports (the subcommand
  and the route) and `catalog` changes. The simplest way is to scale the
  Deployment to 0 for the window
  (`kubectl -n okf-mcp scale deploy/okf-mcp --replicas=0`, then back to 1).
  Then record the live database's numbers with the query below. These are the
  baseline.
- **Compare it** with the live database when it is ready, querying both at the
  same moment with writes still held, so the two must be equal. Counts catch
  missing rows, and one checksum over every document (catalog, path, revision,
  execute bit and content) catches changed documents. It does not cover the
  revision history's content or changesets; the counts and `last_revision`
  stand in for those:

  ```sh
  kubectl -n okf-mcp exec -i <instance> -c postgres -- psql -d okf_mcp <<'SQL'
  SELECT (SELECT count(*) FROM catalogs) AS catalogs,
         (SELECT count(*) FROM documents) AS documents,
         (SELECT count(*) FROM revisions) AS revisions,
         (SELECT max(created_at) FROM revisions) AS last_revision,
         (SELECT encode(sha256(convert_to(string_agg(
                    catalog_id || '/' || path || '@' || revision || ':' || executable
                      || ':' || encode(sha256(content), 'hex'),
                    ',' ORDER BY catalog_id, path), 'UTF8')), 'hex')
            FROM documents) AS content_checksum;
  SQL
  ```

  Run it on each cluster's primary. `catalog_id` is kept by a physical
  restore, so the checksum compares like with like, even for catalogs that share
  a path. `convert_to` keeps backslashes in paths from being read as `bytea`
  escapes.
- **If the live database moved anyway,** the checksum can no longer match.
  Require each count to be at least the baseline and at most the live value,
  and compare one document you know didn't change by its own
  `encode(sha256(content), 'hex')`. The bound assumes rows only grow: a
  catalog deleted in the window (which cascades through `documents` and
  `revisions`) breaks it even on a good restore, so a window that may include a
  deletion needs writes held, not bounds.
- **Then** either point `DATABASE_URL` and `PGSSLROOTCERT` at its `-app` and
  `-ca` secrets, or delete it (`kubectl -n okf-mcp delete cluster
  okf-mcp-db-restore`; CloudNativePG removes its volumes).
- **Tested:** the restore `Cluster` above was followed as written in milestone
  7's restore drill. A small catalog was ready about 3 minutes after the
  `Cluster` was created, and matched the live database exactly. The comparison
  query was revised after the drill (the drill's version ordered by `path`
  only), and the revised query was then run as written on the live database.

### An extra copy of each catalog

The export route gives a catalog's published files as a `.tar.gz` (a
`read` key is enough), and `okf-mcp import` or the import route restores one.
This keeps the files, not the revision history or open changesets, so it
complements the database backups rather than replacing them:

```sh
curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" -o notes.tar.gz \
    https://okf-mcp.example.com/catalogs/notes/export
```

## 10. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| 401 on `/mcp` or an archive route | No `Authorization: Bearer` header, or the key is unknown or revoked. The body is plain text, with `WWW-Authenticate: Bearer` |
| 403 on every `/mcp` request through the ingress | The `Host` is not in `allowed_hosts` (rmcp's DNS-rebinding check). Add the public host name and restart the pod |
| 404 on `/mcp` with "Session not found" | The `Mcp-Session-Id` was opened by another key, idled out past `session_idle_timeout`, or predates a restart. Reconnect with the key that opened it |
| A tool returns `CatalogNotSelected` or `Forbidden` | The key is not granted that catalog, or its access level is too low (a `read` key calling a write tool). `okf-mcp key grant`, or a key with more access |
| 404 on an archive route | The catalog doesn't exist or the key isn't granted it (never 403, so a key can't probe which catalogs exist) |
| 403 on an archive route | The key reaches the catalog but its level is too low: import needs `admin` |
| 413 on import | The body is over `max_import_bytes` plus slack (a plain-text axum rejection), or, if the answer comes from nginx, over the ingress's `proxy-body-size`. Raise both together |
| 409 on import | The catalog is not empty: add `?replace=true` (or `--replace` on the command line) |
| `/healthz` is 503 and the pod is not ready | The database did not answer within 2 seconds: check the `Cluster`'s state, the `okf-mcp-db-app` secret and network policies |
| The pod never becomes ready on an IPv6 cluster | `listen` is `0.0.0.0:8080`; set `"[::]:8080"` |
| The server exits with a configuration error | The file failed validation; the message names the file and the key |
| The server exits naming a migration it does not know | The database was migrated by a newer version: deploy that version or newer |
| TLS errors at startup (`UnknownIssuer`, a name mismatch) | `PGSSLROOTCERT` names the wrong CA, or the URL's host is not one the certificate names (with CloudNativePG, use the `-rw` service from the `-app` secret's `uri`). Check that the URL names no `sslmode` or `sslrootcert` |
| `clock.ok` is false | Read the pod's `warn` line, "the clock check failed": often the name doesn't resolve from the pod, or UDP port 123 is blocked. `time_server` can be an address |
| Database tests fail locally with TLS errors | `PGSSLMODE=verify-full` is exported in your shell; unset it |
