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

- **Name:** `ghcr.io/petergrace/okf-mcp:<version>`, for example `:0.11.1`.
  Every build is also tagged `:sha-<short commit>`. There is no `:latest`:
  manifests pin a version.
- **Built by** `.github/workflows/image.yml` when a `v*` tag is pushed (`v0.11.1`
  gives `:0.11.1`), and on a manual dispatch (which publishes `:sha-<short>`
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
| `maintenance_review_intervals` | `{}` (30 days per catalog) | Per-catalog review durations, e.g. `{ pg = "30d" }`; each must be at least one day |
| `max_import_bytes` | `"64MiB"` | The largest uncompressed import (`MiB` is binary, `MB` decimal, or a number of bytes). The import route caps the request body at this plus 0.1% and 64 KiB of gzip slack |
| `time_server` | none | The NTP server for the clock check (`host`, `host:port` or an address). Without it there is no check |
| `concept_outline_chars` | `20000` | `get_concept` returns an outline of the headings, not the body, for a body longer than this many characters, unless the call passes `full` or `section` |
| `database_max_connections` | `16` | The most database connections one process opens (`serve` and every subcommand). Writers queued on one changeset wait without one; the writer whose turn it is, a publish, and a read that builds an overlay each hold one |
| `overlay_cache_size` | `64` | The most changeset overlays (an open changeset's view of its catalog) kept in memory; past it, the least recently used is dropped and rebuilt on its next read |

The server refuses to start (and every subcommand refuses to run) with a zero
`poll_interval`, `session_idle_timeout`, `database_max_connections` or
`overlay_cache_size`, an empty `allowed_hosts` or a blank
entry in it, or a `time_server` that is blank or has surrounding whitespace.
`maintenance_review_intervals` also rejects invalid catalog names, malformed
durations, durations below 86,400 seconds and durations beyond the supported
range. Omitted catalogs use 30 days.

```toml
listen = "[::]:8080"
allowed_hosts = ["okf-mcp.example.com"]
default_catalog = "notes"
time_server = "time.example.com"
session_idle_timeout = "2h"
poll_interval = "10s"
max_import_bytes = "64MiB"
maintenance_review_intervals = { pg = "30d" }
concept_outline_chars = 20000
database_max_connections = 16
overlay_cache_size = 64
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
  to `database_max_connections` (16 by default) connections; keep the
  database's `max_connections` above that times the number of replicas.
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
          image: ghcr.io/petergrace/okf-mcp:0.11.1
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

**Change a catalog's description** (what `list_catalogs` tells agents, for
example when a catalog becomes the source of truth). It prints the old and
new text; an unknown catalog is refused, and an empty description is allowed:

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp catalog update notes --description "Team notes; the source of truth"
```

**Create keys.** Access is `read`, `write` or `admin`; grant named catalogs
with `--catalogs a,b` or every catalog, present and future, with `--all`.
Reporting is independently opt-in via `key create --maintenance-report`: it
requires read access to the catalog and never grants write access; write/admin
do not enable reporting by themselves:

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create claude --access write --catalogs notes --default notes --actor claude-code-tui
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create claude --access write --catalogs notes --default notes --actor claude-ai-web
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create ops-admin --access admin --all
```

The positional value is a grouping name, so the two `claude` credentials are
listed together while retaining separate actors, grants, defaults and
revocation. `key create` prints the key once and its immutable administration
id on stderr; only the key's SHA-256 is stored. Put it straight
into a password manager or a secret store. Never paste it into logs, tickets,
chat or shell history (a leading space keeps a command out of history in most
shells). `key list` shows name, actor, id, access, state, grants, default and
`maintenance_report true|false`, grouped by name. Use
`okf-mcp key maintenance-report ID --enabled true|false [--actor ACTOR]` to
change the reporting flag. `revoke`, `grant`, `ungrant` and `maintenance-report`
accept an id, an unambiguous live name, or `NAME --actor ACTOR`; for example,
`okf-mcp key grant claude work --actor
claude-ai-web`. A revoked name/actor pair can be created again for rotation;
the old row and its changeset history remain attached to the old id. A revoked
key stops working within `poll_interval`.

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

The server picks up a catalog created, updated or imported from the command
line within `poll_interval` (`/healthz`'s `catalogs` counts it once it is
loaded; a description change does not rebuild the catalog's snapshot); an
import through the HTTP route is visible at once.

`okf-mcp export` inside the pod must write under `/tmp` (or to `-`, stdout),
the only writable path:

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- okf-mcp export notes - > notes.tar.gz
```

It reports the generation the archive holds on stderr (`exported catalog
"notes" at generation 42`). The import report (JSON on stdout, or the HTTP
route's body) lists the `added`, `updated` and `deleted` paths and counts the
`unchanged` ones.

**Update machinery** (`AGENTS.md`, `tools/`, `install/` and the rest that the
write tools refuse) with `--replace --only-machinery`. Only the source's
machinery is compared and written; its other files are ignored and the
catalog's concepts stay as they are, so a concept published meanwhile is
kept. The source can be the catalog's template checkout or an export with
new machinery overlaid:

```sh
git -C ~/notes-template archive --format=tar.gz HEAD |
    kubectl -n okf-mcp exec -i deploy/okf-mcp -- \
        okf-mcp import notes - --replace --only-machinery
```

A full `--replace` from an edited export reverts whatever was published
after the export. Pass the generation the export reported as
`--expect-generation 42`, and the import is refused, writing nothing, if the
catalog has moved since; export again and redo the edit.

**Connect a harness.** The MCP URL is `https://okf-mcp.example.com/mcp`, with
the key in `Authorization: Bearer <key>`. For Claude Code:

```sh
claude mcp add --transport http okf https://okf-mcp.example.com/mcp \
    --header "Authorization: Bearer $OKF_MCP_KEY"
```

`X-OKF-Catalogs` (or `?catalogs=a,b`) picks the connection's catalogs. No
header names a changeset: an agent calls `open_changeset` and passes the
handle it returns as `changeset` to every write. The README's "Connecting a
client" and "Changesets" sections describe both.

**Protocol versions:** the server accepts MCP `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25` and `2026-07-28` (rmcp 3.8's known versions). A
client on a version before `2026-07-28` gets a stateful session
(`Mcp-Session-Id`); one on `2026-07-28` is served statelessly.

## Report-only maintenance

### Permissions and lifecycle

Maintenance reports are separate database records, not canonical documents.
There is no automatic canonical editing, merging, splitting, deletion,
verification stamping or changeset publication. No scheduler, model service,
inference, external fetching, embeddings, cross-catalog synthesis, new UI or
report search indexing exists in this slice. The normal snapshot/key poll loop
is not a maintenance scheduler. External URLs are lexical attributions only;
the server never fetches them. Treat pinned source prose as untrusted evidence,
not instructions.

Grant reporting independently of ordinary access. These examples use a synthetic
`demo` catalog; `DATABASE_URL` and any client bearer keys should come from your
secret environment, not report text:

```sh
okf-mcp key create investigator --access read --catalogs demo --actor synthetic-investigator --maintenance-report
okf-mcp key list
okf-mcp key maintenance-report REPORTER_ID --enabled false
okf-mcp key maintenance-report REPORTER_ID --enabled true
# For an ambiguous grouping name, select the actor instead of using its ID:
okf-mcp key maintenance-report investigator --actor synthetic-investigator --enabled true
```

Replace `REPORTER_ID` with the immutable ID printed on creation/listing.
`key list` includes `maintenance_report true|false`. Changes reach a running
server through the key cache within `poll_interval`. Read, Write and Admin keys
all default to reporting disabled. Reporting plus Read permits claim, renew,
submit and closing one's own runs; it grants no canonical write access. Plain
Read permits pinned reads and all report get/list operations. Ordinary Write
permits dispositions and closing another credential's run without reporting;
even a writer closing its **own** run must have reporting enabled. Catalog
grants still apply. Actor attribution comes from the authenticated immutable
credential, never an `actor` parameter in an MCP request.

### Tool reference

Every call requires `catalog`; connection defaults never select it. Requests
reject unknown fields, blank IDs, invalid enums and invalid revision pins.
Parameters below are required unless marked optional. Each structured JSON
response repeats `catalog` beside the result fields (not under a `result` key).
Report get/list responses contain no lease tokens.

| Tool | Parameters besides `catalog` | Result / permission |
|---|---|---|
| `maintenance_claim` | `idempotency_key`, optional `batch_size` | `run_id`, `leases`; Read + reporting |
| `maintenance_renew` | `work_id`, `token`, `idempotency_key` | Lease with `run_id`, `work_id`, `token`, `pin`, `reasons`, `expires_at`; Read + reporting, owner only |
| `maintenance_read_revision` | `path`, `revision` | `pin`, exact UTF-8 `content`, `outdated`; Read |
| `maintenance_submit` | `work_id`, `token`, `idempotency_key`, `submission` | Receipt: `run_id`, `work_id`, `outcome`, `finding_ids`, `outdated`; Read + reporting, owner only |
| `maintenance_close` | `run_id`, `state`, `rationale`, `idempotency_key` | Run summary; own run requires reporting, another credential's requires Write |
| `maintenance_list_runs` | optional `after`, `limit` | `items`, `next_after`; Read |
| `maintenance_get_run` | `run_id` | `run` summary and `work` array with original evidence/finding IDs; Read |
| `maintenance_list_findings` | optional `after`, `limit` | `items`, `next_after`; Read |
| `maintenance_get_finding` | `finding_id` | Finding input, actor, time, disposition/history and read-time `outdated`; Read |
| `maintenance_dispose` | `finding_id`, `disposition`, `rationale`, `idempotency_key` | Finding with appended disposition history; Write, reporting alone insufficient |

`maintenance_close.state` accepts `completed`, `failed` or `cancelled`, not
`running`. Explicit completion requires every item terminal; the last terminal
item also completes a running run automatically. Closing failed/cancelled
invalidates outstanding leases and releases work for a later run. Empty claims
complete immediately. History and completed work survive closure/recovery.
An authorized `state: failed` close against an already terminal run acknowledges
its actual `completed`, `cancelled` or `failed` state idempotently; it does not
rewrite the state, terminal time/reason, or work history. This also covers another
caller terminalizing the run between lookup and close. Ownership and reporting/Write
checks still apply before replay. The acknowledgment is not a submission receipt.

### Limits, evidence and retries

| Boundary | Implemented limit |
|---|---|
| Claim batch / primary concepts per run | Default 10; 1–50 inclusive; failed attempts count too; a new batch is a new run |
| Lease | 900 seconds; renew to `min(now + 900 seconds, run deadline)` |
| Run deadline | Creation + 3,600 seconds; exhaustion fails the run as `budget_exhausted` |
| Expiry | Expired at `now >= expires_at`; recovery records `lease_expired`, eligible immediately with a new attempt/token |
| Explicit failed/inconclusive retry | 86,400-second cooldown |
| Aging review interval | Default 30 days; `maintenance_review_intervals = { pg = "30d" }`; minimum one day |
| Findings | At most 20 per primary work submission |
| Evidence entries | At most 20 submission-level local entries; at most 20 combined local/external entries per finding |
| Excerpt | Nonempty; at most 2,048 UTF-8 bytes (2 KiB), local or external |
| Submission | At most 262,144 bytes (256 KiB) of serialized `Submission` JSON, not the surrounding MCP request |
| Rationale | At most 4,096 UTF-8 bytes for close/disposition |
| `reported_tokens` | Optional nonnegative integer, at most 9,223,372,036,854,775,807 (storage limit) |
| Idempotency key | 1–128 ASCII characters; no whitespace or control characters; secret-scanned |
| Revision pin | Positive revision, concept `.md` path of at most 256 bytes; no traversal or machinery paths; MCP uses `/...md` |
| Report page | Default 20; 1–200 inclusive |

All timestamps come from the injected server clock; none is a caller parameter.
All mutations require an idempotency key, scoped by catalog, immutable credential
and operation. Retry the same normalized request with the same key to get the
original response without renewing a lease or changing counts/times. Reusing a
key with different content is `Refused`. Authorization runs before replay;
revoked/ungranted/disabled reporters cannot replay successful mutations. A
successful submit replay remains valid after lease expiry. Never share lease
tokens; a same-name replacement credential is not the original owner.

Local excerpts must be exact substrings of persisted UTF-8 revision bytes,
including line endings, not normalized snapshot text. Completed submissions
must cite the exact primary pin in `submission.evidence`; every finding needs
local evidence even with external observations. `suggested_link`,
`possible_duplicate` and `conflicting_assertions` require two distinct concept
paths. Other categories are `potentially_outdated_information` and
`unverifiable_claim`. `relationship` is `direct_statement`, `inference` or
`external_observation`. External observations require literal `http://` or
`https://` resources without credentials, whitespace or percent-encoded
authorities, a nonempty excerpt and nonblank attribution. Reports cannot serve
as evidence inputs.

Caller-supplied `submission.outcome` is `completed`, `inconclusive` or `failed`.
`superseded` is a server-assigned outcome, rejected as submission input.
Completed work may assess `supported_current`, `outdated` or `not_assessed`;
aging work must use one of the first two. Inconclusive work may assess
`unverifiable` or `not_assessed`; failed submissions use `not_assessed`.
Failed/inconclusive work need not supply evidence. Reported support is an
attributed investigator assessment, never human/server verification; absence of
evidence never means current. Completed aging work schedules another review
after the interval whether current or outdated. A newer revision is uncovered
and invalidates effective support; source edits/deletion can supersede work and
make historical reports `outdated`. Legacy `verified` stamps only hint at
scheduling (malformed/future stamps are ignored). Maintenance never writes
canonical `verified`, `reviewed` or `generated` stamps.

The existing secret scanner rejects detected patterns in report text and
idempotency keys with masked `SecretDetected` errors. It is **heuristic**, not a
proof that text contains no secret. Keep secrets out of summaries, excerpts,
URLs, rationales and exported reports even if validation accepts them.

### Investigate and record follow-up

These are synthetic MCP `tools/call` parameter objects, not new HTTP endpoints.
Use your client's normal MCP connection with a reporter key from its secret
environment. Suppose the synthetic catalog has `/service.md` revision 1 whose
exact content includes `The service port is 8080.`. Do not assume a real catalog
returns this pin or these IDs: substitute the actual lease and pinned bytes.

1. Claim one item with the reporting credential:

```json
{"name":"maintenance_claim","arguments":{"catalog":"demo","batch_size":1,"idempotency_key":"demo-claim-1"}}
```

Save returned `run_id` and `leases[0]` (if `leases` is empty, there is no work).
Replace `WORK_ID`, `LEASE_TOKEN`, `RUN_ID`, path and revision below with those
returned values; do not invent them. Each lease also supplies `reasons` and
`expires_at`.

2. Read the lease's exact revision, not the latest canonical document:

```json
{"name":"maintenance_read_revision","arguments":{"catalog":"demo","path":"/service.md","revision":1}}
```

3. Submit an inconclusive investigation with an attributed finding. This example
cannot establish whether the recorded port is current and does not claim it can:

```json
{
  "name": "maintenance_submit",
  "arguments": {
    "catalog": "demo",
    "work_id": "WORK_ID",
    "token": "LEASE_TOKEN",
    "idempotency_key": "demo-submit-1",
    "submission": {
      "outcome": "inconclusive",
      "assessment": "unverifiable",
      "summary": "The recorded port needs independent confirmation.",
      "evidence": [{"pin":{"path":"/service.md","revision":1},"excerpt":"The service port is 8080."}],
      "findings": [{
        "category": "unverifiable_claim",
        "assertion": "The document states a service port without confirmation of current runtime state.",
        "follow_up": "Ask the service owner to confirm the current port.",
        "uncertainty": "No runtime observation was available in this investigation.",
        "relationship": "direct_statement",
        "evidence": [{"pin":{"path":"/service.md","revision":1},"excerpt":"The service port is 8080."}],
        "external": []
      }],
      "reported_tokens": null
    }
  }
}
```

`reported_tokens` is optional investigator accounting, not measured billing or
proof a model ran. Save the returned receipt's `finding_ids[0]` as `FINDING_ID`.
If work takes longer, renew before expiry using
`{"catalog":"demo","work_id":"WORK_ID","token":"LEASE_TOKEN","idempotency_key":"demo-renew-1"}`
as `maintenance_renew` arguments. To stop unfinished work, `maintenance_close`
arguments are
`{"catalog":"demo","run_id":"RUN_ID","state":"cancelled","rationale":"Stop synthetic investigation.","idempotency_key":"demo-close-1"}`.

4. Inspect and export report JSON with any catalog reader:

```json
{"name":"maintenance_get_run","arguments":{"catalog":"demo","run_id":"RUN_ID"}}
{"name":"maintenance_list_runs","arguments":{"catalog":"demo","limit":20}}
{"name":"maintenance_list_findings","arguments":{"catalog":"demo","limit":20}}
{"name":"maintenance_get_finding","arguments":{"catalog":"demo","finding_id":"FINDING_ID"}}
```

The block contains four separate calls, not one JSON document. List results are
`{"catalog":"demo","items":[...],"next_after":"LAST_ROW_ID"}` when more
rows exist. Continue the same list tool with
`{"catalog":"demo","limit":20,"after":"LAST_ROW_ID"}`; stop when
`next_after` is `null`. Pages ascend by `(created_at, id)`. Cursors are row IDs,
not offsets/timestamps, and belong to that catalog and list type. Foreign or
missing cursors/records return `NotFound`. To export reports, save these JSON
results, paginate both lists, and retrieve run/finding detail by ID. There is no
separate report-export tool or report bundle endpoint.

5. Switch to an ordinary writer credential and record a decision:

```json
{"name":"maintenance_dispose","arguments":{"catalog":"demo","finding_id":"FINDING_ID","disposition":"accepted_for_follow_up","rationale":"Owner confirmation is needed before any canonical edit.","idempotency_key":"demo-dispose-1"}}
```

Other dispositions are `dismissed`, `needs_input` and `open` (reopen). History is
append-only. Acceptance **does not apply changes**. Any later canonical edit or
publication remains a separate, explicitly authorized changeset workflow.

### Export, retention and backup boundaries

Reports do not enter normal search, lint/oldest/links reports, canonical history
or canonical bundle exports (`okf-mcp export` and `GET /catalogs/{name}/export`).
A canonical bundle is not a maintenance backup; importing one does not restore
report history. Database backups include maintenance tables, leases, receipts,
findings and dispositions along with canonical data. Treat database backups as
sensitive: public report get/list output omits tokens, database backups do not.
Deleting a concept preserves historical reports but makes affected pins stale.
Deleting a catalog removes its maintenance records under existing cascade
semantics; recreating the same name is a new identity and cannot replay old
receipts. No automatic report pruning is introduced here.

## Report-only runner

`okf-maintenance` is a separate binary, not a server poller or resident scheduler.
`cargo run` preserves the legacy `okf-mcp` default. Use `cargo run --bin
okf-maintenance -- ...` for development, or the explicitly provisioned runner
executable for operations. It needs no database credentials or catalog checkout.
The controller can use only `search`, `get_concept`, `history`,
`maintenance_claim`, `maintenance_renew`, `maintenance_read_revision`,
`maintenance_submit`, `maintenance_close` and `maintenance_get_run`. It cannot
publish, change canonical content, dispose findings or fetch source URLs.

### Prerequisites and commands

1. Provision a dedicated ordinary **Read + maintenance_report** key restricted to
   one catalog, and a separate provider API key/account/quota. These are operator
   actions, not effects of building this repository. An interactive Claude
   subscription/OAuth login does **not** supply this adapter's authentication:
   `--bare` uses `ANTHROPIC_API_KEY`, never interactive OAuth/keychain state.
2. Choose an exact provider model ID and install a trusted, pinned Claude
   executable outside user homes (the service uses `ProtectHome=true`). The
   inspected real CLI was **`2.1.319 (Claude Code)`**, version/help only; no paid
   output has been verified. Synthetic envelopes target that contract but do not
   prove it works against a provider. Do not silently update the executable or
   expected version when a check fails.
3. Adapt [config.toml](../examples/maintenance/config.toml): replace the `.invalid`
   HTTPS endpoint, catalog/model placeholders and absolute paths. Certificates
   must verify; redirects and inherited proxies are disabled. Set **both**
   `allow_model_upload` and `acknowledge_inflight_budget_overshoot` true only with
   authorization. Both default false, including in the example. Network preflight
   also requires both flags under the existing client contract, though it does
   not upload source documents or run inference. `require_hard_billing_cap=true`
   is rejected as unsupported. Unknown config fields and invalid values fail
   closed without echoing them.
4. Supply `OKF_MAINTENANCE_TOKEN` and `ANTHROPIC_API_KEY` via the environment or a
   protected service `EnvironmentFile`; never flags or TOML. Avoid shell history,
   `set -x`, environment dumps and world-readable files. The controller uses one
   loaded credential object for the connection, journal binding and provider
   adapter. Only the provider key reaches the model child, not the MCP token,
   database credentials, inherited user configuration or proxy overrides.

With prerequisites and explicit operator authorization (these commands are not
run by installation scripts):

```sh
/opt/okf-maintenance/okf-maintenance preflight --config /etc/okf-maintenance/config.toml
/opt/okf-maintenance/okf-maintenance run-once --config /etc/okf-maintenance/config.toml
```

`preflight` reads configuration, credential presence, exact executable version,
required help flags and MCP tool schemas. It creates no journal/lock, claims no
work, performs no report mutation and runs no inference. It does not certify the
key's reporting grant or the paid result envelope. Success is exit 0 with exactly
a zero-count `RunSummary`: `run_id:null`, `reported:0`, `inconclusive:0`, `failed:0`,
`skipped:0`, `invocations:0`, `accounting_known:true`, `reason:"preflight_passed"`.
Here known accounting means no model invocation occurred, not provider validation.

`run-once` locks private state before connecting and either recovers the saved
run or performs one bounded batch. A later invocation may start a fresh budget
window only after the prior run is durably terminal. SIGINT/Ctrl-C and SIGTERM
request cancellation and **await supervisor completion**, including owned inference
cleanup and any permitted report/close acknowledgements. The CLI does not abort
its supervisor. Forced kill/OOM can still interrupt durability and require recovery.

Normal stdout is one JSON summary (or evaluation object), never logs. Run fields
are `run_id`, `reported`, `inconclusive`, `failed`, `skipped`, `invocations`,
`accounting_known`, `reason`. `reported` includes completed and superseded receipts;
`skipped` counts work without receipts. Fixed reasons describe operational state,
not source content. Errors use fixed redacted stderr messages, no raw clap values,
TOML diagnostics, remote bodies, tokens or child stderr.

| Exit | Meaning |
|---|---|
| 0 | Preflight passed; offline evaluation completed; or all selected work acknowledged without runtime failure (including empty batches and inconclusive aging). Evaluation 0 is not a quality threshold. |
| 2 | Invalid CLI/configuration or incompatible readiness contract/version/schema. |
| 3 | Another process holds this local state lock. No new claim or model invocation. |
| 4 | Runtime/permission/transport/I/O/journal/cancellation failure, partial execution or unknown accounting. Runtime protocol failure summaries also exit 4, not 2. |

**Exit 0 never verifies present-day facts.** Catalog consistency is not independent
real-world evidence. Aging always maps to `Inconclusive` / `Unverifiable`; changed-only
work can be `NotAssessed`. The runner never submits `SupportedCurrent`, renews a
canonical verification stamp or counts copied assertions as independent validation.

### Privacy, limits and accounting

Selected primary/neighbor catalog text is uploaded to the configured provider.
Review data classification and provider retention before consent. The secret
scanner is heuristic: passing it is not proof that a document is safe to upload.
Prompt injection remains untrusted data, not permission for shell/browser/MCP tools.
The child gets a private temporary working directory/HOME, an allowlisted
environment and tool-restricting CLI flags. These are **not an OS sandbox or
network egress allowlist**. Systemd filesystem hardening does not change that.

| Control | Default / hard local bound |
|---|---|
| Batch / invocations | Batch 10 (1..50); invocations default `min(10,batch_size)`, 1..batch_size and at most 50 |
| Run duration | Standalone 1,800 seconds, accepted 1..3,000; service example 1,740 |
| Child duration / concurrency | 90 seconds, exactly one model process at a time |
| Primary / neighbor / serialized prompt | 65,536 / 16,384 / 131,072 bytes; at most 4 neighbors from 20 discovered paths |
| Child stdout / stderr | 1,048,576 / 65,536 bytes, bounded and discarded after processing |
| MCP | 10-second RPC timeout; at most 3 transient attempts, delays 1 then 2 seconds; exact mutation payload/key replay |
| Heartbeat / safety margin | Every 60 seconds (earlier sweeps when needed); 30 seconds before lease/server deadline |
| Reservation | `invocation_microusd=100000` (USD 0.10); `run_microusd=1000000` (USD 1.00) |
| Observed token stop | 100,000; input, output, cache creation and cache read all included |
| Model retries | 0; no model-based repair or fallback |

The CLI uses one monotonic `max_run_seconds` window starting before configuration
and connection setup. Connection, paginated tool discovery, readiness probes and
supervision share that deadline; setup never grants supervision a fresh full
window. Preflight uses the same bounded setup window without claims or inference.
SIGINT/SIGTERM cancel network-only setup promptly. Owned readiness/model processes
are cancelled and awaited through reaping, not dropped; blocking local I/O and
process reaping prevent an unconditional OS-level return-by-deadline guarantee.

The MCP raw SSE event cap is 1,048,576 bytes. **JSON responses may already be
buffered and decoded by reqwest/rmcp before the serialized-size check**: it is not
a hard raw-HTTP allocation limit. Use controller `MemoryMax=512M` as illustrated.
An OOM kill is a recoverable crash, not a clean report or proof of zero spending.

Reservations are saved before invocation; every started call consumes a call slot.
Missing/invalid cost or any token class means unknown accounting, not zero. Nonzero
child exit and unknown accounting stop further inference; valid already-submitted
reports remain. A proposal that fails a check fails only its own item: its failed
report's summary reads `Proposal rejected: <site>: <check>` (for example
`Proposal rejected: finding 1 evidence 1: excerpt is not a substring of s1`), so
`maintenance_get_run` shows why without cluster access. The same reason is kept in
the journal's `rejection` field and logged once to stderr at warn level. It names
only indices, packet source ids and the check, never excerpt or source text. Ambiguous/failed reservations are not refunded.
The CLI budget flag, USD reservations and observed token totals are stopping
thresholds, **not hard in-flight billing/token guarantees**. A request may overshoot;
provider-enforced quotas or a separately approved enforcing gateway are required
for strict ceilings. A new completed-run window is not a daily/account-wide cap.

### Private state, recovery and diagnosis

The Linux journal supports x86/x86_64/aarch64; other platforms fail closed. Use an
absolute, operator-owned state directory (0700), files 0600, no symlink components,
and trusted ancestors owned by root or that operator. Non-sticky group/world-writable
ancestors are rejected. The stable `lock` inode must never be unlinked or replaced
while a runner may hold it. A local lock is not distributed exclusion; server leases
remain authoritative across machines.

`journal.json` includes lease tokens, pinned evidence, pending reports, accounting
and exact mutation keys. It is sensitive despite containing only a SHA-256
fingerprint of the MCP credential. Keep encrypted/access-controlled backups of
private state while the service is stopped; preserve ownership and modes. A
canonical export does not back up this local journal. Do not copy state to another
endpoint/catalog/key or edit it to discard an inconvenient pending operation.

Recovery binds to the full normalized HTTPS endpoint (including path/query),
origin, catalog and credential fingerprint. Changed credentials/binding, corruption
or insecure permissions refuse recovery. Resolve deliberately with the operator
and server authority, not by deleting the journal and paying again. A pending claim,
renewal, submit or close replays its exact normalized payload and idempotency key.
Crash after `invocation_started` means potentially spent money: recover reports and
close without another model call in that run. Invocation count, reservations and
known usage survive restart; the recovery process has a fresh bounded local wall
window. New work/submission remains subject to the original server deadline/lease
authority. After exact pending replay, an authoritative terminal read abandons
unreported local work without inventing receipts. Expired unfinished running work
uses a journaled failure-close acknowledgment, not renewal or inference; that
recovery close does not require a live lease or unexpired server deadline. Only a
durable terminal run permits a later fresh run's spending window. Do not infer
permission to retry from an apparently idle process or a missing response.
The first stop reason remains immutable for reporting (for example, `transport`
and exit 4). Successful exact replay, run lookup and authority checks allow
acknowledged terminal cleanup without any new inference even after that historical
transport stop. A current permission, protocol or transport failure still blocks
cleanup; unresolved requests are never discarded or acknowledged locally by guess.

The owned CLI installs a global tracing subscriber **before runtime/SDK
activity**, regardless of `RUST_LOG`, with a fixed filter that passes only the
`okf_maintenance::rejection` warn line described above; everything else is dropped. This removes the inherited rmcp logging hazard:
rmcp can trace full protocol payloads, source bodies and tokens. There is no verbose
dependency override. Library embedders must establish their own safe logging policy;
the library does not globally reconfigure its host. Never enable prompt/protocol
logs to diagnose a fixed error.

Use exit status, safe summary counts/reason, file ownership/modes, pinned version/help,
and separately authorized server-side permission/run inspection. Check `Config` /
`Contract` against documented fields, version and schemas; `Busy` against the owned
service; `Permission` against credentials/grants and private paths; runtime failures
against the durable recovery state. Inspect state only in a protected environment,
never paste it or child output into tickets. Fix cause before rerunning. A
transport/permission/protocol loss may deliberately leave work for recovery/expiry,
without remote cleanup or guessed lease authority.

### Offline evaluation and mandatory live gates

```sh
cargo run --bin okf-maintenance -- evaluate --cases tests/fixtures/investigator/corpus/evaluation.json
```

Evaluation requires no config, credentials, network or model subprocess. Input is
one version-1 JSON corpus, at most 16 MiB / 10,000 cases; see the
[fixture format and metric definitions](../tests/fixtures/investigator/corpus/README.md).
Each case has `id`, `provenance`, `annotation_author`, `review`, `packet`, raw
`proposal`, `expected` relation keys and per-key `admissibility`. Provider envelopes
are not corpus proposals. Unknown/duplicate DTO fields and invalid corpus shapes
fail closed. Predictions require the whole production proposal gate and exact
citations; relation keys are category plus sorted distinct evidence paths, not
assertion wording or revision numbers.

Output fields: `cases`, `true_positive`, `false_positive`, `false_negative`,
`rejected_citations`, `false_verification`, `rejected_verification`,
`excluded_unreviewed`, `precision`, `recall`. Precision is TP/(TP+FP), recall
TP/(TP+FN); undefined denominators are JSON null. Ambiguous, unreviewed or
incompletely annotated cases are excluded from both denominators. Rejected
proposals yield no predictions and leave expected keys unmatched. Citation and
forbidden-verification diagnostics include excluded cases. `false_verification`
counts accepted structured `SupportedCurrent` results (must remain zero), not a
natural-language detector. `rejected_verification` counts attempted forbidden
assessment/outcome fields.

The generated synthetic fixture has 11 cases, TP=4, FP=2, FN=3, one rejected
citation, zero false verification, two rejected verification attempts and two
excluded cases; precision 4/6, recall 4/7. These are parser/mapping specifications,
**not model-quality measurements or independent human review**. No real paid
response has been captured or verified by this implementation.

Before any scheduling, require **both separate gates**:

1. Independent human review of the synthetic corpus and its labels, covering
   valid/invalid links, similar-name distinct entities, copies, history versus
   contradictions, unverifiable aging and injected instructions. Generated
   `synthetic_specification` labels or merely setting `human_reviewed` do not
   satisfy this review. Choose and document acceptable quality criteria.
2. A **separately authorized one-primary live smoke run** against the exact pinned
   CLI/model, with both consents true, `batch_size=1`, `max_invocations=1`,
   `invocation_microusd=100000` and `run_microusd=100000`. Use approved sanitized
   source data and a fresh authorized state binding; do not overwrite a recoverable
   production journal. Arrange an explicitly approved sanitized envelope capture
   without enabling dependency logs or storing credentials/private source bodies.
   Confirm the target result has `type:"result"`, `subtype:"success"`,
   `is_error:false`, object `structured_output`, nonnegative `total_cost_usd`,
   and explicit integer input/output/cache-creation/cache-read usage fields.
   Human-review evidence, accounting, privacy and absence of canonical writes.
   The ordinary CLI intentionally does not export raw envelopes; any capture
   procedure is a separate operator authorization, not a debug flag here.

Version/help preflight, synthetic parser tests and generated labels satisfy neither
live gate. Leave the timer disabled when smoke evidence is absent or failed.
Accounts, keys, quotas and paid smoke remain operator work, not test-suite actions.

### Disabled systemd examples

[Service](../examples/maintenance/okf-maintenance.service) and
[timer](../examples/maintenance/okf-maintenance.timer) are repository examples only.
**Nothing installs or enables them.** They contain no `[Install]` targets and no
installation scripts. Both consent flags in the matching config remain false.

After authorization and both gates, the operator may provision a dedicated
`okf-maintenance` user/group, root-owned pinned executables under
`/opt/okf-maintenance`, and `/etc/okf-maintenance` owned by
`root:okf-maintenance` (0750). Keep `config.toml` owned by
`root:okf-maintenance` (0640) and `credentials.env` root-owned 0600, containing
only the two required environment assignments. The system service manager reads the protected
EnvironmentFile. Do not put the pinned model executable under a user home or rely
on the developer's PATH, repository tree or interactive credentials.

The service uses `UMask=0077`, `StateDirectory=okf-maintenance`,
`StateDirectoryMode=0700`, explicit `/var/lib/okf-maintenance` writable state,
`PrivateTmp`, `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome=true`, and
`MemoryMax=512M`. For `Type=oneshot`, `TimeoutStartSec=1800` is the execution cap
(not `RuntimeMaxSec`); matching `max_run_seconds=1740` leaves 60 seconds for orderly
shutdown. `KillMode=control-group` handles the service's remaining descendants on
forced stop. This is defense in depth, not an egress firewall or billing guarantee.

The timer is daily with a randomized delay of up to one hour, persistent catch-up,
and `Unit=okf-maintenance.service`. Persistent catch-up may run soon after boot;
authorize that behavior too. An operator who elects to install the reviewed files
can reload systemd and explicitly start the timer. To persist activation across
boots, they must deliberately add an appropriate install target or wants-link;
this repository does not do so. Do not start it merely because full tests passed.

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

1. Release a new version with `just release-prepare <patchlevel>` and
   `just release-ship` (below). The image workflow publishes `:<version>`.
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
  sessions do not; clients reconnect and carry on with the same handles.

### Preparing and shipping a release

Prerequisites: Bash, Python 3.11+ (standard library only), Git, just, the usual
Rust/build/test prerequisites, `cargo install cargo-release --version 0.25.18
--locked`, and authenticated GitHub CLI (`gh auth login`) with permission to
push a branch and merge its PR. `origin` must name this repository on github.com
(HTTPS or SSH), with exactly one resolved fetch URL and one matching resolved
push URL; extra URLs are rejected before any release push. Use a full-history
clone with the previous release tags fetched; notes are computed from the
reachable stable tags and committed history. Do not run releases concurrently.

```sh
git switch main
git pull --ff-only
just release-prepare patch   # required: patch, minor, major, or exact 1.2.3
just release-ship
```

Preparation requires a clean `main` exactly equal to fetched `origin/main`.
It rejects non-stable or non-increasing versions and existing local or remote
version tags before changing versioned files. cargo-release bumps `Cargo.toml`
and `Cargo.lock`; its hook updates the four pinned image version mentions in
this document, writes `changelog/<UTC-timestamp>-release-v<version>.md` from
committed history since the latest reachable stable tag, and runs `just fmt`,
`just lint`, and `just test`. These checks respect `RUST_TEST_THREADS` if set.
One local `chore: Release v<version>` commit and a marked annotated version tag
are created. Nothing is pushed or published to crates.io.

Shipping checks that `HEAD` is that prepared, single-parent commit and its
annotated tag matches the package version. A published tag is always refused.
Before opening or reusing `release/v<version>`'s PR, it reruns the three local
checks. **There is no PR CI configured:** this is local validation, not a claim
that absent CI passed. The script does not wait for nonexistent checks. A
conflicting remote release branch, ambiguous/closed PR, API failure, or main
moving before merge stops shipping without cleanup. Branch creation uses an
empty expected-ref lease: it can create a missing branch only, never overwrite
an existing one, even if another actor creates it concurrently.

The PR is merged with its expected head commit guarded by `gh`. Before any tag
publication, the script fetches main and verifies the merge's parents are the
original base and prepared commit, its tree equals the locally validated tree,
and it is on main. A concurrent main change incorporated into the merge is
therefore refused, even if it would leave the same tree. Only then may the
unpublished preparation tag be moved locally to the actual merge commit and
pushed (no remote ref is force-updated). Both pushes explicitly disable
follow-tags, even when `push.followTags=true`; unrelated local tags stay local.
The tag push specifies its full source and destination, so `remote.origin.push`
mappings cannot redirect publication. No branches are automatically deleted.
The existing tag workflow builds and publishes the **server image only**; it
does not package the maintenance runner, create a GitHub release, update a
`latest` image, or deploy anything. Check the image workflow separately after
shipping; tag-push success does not prove image publication succeeded.

#### Failures and retries

There is no destructive rollback. Inspect `git status`, `git diff`, local tags,
remote refs, and the PR before taking further action. Preparation failures can
leave changed manifests/examples, new notes, staged files, or a local release
commit/tag if a later step failed. Do not rerun preparation on that dirty or
ahead-of-origin state. Preserve any wanted work and manually clean up only the
release artifacts you have identified; return main to origin/main before
preparing afresh. Never delete or replace a published tag.

If shipping stops after a branch push or PR creation, rerun `just release-ship`;
it reuses the matching branch/PR. If the merge succeeded but its API response or
the tag push failed, rerun the same command: it validates the already merged PR
and retries the unpublished tag, including when the local tag was already moved.
Later commits on main do not invalidate that verified merge. Local main stays
at the prepared commit throughout, so retries retain the original identity.
After successful publication, fast-forward it manually with `git fetch origin
main` and `git merge --ff-only origin/main`. An already published tag causes a
retry to refuse, not to retag or republish.

If main moved before the release merged, or the merged tree/parents differ,
manual reconciliation is required; no tag is published. Review the PR and
coordinate a fresh release rather than forcing a tag or branch. The scripts
never close a PR, delete a branch, reset main, or discard changes for you.

The release integration matrix uses throwaway repositories, synthetic bare
origins, actual just dispatch and cargo-release, and fake GitHub/check commands:

```sh
python3 -B -m unittest discover -s scripts -p 'test_release.py' -v
```

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
| A tool returns `UnknownChangeset`, or `Refused` naming a closed changeset ("is published", "is discarded", "was published or discarded while this write waited", "is no longer open") | `UnknownChangeset`: the handle is mistyped or belongs to another key (only its key, and an `admin` that reaches its catalog, may use it). `Refused`: its changeset is closed, by `publish`, by `discard`, or by the prune of changesets left empty for 24 hours, possibly while the call waited for it. `list_changesets` lists the open handles the key may use; otherwise call `open_changeset` |
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
