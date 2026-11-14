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
  | `POST /catalogs/{name}/import[?replace=true&only_machinery=true&expect_generation=N]` | `admin` on the catalog | Imports an uploaded `.tar.gz` |

  Keys are bearer tokens (`Authorization: Bearer <key>`). Only `/healthz` is
  open, and it names no catalog, host or secret.
- **The command line** (`okf-mcp catalog`, `key`, `import`, `export`,
  `migrate`) talks to the database directly, not to the server. The running
  server notices catalogs, imports and key changes within `poll_interval`.

## 2. The image

- **Name:** `ghcr.io/hypotheosis/okf-mcp:<version>`, for example `:0.15.0`.
  Every build is also tagged `:sha-<short commit>`. There is no `:latest`:
  manifests pin a version.
- **Built by** `.github/workflows/image.yml` when a `v*` tag is pushed (`v0.15.0`
  gives `:0.15.0`), and on a manual dispatch (which publishes `:sha-<short>`
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
| `public_url` | none | The server's public origin, such as `"https://okf-mcp.example.com"` (no path, query or user info; a trailing `/` is dropped). Setting it turns on OAuth ("OAuth connectors") and console sign-in by account ("Signing in and self-service"); without it the OAuth and sign-in routes are 404. It must be `https`, except on a loopback host (such as `localhost`, `127.0.0.1` or `::1`) for local testing, and its host must be in `allowed_hosts` |
| `oauth.access_token_ttl` | `"1h"` | How long an OAuth access token works |
| `oauth.refresh_token_ttl` | `"30d"` | How long a refresh token works. Each refresh issues a new one with a fresh lifetime, so a connection used within this stays signed in |
| `oauth.code_max_expiry` | `"7d"` | The longest expiry a sign-in code may be given (`oauth code create --expires`, the console's expiry) |
| `oauth.dcr_enabled` | `true` | Accept dynamic client registration (RFC 7591) at `/oauth/register`. Clients that publish a Client ID Metadata Document work either way |
| `oauth.cimd_allow_private` | `false` | Fetch Client ID Metadata Documents from loopback and private addresses. For test rigs only: it opens the fetcher to the internal network |
| `auth.password_sign_in` | `true` | Offer email and password sign-in on `/signin`. The default for the `password_sign_in` setting ([Signing in and self-service](#signing-in-and-self-service)) |
| `auth.max_keys_per_user` | `20` | The most live self-issued keys a user may hold, `0` to `1000` (`max_keys_per_user` setting) |
| `auth.max_key_lifetime` | none | The longest expiry a self-issued key may have, such as `"90d"`; unset means no limit (`max_key_lifetime` setting) |
| `auth.console_session_idle` | `"12h"` | A console session unused this long ends (`console_session_idle` setting). Not `session_idle_timeout`, which is MCP's |
| `auth.console_session_absolute` | `"7d"` | A console session ends this long after sign-in, used or not (`console_session_absolute` setting) |
| `auth.setup_link_ttl` | `"24h"` | How long a setup link works (`setup_link_ttl` setting) |
| `auth.reset_link_ttl` | `"1h"` | How long a password reset link works (`reset_link_ttl` setting) |
| `auth.trusted_proxy_header` | none | The header whose last address is the client's, such as `"x-forwarded-for"`, behind an ingress that appends it. Unset: the TCP peer's address, and no header is read. The sign-in limits and the sign-in history use it. Config only, not a setting |
| `[[auth.oidc]]` | none | One table per OIDC provider: `id` (1 to 32 of `a-z`, `0-9` and `-`; stable, since each linked identity stores it), `label` (the button text), `issuer`, `client_id`, `client_secret_env` (the environment variable holding the secret) and `scopes` (default `["openid", "email", "profile"]`). Needs `public_url` |
| `mail.provider` | `"log"` | `"smtp"`, `"postmark"` or `"log"`. **`log` is for development only:** it writes each whole message, setup and reset links included, to the log at `info`, and delivers nothing |
| `mail.from` | `"okf <okf@localhost>"` | The From address, such as `"okf <okf@example.com>"`; it must parse as one mailbox. Set it for `smtp` and `postmark` |
| `mail.worker` | `true` | Run the mail worker in `serve`. Its own pool opens up to 4 connections, on top of `database_max_connections` |
| `mail.smtp.host`, `mail.smtp.port`, `mail.smtp.starttls`, `mail.smtp.username`, `mail.smtp.password_env` | none, `587`, `true`, none, none | The SMTP relay. `starttls = true` requires STARTTLS; `false` is TLS from the first byte (port 465); there is no plain-text mode. `username` and `password_env` (the environment variable holding the password) go together |
| `mail.postmark.token_env`, `mail.postmark.message_stream` | none, `"outbound"` | Postmark's server token (by environment variable) and message stream |

The server refuses to start (and every subcommand refuses to run) with a zero
`poll_interval`, `session_idle_timeout`, `database_max_connections` or
`overlay_cache_size`, an empty `allowed_hosts` or a blank
entry in it, or a `time_server` that is blank or has surrounding whitespace.
`maintenance_review_intervals` also rejects invalid catalog names, malformed
durations, durations below 86,400 seconds and durations beyond the supported
range. Omitted catalogs use 30 days. It also refuses a zero `oauth.*`
lifetime, and a `public_url` that is not a URL, has a path, query or user
info, is `http` on a host other than a loopback one, or whose host no
`allowed_hosts` entry admits (an entry with a port must match the URL's
port).

For `[auth]` it refuses a zero duration or one over ten years,
`max_keys_per_user` over 1000, and a `trusted_proxy_header` that is not a
header name. An `[[auth.oidc]]` table needs `public_url`, and each needs
every key but `scopes`. Unlike the rest of the file, it refuses a key it does
not know, so a misspelt key or an inline `client_secret` is an error. It
also refuses an `id` that is malformed or listed twice, an empty `label`,
`client_id` or `client_secret_env`, and an `issuer` that is not `https`
(`http` only on a loopback host) or has a query or fragment. For `[mail]`
it refuses a `from` that is not one address, an `smtp` provider without a
`[mail.smtp]` table, with a blank host, port 0, or `username` without
`password_env` (or the reverse), and a `postmark` provider without a
`[mail.postmark]` table or with a blank `token_env` or `message_stream`.
`serve` also refuses to start when a variable named by `client_secret_env`,
`password_env` or `token_env` is unset or empty, naming the variable (never
a value). The other subcommands never read those variables: `user invite`
and `mail test` only queue mail, which a running server sends.

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
public_url = "https://okf-mcp.example.com"   # turns on OAuth; see "OAuth connectors"

[oauth]
access_token_ttl = "1h"
refresh_token_ttl = "30d"
code_max_expiry = "7d"
dcr_enabled = true
cimd_allow_private = false

[auth]
password_sign_in = true
max_keys_per_user = 20
max_key_lifetime = "90d"
console_session_idle = "12h"
console_session_absolute = "7d"
trusted_proxy_header = "x-forwarded-for"   # behind an ingress only

[[auth.oidc]]
id = "example"
label = "Example ID"
issuer = "https://id.example.com"
client_id = "okf-console"
client_secret_env = "OKF_OIDC_EXAMPLE_SECRET"

[mail]
provider = "smtp"
from = "okf <okf@example.com>"

[mail.smtp]
host = "smtp.example.com"
port = 587
starttls = true
username = "okf"
password_env = "OKF_SMTP_PASSWORD"
```

`[oauth]`, `[auth]` and `[mail]` are TOML tables, so they come after every
top-level key: a top-level key written below one would land inside it.

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
| `OKF_OIDC_EXAMPLE_SECRET` (any name) | An OIDC client secret, named by its provider's `client_secret_env` |
| `OKF_SMTP_PASSWORD` (any name) | The SMTP password, named by `mail.smtp.password_env` |
| `OKF_POSTMARK_TOKEN` (any name) | The Postmark server token, named by `mail.postmark.token_env` |

Logs go to stderr. They never carry the database URL, an API key or a
document's text, nor a password, session id, CSRF token, setup or reset
link, or client secret. Each request's line names the method and the matched
route (`/api/v1/users/{name}/sessions/{id_prefix}`), never the literal path
or the query, so neither an id in a path nor an emailed link's token reaches
the log. The `log` mail provider is the one exception: it logs whole
messages, links included, which is why it is for development only.

Set `OKF_MCP_CONFIG` in the container's environment rather than passing
`--config` as an argument: then `kubectl exec ... -- okf-mcp <subcommand>`
reads the same file as the server, with no extra flags.

## 4. Postgres

- **One database and its owner role**, for example `okf_mcp` owned by
  `okf_mcp`. The server needs no superuser. The migrations create three
  extensions, `citext` (okf's) and `pgcrypto` and `hstore` (apalis', for the
  mail queue). All three are trusted extensions, so the database's owner can
  create them (Postgres 13 or later). The pool opens up to
  `database_max_connections` (16 by default) connections, and the mail
  worker's own pool up to 4 more; keep the database's `max_connections`
  above that sum times the number of replicas.
- **Two schemas.** okf's tables are in `public`; the mail queue is apalis'
  `apalis` schema, which `migrate` sets up after okf's migrations, with its
  own migration table (`apalis._sqlx_migrations`).
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
    public_url = "https://okf-mcp.example.com"
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
          image: ghcr.io/hypotheosis/okf-mcp:0.15.0
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

**Create a user** for each person who will hold a credential. Every key,
sign-in code and catalog needs one: a key or code belongs to a user, and a
catalog to a team. Creating user `alice` also creates her personal team
`alice` ([Users and teams](#users-and-teams) has the rest):

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- okf-mcp user create alice --display-name "Alice Example"
```

**Create a catalog**, owned by a team (a user's name names that user's
personal team):

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp catalog create notes --description "Team notes" --owner alice
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
`--user` names the user the key belongs to, which must exist; it changes
nothing about what the key reaches. Reporting is independently opt-in via
`key create --maintenance-report`: it requires read access to the catalog and
never grants write access; write/admin do not enable reporting by themselves:

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create claude --access write --catalogs notes --default notes --actor claude-code-tui --user alice
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create claude --access write --catalogs notes --default notes --actor claude-ai-web --user alice
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp key create ops-admin --access admin --all --user alice
```

The positional value is a grouping name, so the two `claude` credentials are
listed together while retaining separate actors, grants, defaults and
revocation. `key create` prints the key once and its immutable administration
id on stderr; only the key's SHA-256 is stored. Put it straight
into a password manager or a secret store. Never paste it into logs, tickets,
chat or shell history (a leading space keeps a command out of history in most
shells). `key list` shows name, actor, id, access, state, grants, `default
X`, `maintenance_report true|false` and `user U` (`-` for a key from before
users existed that nobody has assigned), grouped by name. Use
`okf-mcp key maintenance-report ID --enabled true|false [--actor ACTOR]` to
change the reporting flag. `revoke`, `grant`, `ungrant` and `maintenance-report`
accept an id, an unambiguous live name, or `NAME --actor ACTOR` of an API key;
they never match an OAuth grant, whose name may repeat a key's (see "Mint a
sign-in code" below); for example,
`okf-mcp key grant claude work --actor
claude-ai-web`. A revoked name/actor pair can be created again for rotation;
the old row and its changeset history remain attached to the old id. A revoked
key stops working within `poll_interval`.

**Mint a sign-in code** for a person connecting an OAuth client. Codes are
redeemed on the OAuth consent page, which exists only once OAuth is on
(`public_url` is set). The code is printed once. It is single-use and expires
unused after 24 hours unless `--expires` says otherwise, up to
`oauth.code_max_expiry` (7 days by default). Access is `read` or `write`;
`admin` is refused. `--user` (required) names the user the code and its grant
belong to; `--actor` defaults to the code's name. The person enters the code
on the consent page, which turns it into an OAuth grant: a key row named
`<code name>.<client host>-<8 characters of the code id>`, with the code's
catalogs, access, default, actor and user. `okf-mcp key ...` never acts on a grant,
except `key assign`, which gives a key or a grant its user
([Users and teams](#users-and-teams)); `okf-mcp oauth grant revoke ID` revokes one (see
[Managing codes and grants](#managing-codes-and-grants) for every
subcommand, and [OAuth connectors](#oauth-connectors) for the whole flow):

```sh
kubectl -n okf-mcp exec deploy/okf-mcp -- \
    okf-mcp oauth code create laptop --actor example-actor --access write --catalogs notes --default notes --user alice
```

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
a non-empty catalog, `?only_machinery=true` for `--only-machinery` and
`?expect_generation=N` for `--expect-generation N`, both described below):

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
catalog has moved since; export again and redo the edit. Over HTTP, the
same guarded machinery-only import is:

```sh
curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" --data-binary @notes.tar.gz \
    "https://okf-mcp.example.com/api/v1/catalogs/notes/import?replace=true&only_machinery=true&expect_generation=12"
```

**Connect a harness.** The MCP URL is `https://okf-mcp.example.com/mcp`, with
the key in `Authorization: Bearer <key>`. For Claude Code:

```sh
claude mcp add --transport http okf https://okf-mcp.example.com/mcp \
    --header "Authorization: Bearer $OKF_MCP_KEY"
```

`X-OKF-Catalogs` (or its alias `X-OKF-Catalog`, or `?catalogs=a,b`) picks
the connection's catalogs, and bounds them: the session sees no other
catalog, whatever the key's grants or default. A request with both header
spellings reads them as one list, the `X-OKF-Catalogs` names first. No
header names a changeset: an agent calls `open_changeset` and passes the
handle it returns as `changeset` to every write. The README's "Connecting a
client" and "Changesets" sections describe both.

**Protocol versions:** the server accepts MCP `2024-11-05`, `2025-03-26`,
`2025-06-18`, `2025-11-25` and `2026-07-28` (rmcp 3.8's known versions). A
client on a version before `2026-07-28` gets a stateful session
(`Mcp-Session-Id`); one on `2026-07-28` is served statelessly.

## Users and teams

Every API key, OAuth grant and sign-in code belongs to a **user**, and every
catalog to a **team**:

- Creating a user also creates its **personal team**, of the same name, with
  the user as its owner. A personal team may take more members.
- A **team** has members, each an `owner` or a `member`. The role governs
  only the team's own membership.
- A catalog is owned by exactly one team, and a credential belongs to exactly
  one user (never a team). `actor` stays as each credential's provenance
  label.
- **Ownership grants no access.** A credential reaches exactly the catalogs
  it is granted; owning a catalog, or being in the team that owns it, adds
  nothing.

User and team names are `[a-z0-9-]+` and are unique across both: `user
create alice` is refused while a team `alice` exists, and the other way
round. Every command below needs the database, as every subcommand does; the
REST routes and console controls for each are in the
[parity table](#cli-api-and-console-parity). A running server picks up each
change within `poll_interval`; the same change through the API or the console
takes effect at once.

| Command | Does |
|---|---|
| `okf-mcp user create NAME [--display-name TEXT]` | Creates the user and its personal team `NAME` |
| `okf-mcp user list [--email ADDRESS]` | One tab-separated line per user: name, `credentials N` (keys, grants and codes naming it, revoked ones included), `email E` (`-` when unset), `admin yes\|no`, `teams a,b`, then the display name. `--email` prints only the user with that email, in any letter case, and nothing when no user has it |
| `okf-mcp user delete NAME` | Deletes a user nothing references, with its personal team (see below) |
| `okf-mcp user set-admin NAME --on\|--off` | Gives or takes the admin flag: an admin user's console session acts as an admin that reaches every catalog. A change signs the user out of every console session. `--off` for the last admin user is allowed here (operator access), refused over REST |
| `okf-mcp user set-email NAME EMAIL` (`--clear`) | Sets the email a user signs in and gets mail with (unique, case-insensitive), or clears it. A change ends the user's outstanding emailed links |
| `okf-mcp user invite NAME` | Queues a setup email with a single-use link (valid `setup_link_ttl`) to the user's email; an older setup link stops working. Needs `public_url`; prints the address, never the link. It only queues: a running server's mail worker sends it, so the command needs no mail secret |
| `okf-mcp user entitle NAME CATALOG --access read\|write` | Adds a catalog to the user's ceiling, or changes its level |
| `okf-mcp user unentitle NAME CATALOG` | Removes an entitlement |
| `okf-mcp user sessions NAME` | The user's console sessions, most recently used first, one per line: id prefix (never the id), `created T`, `seen T`, `ip A`, then the browser |
| `okf-mcp user revoke-sessions NAME` | Signs the user out of every console session; keys and grants keep working. Prints `revoked N sessions of NAME` |
| `okf-mcp user sign-ins NAME [--limit N]` | The user's recent sign-in attempts (20 by default), newest first: time, method, outcome, `ip A` |
| `okf-mcp team create NAME [--display-name TEXT] --owner USER` | Creates a shared team with `USER` as its first owner |
| `okf-mcp team list` | One line per team: name, `personal U` (`-` for a shared team), `members u(role),...`, `catalogs c,...` (`-` for none), `suspended TIME` (`-` when active), then the display name |
| `okf-mcp team add-member TEAM USER [--role owner\|member]` | Adds a member (`member` by default), or gives a member that role |
| `okf-mcp team set-role TEAM USER --role owner\|member` | Changes a member's role, adding the user if it is not a member |
| `okf-mcp team remove-member TEAM USER` | Removes a member |
| `okf-mcp team delete NAME` | Deletes a shared team that owns no catalog; its memberships go with it |
| `okf-mcp team suspend NAME [--notice TEXT]` | Suspends the team: its catalogs refuse every non-admin credential except export (see [Suspending a team](#suspending-a-team)). Prints `suspended team NAME`. Repeating it replaces the notice (none when `--notice` is left out) and keeps the first suspension time |
| `okf-mcp team unsuspend NAME` | Lifts the suspension. Prints `lifted the suspension of team NAME`; no change when the team is active |
| `okf-mcp catalog create NAME --owner TEAM [--description TEXT]` | `--owner` is required |
| `okf-mcp catalog set-owner NAME --team TEAM` | Moves a catalog to another team, or gives a legacy catalog its first owner. Prints `catalog C: owner OLD -> NEW` (`(none)` for a legacy catalog) |
| `okf-mcp catalog set-member-access NAME --access none\|read\|write` | What members of the owner team reach through their own keys and grants (default `write`). Prints `catalog C: member access OLD -> NEW` |
| `okf-mcp key create NAME --user USER ...` | `--user` is required; `--actor` still defaults to `NAME` |
| `okf-mcp key assign ID --user USER [--actor ACTOR]` | Gives a key or an OAuth grant a user, or changes it; access is unchanged. `ID` is an id, or an unambiguous live name (`--actor` narrows it). The only `key` subcommand that also acts on grants. Prints `key ID now belongs to USER` |
| `okf-mcp oauth code create NAME --user USER ...` | `--user` is required; `--actor` defaults to `NAME` |
| `okf-mcp oauth code assign ID --user USER` | Gives a sign-in code, in any state, a user. Prints `sign-in code ID now belongs to USER` |
| `okf-mcp unassigned` | Every key, grant, sign-in code and catalog with no user or owner, one per line (`key NAME (id ID, actor ACTOR)`, `grant NAME (id ID)`, `code NAME (id ID)`, `catalog NAME`), or `nothing is unassigned` |

`catalog list` prints name, `generation N`, `owner T` (`-` when unassigned),
then the description. `key list`, `oauth grant list` and `oauth code list`
end with `user U` (`-` when unassigned). Every listing is tab-separated with
no header row, so `cut -f` works on it.

A shared catalog belongs to a team with several members:

```sh
okf-mcp user create alice --display-name "Alice Example"
okf-mcp user create bob
okf-mcp team create family --display-name "The family" --owner alice
okf-mcp team add-member family bob
okf-mcp catalog create recipes --owner family
okf-mcp key create bob-laptop --access write --catalogs recipes --default recipes --user bob
okf-mcp team list
```

### Migrating an existing deployment

Upgrading to a release with users and teams (migration `0009`) creates no
user and assigns nothing: every key, grant, sign-in code and catalog from
before it stays **unassigned** and keeps working exactly as before. Assign
them as follows (inside the pod, with `kubectl exec` as in
[First run](#6-first-run)):

1. List what is unassigned:

   ```sh
   okf-mcp unassigned
   ```

2. Create a user for each person who holds a credential, and a team for each
   group that shares a catalog:

   ```sh
   okf-mcp user create alice
   okf-mcp team create family --owner alice
   ```

3. Give each key and grant a user (by id, or by an unambiguous live name),
   each sign-in code a user, and each catalog an owner:

   ```sh
   okf-mcp key assign claude --actor claude-code-tui --user alice
   okf-mcp key assign GRANT_ID --user alice     # a grant, by its id
   okf-mcp oauth code assign CODE_ID --user alice
   okf-mcp catalog set-owner notes --team family
   ```

4. Run `okf-mcp unassigned` again, and repeat until it prints `nothing is
   unassigned`.

The console does the same at Users → "What is unassigned"
(`/ui/#/users/unassigned`), with a user or team picker and Assign on each
row; the Users link shows the count while anything is unassigned. Nothing
forces the migration to finish: assigning is bookkeeping, and an unassigned
credential authenticates as before (`whoami` shows `user: null`). Making the
columns `NOT NULL` once nothing is unassigned is future work (#296).

### Deletion rules

Users and teams that something refers to are kept, so the owner of a
credential or catalog never disappears. Each refusal is a 409 over REST, and
a non-zero exit naming what still refers to it on the command line:

- `user delete` is refused while any key, grant (revoked ones included) or
  sign-in code names the user, while the user belongs to a team other than
  its personal team, or while its personal team owns a catalog. Revoked keys
  and grants are kept, so a user is refused for as long as any of them still
  names it; `key assign` can move even a revoked key or grant to another
  user, and a sign-in code nobody redeemed is pruned a week after it
  expires.
- `team delete` is refused for a personal team (it goes with its user) and
  for a team that owns a catalog.
- `team remove-member` and `team set-role` never remove or demote a team's
  last owner, and `remove-member` never removes a personal team's user.

An unknown user or team is a 404 (`{"error": "UnknownName", "message": ...,
"noun": "user", "name": ...}`) on every route that names one, including the `user` and
`owner` of a create body; on the command line it is a non-zero exit naming
it. A create body without its required `user` or `owner` is a 422; on the
command line a missing `--user` or `--owner` is clap's usage error (exit 2).
Disabling a user or team instead of deleting it is future work (#297).

### Suspending a team

Suspending a team stops every catalog it owns from answering its members,
through MCP, the REST API and the console, until the suspension is lifted.
Nothing is deleted or changed: documents, history, open changesets, keys,
grants and memberships all stay, and lifting the suspension restores every
credential exactly as it was. okf-mcp records no reason; the optional
**notice** (at most 2000 characters) is what members are told.

```sh
okf-mcp team suspend family --notice "Paused while the account is reviewed."
okf-mcp team list             # family ... suspended 2026-11-13T12:00:00Z ...
okf-mcp team unsuspend family
```

Over REST it is `PUT /api/v1/teams/{name}/suspension` with
`{"suspended": true, "notice": "..."}` or `{"suspended": false}` (the
[API reference](#users-and-teams-api) has the details); in the console,
Teams → a team → Suspension → Suspend, or Lift suspension. The route and the
console need an admin that reaches every catalog, as the other team writes
do; the command line needs only the database, as every subcommand does.

- **What it refuses.** Every read and write of the team's catalogs by a
  non-admin credential (API keys, OAuth grants and console sessions, whoever
  issued them): every MCP tool but `list_catalogs`, changeset writes and
  publish, the
  maintenance tools, and the `/api/v1` browse, search, history, health,
  report, changeset and maintenance routes. The answer is 403 with
  `{"error": "Suspended", "message", "catalog", "team", "notice"}`
  (`notice` is `null` when there is none); the message names the catalog
  and the team and ends with the notice.
- **What stays open.** **Export** (`GET /catalogs/{name}/export` and
  `GET /api/v1/catalogs/{name}/export`) for any credential that reaches the
  catalog, so members can take a copy. Console sign-in. **Listing:**
  `list_catalogs` and `GET /api/v1/catalogs` still show the catalog, with
  `suspended` set to `{"team", "notice"}` (null for an active catalog), and
  `GET /api/v1/me`'s ceiling is unchanged. In the console the team and its
  catalogs carry a "suspended" pill, Browse shows the notice with a link to
  Import & export, and the 403 shows its own message.
- **Admins are exempt.** An `admin` key, or an admin user's console session,
  is not affected, so the operator can still inspect, export and repair the
  catalogs. Import needs `admin` anyway, so only an admin can import into a
  suspended catalog.
- **Only credentials that reach the catalog learn of it.** Reach is checked
  first: a credential that doesn't reach the catalog still gets
  `CatalogNotSelected` (404), and no listing shows it the catalog. A
  credential that reaches it below the level a route needs gets `Forbidden`
  from routes that check the level first, and `Suspended` from tools that
  select their catalog first (`open_changeset`, the maintenance tools).
- **Connection lists drop, named catalogs refuse.** A call that names a
  suspended catalog (an argument, the key's default or the server's
  `default_catalog`) is refused with `Suspended`. A call that falls back to
  the connection's list (`X-OKF-Catalogs`) leaves suspended catalogs out and
  reads the rest, so one suspended team doesn't break a connection that also
  reads other teams' catalogs; only a list whose every catalog is suspended
  is refused. `GET /api/v1/search` without `catalogs`, and `list_changesets`
  (and `GET /api/v1/changesets`) without `catalog`, leave suspended catalogs
  out the same way.
- **It follows ownership.** The suspension is the owner team's: a catalog
  moved to a suspended team (`catalog set-owner`), or created for one, is
  suspended too, and one moved away is not.
- **Repeating and lifting.** Suspending a suspended team keeps its first
  `suspended_at` and replaces the notice (a missing or blank notice clears
  it). Lifting a team that is not suspended changes nothing. An unknown team
  is a 404 (`UnknownName`); a notice over 2000 characters or holding a NUL
  is refused (409 `Refused`, a non-zero exit on the command line), leaving
  the team as it was. The console can't change a suspended team's notice
  without lifting it (#361); the CLI and REST can.
- **Timing.** On the single replica the server runs as, the REST route
  (which the console uses) polls before answering, so the next request sees
  the change.
  If that poll fails the change is still committed and takes effect at the
  poll loop's next successful pass, within `poll_interval`. The command line
  writes the database only, so a running server picks it up within
  `poll_interval`, as for every other `team` command.

### CLI, API and console parity

Every admin command for users, teams, assignment, sign-in, settings and
mail has a REST route and a console control. `tests/admin_parity.rs` fails
when a `user`, `team`, `settings` or `mail` subcommand, or one of the other
commands below, has no row here or its row names a route the API reference
does not document. A row may name a subcommand's flag (`user list --email`);
the test checks the flag exists.

| CLI | REST | Console |
|---|---|---|
| `okf-mcp user create` | `POST /api/v1/users` | Users → New user |
| `okf-mcp user list` | `GET /api/v1/users` | Users |
| `okf-mcp user list --email` | `GET /api/v1/users` | Users → the filter box |
| `okf-mcp user delete` | `DELETE /api/v1/users/{name}` | Users → user → Delete user |
| `okf-mcp team create` | `POST /api/v1/teams` | Teams → New team |
| `okf-mcp team list` | `GET /api/v1/teams` | Teams |
| `okf-mcp team add-member` | `PUT /api/v1/teams/{name}/members/{user}` | Teams → team → Add member |
| `okf-mcp team set-role` | `PUT /api/v1/teams/{name}/members/{user}` | Teams → team → a member's role |
| `okf-mcp team remove-member` | `DELETE /api/v1/teams/{name}/members/{user}` | Teams → team → Remove |
| `okf-mcp team delete` | `DELETE /api/v1/teams/{name}` | Teams → team → Delete team |
| `okf-mcp team suspend` | `PUT /api/v1/teams/{name}/suspension` | Teams → team → Suspension → Suspend |
| `okf-mcp team unsuspend` | `PUT /api/v1/teams/{name}/suspension` | Teams → team → Suspension → Lift suspension |
| `okf-mcp catalog set-owner` | `PUT /api/v1/catalogs/{catalog}/owner` | Catalogs → catalog → Change owner (Assign when unassigned); Users → What is unassigned |
| `okf-mcp key assign` | `PUT /api/v1/keys/{id}/user` | API keys → key or grant → Assign / Change user; Users → What is unassigned |
| `okf-mcp oauth code assign` | `PUT /api/v1/oauth/codes/{id}/user` | API keys → sign-in code → Assign / Change user; Users → What is unassigned |
| `okf-mcp unassigned` | `GET /api/v1/unassigned` | Users → What is unassigned (`/ui/#/users/unassigned`), and the count on the Users link |
| `okf-mcp user set-admin` | `PUT /api/v1/users/{name}/admin` | Users → user → Admin switch |
| `okf-mcp user set-email` | `PUT /api/v1/users/{name}/email` | Users → user → Email (Save, Clear) |
| `okf-mcp user invite` | `POST /api/v1/users/{name}/invite` | Users → user → Send setup email |
| `okf-mcp user entitle` | `PUT /api/v1/users/{name}/entitlements/{catalog}` | Users → user → Entitlements → Entitle, or a level |
| `okf-mcp user unentitle` | `DELETE /api/v1/users/{name}/entitlements/{catalog}` | Users → user → Entitlements → Remove |
| `okf-mcp user sessions` | `GET /api/v1/users/{name}/sessions` | Users → user → Console sessions |
| `okf-mcp user revoke-sessions` | `DELETE /api/v1/users/{name}/sessions` | Users → user → Sign out everywhere |
| `okf-mcp user sign-ins` | `GET /api/v1/users/{name}/sign-ins` | Users → user → Recent sign-ins |
| `okf-mcp catalog set-member-access` | `PUT /api/v1/catalogs/{catalog}/member-access` | Catalogs → catalog → Team member access |
| `okf-mcp settings list` | `GET /api/v1/settings` | Settings → Policy |
| `okf-mcp settings set` | `PUT /api/v1/settings/{key}` | Settings → Policy → a value's Save |
| `okf-mcp settings reset` | `DELETE /api/v1/settings/{key}` | Settings → Policy → Reset |
| `okf-mcp mail test` | `POST /api/v1/mail/test` | Settings → Mail → Send test email |
| `okf-mcp mail queue` | `GET /api/v1/mail/queue` | Settings → Mail queue |
| `okf-mcp mail retry` | `POST /api/v1/mail/queue/{id}/retry` | Settings → Mail queue → Retry |
| `okf-mcp mail delete` | `DELETE /api/v1/mail/queue/{id}` | Settings → Mail queue → Delete |

`GET /api/v1/users/{name}` and `GET /api/v1/teams/{name}` (a user's or a
team's drawer in the console) have no command of their own: `user list` and
`team list` show the same facts. Nor do `DELETE
/api/v1/users/{name}/sessions/{id_prefix}` (one session's Sign out in the
drawer; `user revoke-sessions` ends them all) and `GET
/api/v1/catalogs/{catalog}/member-access` (`catalog set-member-access`
prints the previous level). The self-service routes under `/api/v1/me` are
not admin actions and have no command. The users, teams and unassigned writes need
an admin key that reaches every catalog, as `POST /api/v1/catalogs` does;
reading users and teams needs any admin key, so a scoped admin can pick the
user of a key it creates. For any other admin the console shows these
controls disabled, or the Unassigned view a note, saying why.

### Audit log

Every change below writes one entry in the change's own transaction: the
change and its entry commit together, and a change whose entry cannot be
written fails. An entry about a catalog names it and belongs to its owner
team; a settings entry names the setting and belongs to no team.

| Action | Recorded when | Details |
|---|---|---|
| `catalog.published` | A publish writes at least one revision (one that changes nothing writes none and records nothing). Moves, splits and concept deletes are publishes and are recorded as such | `changeset` (the handle; the diff stays in the catalog's history), `files` (revisions written), `generation_from`, `generation_to` |
| `catalog.reverted` | A `revert` succeeds: a document's old revision (or its deletion) goes into a changeset. A refused revert records nothing | `path`, `revision`, `changeset` |
| `catalog.changeset_discarded` | An admin closes another key's changeset (`discard` without `paths`, or `DELETE /api/v1/changesets/{handle}`). A key discarding its own changeset records nothing | `changeset`, `files` (entries dropped), `reason` (`admin`) |
| `setting.changed` | `settings set` or `PUT /api/v1/settings/{key}` changes a setting ([Settings](#settings)). Setting the value already stored records nothing | `from` (the stored value before, `null` without an override) and `to`; for `support_contact` only `set` (whether a contact is set now), never the address |
| `setting.cleared` | `settings reset` or `DELETE /api/v1/settings/{key}` removes an override (with none, nothing is recorded) | `from`; nothing for `support_contact` |

The log is append-only: the database refuses any change to an entry, and
any delete of one younger than `audit_retention_days` (at least 30 days,
whatever the stored value). At its first poll after starting, and then
once a day, the server deletes the entries older than
`audit_retention_days` plus an hour (a margin for clock skew between the
server and the database); nothing else deletes them. Lowering the setting
takes effect at the next daily pass; raising it keeps the entries not yet
deleted.

## Signing in and self-service

People sign in to the console as themselves, with any of three methods: a
password, a passkey, or a linked account at an OIDC provider. Signed in,
they manage their own API keys and OAuth grants within a **ceiling** an admin
sets, and approve OAuth consent as themselves. Pasting an API key into the
console still works, for automation and for admins without a user.

Signing in needs `public_url` (section 3). It gives the cookies' origin, the
passkey relying party (its host) and the OIDC redirect URIs. Without it,
`GET /signin` offers only "Use an API key instead", and every other sign-in
route answers 404 (`POST /signin` included).

### Signing in

`GET /signin` offers:

- **Email and password**, while the `password_sign_in` setting is on (when
  it is off, the form is gone and `POST /signin` answers 404);
- **Sign in with a passkey**, while passkeys are on (below): the browser
  offers the passkeys it holds for this server, so no email is typed first;
- **one button per `[[auth.oidc]]` provider**;
- **Use an API key instead**: the console's pasted-key card.

It takes `return_to`, a same-origin path starting `/ui/` or
`/oauth/authorize?`; anything else becomes `/ui/`. The console's card links
to it with "Sign in with your account", carrying the page you were on.

- **One refusal for everything.** Every refused password sign-in (a wrong
  password, an unknown email, a locked account, a malformed form) answers
  401 "Email or password is incorrect", and costs the same: an unknown
  email is checked against a dummy hash.
- **Lockout.** After 10 consecutive failures an account locks for 15
  minutes. The lock is stored, so a restart does not lift it.
- **Per-address limit.** Each client address may try 30 times per 15
  minutes (a token bucket: a burst of 30, refilled over 15 minutes), shared
  by password sign-in, passkey sign-in and OIDC starts; past it, 429. IPv6
  addresses count by their `/64`. The limit is in memory, per process
  (sharing it across replicas is #357).
  Behind an ingress, set `auth.trusted_proxy_header`, or every request seems
  to come from the ingress and all of them share one limit.
- **Other sites' forms are refused.** Every sign-in, forgot, setup and reset
  form POST, and every open passkey route, answers 403 when the browser's
  `Sec-Fetch-Site` header is neither `same-origin` nor `none` (login CSRF).
  A request without the header, such as curl's, is let through.

Passwords are hashed with argon2id (19 MiB, 2 passes, 1 lane). A password
has 12 to 1024 characters and is not in the bundled list of common
passwords; there are no composition rules.

Every attempt writes a sign-in event (method `password`, `passkey`, `oidc`
or `setup`; outcome `ok`, `failed` or `locked`; the client address), which
the user sees on their Account page and an admin in the user's drawer or
with `okf-mcp user sign-ins NAME`, and the same facts as an `info` log line
with the user id. Events older than 90 days are pruned.

### Sessions

A sign-in starts a **console session**. The `okf_session` cookie holds 256
random bits (`HttpOnly; SameSite=Lax; Path=/`, and `Secure` when
`public_url` is `https`); the server stores only its SHA-256, and a CSRF
token derived from it, stored hashed too.

- **Lifetime.** A session ends after `console_session_idle` unused (default
  12 hours) or `console_session_absolute` after sign-in (default 7 days),
  whichever comes first. Use is recorded at most once per
  `min(1 minute, idle / 4)`, so an idle session ends between `idle` and
  `idle` plus that interval after its last use.
- **CSRF.** Every write the cookie authenticates must carry `X-OKF-CSRF`
  with the session's token (`GET /api/v1/session` gives it); otherwise it
  gets 403. The console does this itself.
- **One credential per request.** A request with a bearer key or token is
  that credential's, whatever cookie it carries. `/mcp` and
  `/catalogs/{name}/export|import` take only bearer credentials. The
  `/api/v1` copies of export and import take a session, at its levels.
- **What a session reaches.** An admin user's session is an admin that
  reaches every catalog, as an every-catalog admin key is. A member's
  session reaches exactly its ceiling, at the ceiling's levels. The admin
  flag and the ceiling are read from the key cache on every request.
- **No changesets.** A session never owns a changeset, so MCP writes still
  need a key or a grant.
- **Ending sessions.**
  - Sign-out (the console's Sign out, `DELETE /api/v1/session`) deletes the
    session.
  - Changing a user's admin flag signs them out of every session, and they
    sign in again with the new flag.
  - Changing your password, adding a passkey or linking an account from the
    Account page signs your other sessions out, unless "Keep me signed in
    elsewhere" is ticked.
  - A password reset signs every session of the user out.
  - The Account page signs out one session at a time, this browser's
    included.
  - An admin signs a user out everywhere with
    `okf-mcp user revoke-sessions NAME` or the user drawer's "Sign out
    everywhere", or ends one session from the drawer.

### Setting up an account, and resetting a password

No admin ever sees a password. An admin gives the user an email and sends a
setup link:

```sh
okf-mcp user set-email alice alice@example.com
okf-mcp user invite alice
```

The link (`{public_url}/account/setup?token=...`) opens a page where the
person chooses a password, registers a passkey, or both, and is signed in.
It works once, for `setup_link_ttl` (24 hours by default), and a newer setup
link replaces it. Changing the user's email ends every link they have not
used yet.

On `/signin/forgot` anyone may ask for a reset link. The answer is always
"If an account exists, we sent a link.", except a 403 for another site's
form and a 429 past the per-address limit (20 requests per hour). Each email
gets at most 3 links per hour; past that the answer is the same and nothing
is sent. A reset link works once, for
`reset_link_ttl` (1 hour by default), and opens a set-password page. The
reset signs every session of the user out, mails a "sign-in method added"
notice, signs nobody in, and sends the browser to `/signin`. An expired,
used or unknown link answers 410 "This link has expired. Ask for a new
one."

### Passkeys

Passkeys are on when `public_url`'s host is a domain name (a relying party
id cannot be an address, so a `public_url` such as `https://192.0.2.10` turns
them off, and `/auth/passkey/*` answers 404). They are registered on the
setup page or the Account page, and always need user verification (a PIN or
a biometric). The relying party is `public_url`'s host, so a passkey works
only at that host: changing it strands every registered passkey (people then
sign in another way and add a new one). Sign-in uses discoverable
credentials. The server stores public keys only, and checks and advances each
passkey's signature counter: a counter that did not move is logged at `warn`
and refused as a failed sign-in. A ceremony waits at most 5 minutes, in
memory, keyed by the `okf_passkey` cookie (`SameSite=Strict`), so a restart
ends the ones in progress.

### OIDC providers

A provider is an `[[auth.oidc]]` table in the config file, with its client
secret in an environment variable. This example uses a placeholder provider
at `https://id.example.com`:

```toml
[[auth.oidc]]
id = "example"                          # stable: stored with each linked identity
label = "Example ID"                    # the button text
issuer = "https://id.example.com"
client_id = "okf-console"
client_secret_env = "OKF_OIDC_EXAMPLE_SECRET"
scopes = ["openid", "email", "profile"]
```

1. Register a confidential web client at the provider, with the redirect
   URI `{public_url}/auth/oidc/<id>/callback` (here
   `https://okf-mcp.example.com/auth/oidc/example/callback`). Note its
   client id and secret.
2. Add the table above, and put the secret in the server's environment as
   `OKF_OIDC_EXAMPLE_SECRET` (a Kubernetes secret, as for `DATABASE_URL`).
3. Restart the server. `/signin` shows "Example ID", and the pod needs
   outbound HTTPS to the issuer.

The flow is the authorization code flow with PKCE (S256), `state` and
`nonce`, bound to the browser by a 10-minute, single-use `okf_oidc` cookie.
The provider's discovery document (`<issuer>/.well-known/openid-configuration`)
is cached for an hour; a failed refresh keeps the cached one and is tried
again after a minute, and with nothing cached the sign-in answers 503. The
`/auth/oidc/*` routes exist only while a provider is configured; an unknown
provider is 404.

After the ID token checks out (issuer, audience, nonce, expiry):

1. an identity already linked (by provider and subject) signs its user in;
2. otherwise, if the token's email is verified and matches a user's email
   (in any letter case), the identity is linked, the user signs in, and they
   get a "sign-in method added" email;
3. otherwise sign-in is refused (403) with "No account for this email. Ask
   an admin to invite you." There is no self-signup.

A bad `state`, nonce or token is 400 "Sign-in failed; try again". A
signed-in person can also link a provider from the Account page, whatever
the provider's email says: the callback links it only for the browser still
holding the session that started the link, and returns to `/ui/#/account`.
Each user may start 10 links in a burst, then one a minute. A user links
at most one account per provider, and an account linked to another user
cannot be linked (409). Unlinking is refused for the last sign-in method
(409). The Settings page lists the providers (id, label, issuer), never a
secret. Changing a provider's `id` orphans its linked identities.

### Mail

Mail goes through `[mail]`'s provider:

- `smtp`: any relay, over STARTTLS or TLS from the first byte;
- `postmark`: Postmark's HTTP API;
- `log`: **for development only.** It logs each whole message at `info`,
  recipient, subject and body, setup and reset links included, and
  delivers nothing. Anyone who can read the log can use those links. Never
  run it in production; it is the default only so that a server without
  `[mail]` starts.

The templates are a setup link, a reset link, "a sign-in method was added
to your account" (after a password change or reset, a new passkey or a
linked identity) and the Settings page's test email.

A request never sends mail itself. It queues a `SendMail` job in the
server's own Postgres database (the `apalis` schema, section 4); a setup
link and a "method added" notice are queued in the same transaction as the
change they report. A worker in every `serve` process
sends it, on a pool of its own (`mail.worker`); `user invite` and `mail test`
on the command line only queue. Two workers never send the same mail.

- A provider error that may pass is retried: a network error, Postmark 401,
  429 or 5xx, an SMTP 4xx. The first retry is 30 seconds later, doubling up
  to an hour, for 24 hours from when the mail was queued.
- A refusal fails the mail at once: an SMTP 5xx, or any other Postmark
  error (422 among them).
- Failed mail keeps its last error. Sent mail is deleted after 7 days and
  failed mail after 30, by the server's hourly prune.
- A job holds the rendered message, link included, until it is pruned: the
  queue listings never show a body.

```sh
okf-mcp mail test alice@example.com    # queues the test email
okf-mcp mail queue                     # id, status, attempts, to, template, run_at, error
okf-mcp mail retry ID                  # failed mail: send again now, with a new 24-hour window
okf-mcp mail delete ID
```

The queue lists every mail not yet sent, as `pending` (waiting for a
worker), `sending` (a worker holds it), `retrying` (failed, with tries
left) or `failed` (refused, or out of time). Retry takes `retrying` and
`failed` mail; Delete takes anything but `sending`. The Settings page shows
the same queue with Retry and Delete. A setup or reset link in a deleted
mail stays valid until it expires; send a new one to replace it.

### Settings

Policy and branding settings live in the database, so the Settings page
changes them without a restart; the config file's `[auth]` table gives each
policy setting's default. The two branding settings and
`audit_retention_days` have no config key: their defaults are `okf`, `none`
and `400`.

| Key | Default | Meaning |
|---|---|---|
| `password_sign_in` | `true` | Offer email and password sign-in |
| `max_keys_per_user` | `20` | Live self-issued keys per user, `0` to `1000` |
| `max_key_lifetime` | `none` | Longest self-issued key expiry; `none` means no limit |
| `console_session_idle` | `12h` | Console session idle timeout |
| `console_session_absolute` | `7d` | Console session absolute timeout |
| `setup_link_ttl` | `24h` | Setup link lifetime |
| `reset_link_ttl` | `1h` | Password reset link lifetime |
| `product_name` | `okf` | The product name people see: in every email (subjects and text), the console (sidebar, login card and tab title) and the sign-in, forgot, setup, reset and OAuth consent pages. 1 to 60 characters with no control characters; surrounding spaces are trimmed |
| `support_contact` | `none` | An email address or an `https://` URL (with a host). When set, every email ends with "Questions? Contact ..." and the server pages show the same line (a URL as a link); `none` shows no line |
| `audit_retention_days` | `400` | How many days [audit log](#audit-log) entries are kept, `30` to `3650`. A daily pass deletes older ones |

```sh
okf-mcp settings list                      # key, value, then "default" or "set by WHO at T"
okf-mcp settings set max_key_lifetime 90d
okf-mcp settings reset max_key_lifetime    # back to the config file's default
okf-mcp settings set product_name "Acme Notes"
okf-mcp settings set support_contact https://example.com/help
okf-mcp settings set audit_retention_days 730
```

A value is checked as its config key is: `true`/`false` (or `on`/`off`), a
whole number, or a duration like `12h` above zero and at most ten years
(`none` for `max_key_lifetime`); `product_name` and `support_contact` as in
the table. A bad one is refused, naming the key: 422
over REST, a non-zero exit on the command line, and an unknown key is clap's
list of the keys. Each change records who made it (`cli:<os user>` for the
command line, the admin's actor over REST and in the console), writes an
[audit log](#audit-log) entry, and reaches every server within
`poll_interval`. Setting the value already stored changes and records
nothing. A stored value this server cannot read (from
a newer version, or a hand edit) is logged at `warn` and the default used.
Mail is rendered when it is queued, so a queued mail keeps the brand it was
queued with; `mail.from` (the sender) stays configuration.

### Self-service keys and grants

A signed-in person's **My keys** page (`/ui/#/my-keys`) lists, creates,
renames and revokes the keys they made themselves. A self-issued key:

- names its catalogs, each `read` or `write`, all within the ceiling; never
  "all catalogs", never `admin`, never a maintenance reporter;
- counts against `max_keys_per_user` while it is live;
- expires no later than `max_key_lifetime` allows (and at that limit when
  no expiry is given), or never if that is `none`;
- belongs to the signed-in user, with its name as the default actor.

Creating one above the ceiling, past the key limit or beyond the lifetime
limit answers 422 naming the limit (`ceiling`, `max_keys_per_user` or
`max_key_lifetime`). An expired key stops working at once. **My grants**
(`/ui/#/my-grants`) lists and revokes the user's OAuth grants. My keys also
lists the keys an admin made for the user: those are admin-issued, so not
capped by the ceiling, and an admin manages them on the API keys page.
**Account** (`/ui/#/account`) changes the password, adds, renames and
removes passkeys, links and unlinks providers, lists and ends sessions, and
shows recent sign-ins, teams and the ceiling. It refuses to remove the last
sign-in method (409).

### The ceiling and member access

A user's **ceiling** is what their self-issued keys and grants may reach:

- the catalogs owned by every team the user belongs to, at each catalog's
  **member access** (`none`, `read` or `write`; `write` by default);
- plus the user's **entitlements**, catalogs an admin adds one at a time,
  each `read` or `write`;
- where both give a catalog, the higher level.

A self-issued key or grant reaches, on each catalog, the lower of its own
level and the ceiling's; a catalog outside the ceiling is unreachable. Every
change that can move a ceiling (team membership, a catalog's owner or member
access, entitlements, the admin flag) reaches running servers within
`poll_interval`, at once on the server that made it, so lowering a ceiling
lowers live keys and grants. An admin user's session is uncapped, but their
self-issued keys and grants are capped like anyone's: the ceiling comes from
teams and entitlements, never from the admin flag. Admin-issued keys and
grants from sign-in codes are never capped.

```sh
okf-mcp catalog set-member-access recipes --access read   # members read only
okf-mcp user entitle bob notes --access read             # one more catalog for bob
okf-mcp user unentitle bob notes
```

Only admins change member access and entitlements, and changing them needs
an admin that reaches every catalog. Team owners may later (#352): the
server's one check is `store::ceiling::may_set_member_access`.

### Rollout

Migration `0010` only adds. Until an admin acts nothing changes: every key,
grant, sign-in code and catalog works as before, every existing key is
admin-issued (uncapped), every catalog's member access is `write`, and
pasting a key into the console still works. After deploying:

1. Set `public_url` (section 3) if it is not set yet; it turns on OAuth too
   ("OAuth connectors").
2. Configure `[mail]` (until then the `log` provider writes each setup link
   to the server log, which is acceptable on a test server only) and, if
   wanted, `[[auth.oidc]]`. Put their secrets in the environment, and
   restart.
3. Give yourself an email, the admin flag and a setup link (inside the pod,
   with `kubectl exec` as in [First run](#6-first-run)):

   ```sh
   okf-mcp user set-email alice alice@example.com
   okf-mcp user set-admin alice --on
   okf-mcp user invite alice
   okf-mcp mail queue          # the setup mail, until the worker sends it
   ```

4. Open the link, choose a password or register a passkey, and you are
   signed in. Settings (`/ui/#/settings`) shows the policy, the providers
   and the mail queue; send yourself a test email from it.
5. Team catalogs start at `member_access = write`: before members start
   creating their own keys, review shared catalogs and set `read` (or
   `none`) where intended.
6. Invite the others (`user set-email`, then `user invite`, or the user
   drawer's Email and "Send setup email").

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
okf-mcp key create investigator --access read --catalogs demo --actor synthetic-investigator --user alice --maintenance-report
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
only indices, packet source ids and the check, never excerpt or source text.
The runner matches a model excerpt with whitespace tolerance: each run of ASCII
whitespace in it matches any nonempty run in the source, so a quote joined across
a hard line wrap still matches. The report stores the exact source span, so the
server's exact-substring rule above still holds.
Ambiguous/failed reservations are not refunded.
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
`okf_maintenance::rejection` warn line described above; everything else is dropped.
This removes the inherited rmcp logging hazard:
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

## Web console

The server also serves a browser console and the JSON API it runs on, on
the same port as `/mcp`:

| Route | Key | Does |
|---|---|---|
| `GET /` | none | Redirects (303) to `/ui/` |
| `GET /ui`, `GET /ui/` | none | `/ui` redirects (308) to `/ui/`, which serves the console's shell |
| `GET /ui/*` | none | The console's static files, embedded in the binary at build time; an unknown file is 404 |
| `GET /ui/manifest.json` | none | `{"pages": [...]}`: the console pages this build has |
| `GET /ui/brand.json` | none | `{"product_name", "support_contact"}` (`null` when unset): the [branding settings](#settings), which the console shows before anyone signs in |
| `/api/v1/*` | bearer key or console session | The JSON API; each endpoint checks the level it needs |
| `GET /signin`, `POST /signin` | none | The sign-in page and its password form ([Signing in and self-service](#signing-in-and-self-service)). Without `public_url` the page offers only the pasted key and the POST is 404; with `password_sign_in` off the POST is 404 |
| `GET /signin/forgot`, `POST /signin/forgot` | none | Ask for a reset link; only with `public_url` set |
| `GET /account/setup`, `POST /account/setup` | setup link (`?token=`) | Choose a password (or, with the page's script, register a passkey); signs in |
| `GET /account/reset`, `POST /account/reset` | reset link (`?token=`) | Set a new password; signs every session out |
| `POST /auth/passkey/signin/start`, `POST /auth/passkey/signin/finish` | none | Passkey sign-in (discoverable credentials); only while passkeys are on. The finish answers `{redirect}` with the session cookie, or 401 |
| `POST /auth/passkey/setup/start`, `POST /auth/passkey/setup/finish` | setup link (in the body) | Register a passkey from the setup page, use the link and sign in; 422 for a credential that does not verify |
| `GET /auth/oidc/{provider}/start`, `GET /auth/oidc/{provider}/callback` | none | OIDC sign-in, and the end of linking an account; only while a provider is configured |

The console's files hold no catalog data, so they need no key. Every
`/ui/` response carries:

- `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`
- `X-Content-Type-Options: nosniff`
- `Referrer-Policy: no-referrer`
- `Cache-Control: no-cache` on each file (the browser revalidates on every load)

Google Fonts (`fonts.googleapis.com` for the stylesheet, `fonts.gstatic.com`
for the fonts) is the only third-party origin the console loads from. Behind
a proxy that adds its own CSP, keep these sources or the fonts fall back to
the system ones.

Every `/api/v1` response carries `Cache-Control: no-store` and
`X-Content-Type-Options: nosniff`. The API uses the same bearer keys and the
same errors as the other routes: 401 without a valid key (plain text, with
`WWW-Authenticate: Bearer`), and otherwise `{"error": ..., "message": ...}`
with the statuses in the README's "Routes". An unknown `/api/v1` path is 404,
key or not.

The sign-in pages use the console's CSP (their scripts are static files
under `/ui/js/signin/`), with `Cache-Control: no-store`. Sign-out is
`DELETE /api/v1/session`, in the identity table below; there is no sign-out
page.

You sign in to the console with your account ("Sign in with your account";
see [Signing in and self-service](#signing-in-and-self-service)), or by
pasting an API key. A pasted key is kept in that browser's `localStorage` (in
memory only, where storage is blocked) until you sign out, so sign out on a
shared machine; anyone who can run script in that browser profile can read
it. An account session lives in an `HttpOnly` cookie no script can read. At
start the console asks for a session first: a live session wins over a
stored key, which is then dropped, so the console uses one credential at a
time. A refused key (revoked, say) or an ended session signs the browser out
at its next request. Signed in with an account, the console shows "Your
account" (My keys, My grants, Account), and Administer only to an admin
user; hiding it is a convenience, since the API refuses a member's session on
admin routes (403).

### API reference

The API has 88 endpoints (a method and path each). The identity endpoints
are below (3), and each area's endpoints are in its own subsection:
[API keys](#api-keys) (7),
[OAuth sign-in codes](#oauth-sign-in-codes) (4),
[Users and teams](#users-and-teams-api) (21),
[Your account](#your-account-api) (19),
[Settings and mail](#settings-and-mail-api) (7),
[Health](#health) (2), [Browse and search](#browse-and-search) (7),
[Catalogs and import/export](#catalogs-and-importexport) (9),
[Findings and runs](#findings-and-runs) (6) and [Changesets](#changesets) (3).
Each area's tests but Your account's check every endpoint against the
access matrix (no key, read, write, admin, and an admin key that does not
reach the catalog); Your account's check that every route refuses a bearer
key and acts only on the signed-in user's own rows. Every endpoint also takes a console session, at the signed-in user's levels
([Sessions](#sessions)), with `X-OKF-CSRF` on writes.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/whoami` | any | The calling credential: `key_id`, `name`, `actor`, `access`, `maintenance_report`, `all_catalogs`, `catalogs` (the loaded catalogs it reaches, sorted), `default_catalog`, `kind` (`key`, `oauth` or `session`), `client_name` (a grant's client), `user` (the user it belongs to, `null` when unassigned) and `teams` (that user's teams, sorted). A session's `key_id` is `session:<user>`, its `name` `null` and its `actor` the user's name. Never a key, token or hash |
| `GET /api/v1/session` | console session | `{user, email, admin, csrf, ceiling, expires_at}`: the signed-in user's name, email and admin flag, the CSRF token writes must send as `X-OKF-CSRF`, the ceiling (`{catalog: "read"\|"write"}`) and when the session ends at the latest. 401 without a usable session (a dead cookie is cleared); a bearer credential gets 403 |
| `DELETE /api/v1/session` | console session | Signs out: deletes the session and clears the cookie; 204. Needs `X-OKF-CSRF` (403 without); a bearer credential gets 403 |

```sh
curl -s -H "Authorization: Bearer $OKF_MCP_KEY" https://okf-mcp.example.com/api/v1/whoami
```

### API keys

The API keys page (`/ui/#/keys`) and its API manage keys without a shell on
the server, as `okf-mcp key ...` does. Both are for admin keys only: a read
or write key gets 403 from the API, and the console hides the page.

An admin key manages only the keys inside its reach. One that reaches every
catalog manages every key. One scoped to catalogs manages the keys whose
every catalog it reaches, and no all-catalogs key; any other key answers 404,
as an unknown id does. A new key or a grant may name only catalogs the
caller reaches, and `all` needs a caller that reaches every catalog. Paths
take a key's immutable id, never its name.

The secret is in the `POST /api/v1/keys` response only: it is not stored and
cannot be shown again, and no other response holds a secret or a hash. A
revoke, grant, ungrant or reporting change takes effect on the key's next
request (the server reloads its keys at once). Key changes made here record
no audit actor, as with the CLI.

Every key belongs to a user ([Users and teams](#users-and-teams)). New key
has a required user picker, the list has a User column (`unassigned` for a
key from before users existed), and each key's or grant's drawer, revoked
ones included, has Assign (unassigned) or Change user.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/keys` | admin | The keys the caller manages, revoked ones included: `id`, `name`, `actor`, `access`, `kind` (`key` or `oauth`), `client_name`, `maintenance_report`, `all_catalogs`, `catalogs`, `catalog_access` (each catalog's level), `default_catalog`, `created_at`, `revoked_at`, `expires_at` (`null`: never), `self_issued`, `user` (`null` when unassigned). Never a secret or hash |
| `POST /api/v1/keys` | admin | Body `{name, user, access: "read"\|"write"\|"admin", all, catalogs, default_catalog, actor, maintenance_report}` (`user` required; `actor` defaults to `name`). Answers `{key, secret}`. 400 for a bad `access`, 403 for `all` from a scoped caller, 404 for an unreached catalog or an unknown user, 409 for a bad or taken name, no catalogs, or `all` with `catalogs`, 422 for an unknown field or a missing `user` |
| `PATCH /api/v1/keys/{id}` | admin | Body `{maintenance_report: bool}`. 409 for a revoked key, or for enabling reporting on an OAuth grant |
| `PUT /api/v1/keys/{id}/user` | admin | Body `{user}`. Gives the key or OAuth grant (live or revoked) that user, or changes it; access is unchanged. Answers its view, and its next request runs as that user. 404 for an unmanaged key or an unknown user, 422 without `user` |
| `POST /api/v1/keys/{id}/revoke` | admin | Refused from the key's next request on. 409 if already revoked |
| `PUT /api/v1/keys/{id}/catalogs/{catalog}` | admin | Grants a catalog. 404 for an unreached catalog or an unmanaged key; 409 for a revoked or all-catalogs key |
| `DELETE /api/v1/keys/{id}/catalogs/{catalog}` | admin | Removes a grant, and the default if it was that catalog. Errors as for the grant |

```sh
curl -fsS -H "Authorization: Bearer $OKF_MCP_KEY" -H 'Content-Type: application/json' \
    -d '{"name":"ci-reader","access":"read","catalogs":["main"],"default_catalog":"main"}' \
    https://okf-mcp.example.com/api/v1/keys
```

### OAuth sign-in codes

`okf-mcp oauth ...` and `/api/v1/oauth/codes` mint and manage the one-time
codes the OAuth consent page asks for. The reach rule is the one for keys:
a scoped admin manages only codes whose every catalog it reaches, and no
all-catalogs code. The code itself is in the create response (and the CLI's
stdout) only; only its SHA-256 is stored. Codes are read from the database on
every use, so minting or revoking one takes effect at once. Grants, the keys
codes become, are listed and revoked with the other keys: their views carry
`kind: "oauth"` and `client_name`.

The API keys page (`/ui/#/keys`) has a "Sign-in codes" section below the
keys: "New sign-in code" mints one and shows it once, the list shows live
codes (tick "Show redeemed, expired and revoked codes" for the rest), and a
code's drawer revokes it until someone redeems it. In the key list, an OAuth
grant is marked `OAuth` with its client's name. Its drawer revokes it or
changes its catalogs, and has no reporting switch: a grant never reports.

The `okf-mcp oauth` commands are in "OAuth connectors"
([Managing codes and grants](#managing-codes-and-grants)), with how to turn
OAuth on and connect a client.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/oauth/codes` | admin | The codes the caller manages, newest first: `id`, `name`, `actor`, `access`, `all_catalogs`, `catalogs`, `default_catalog`, `state`, `created_at`, `expires_at`, `redeemed_at`, `grant_id`, `revoked_at`, `user` (`null` when unassigned). Never a code or hash |
| `POST /api/v1/oauth/codes` | admin | Body `{name, user, actor, access: "read"\|"write", all, catalogs, default_catalog, expires_in}` (`user` required; `actor` optional, and absent or blank means `name`; `expires_in` like `2h` or `3d`, default 24h). Answers `{code, secret}`. 400 for a bad `access` or `expires_in`, 403 for `all` from a scoped caller, 404 for an unreached catalog or an unknown user, 409 for `admin`, a bad name, no catalogs, `all` with `catalogs` or too long an expiry, 422 for an unknown field or a missing `user` |
| `DELETE /api/v1/oauth/codes/{id}` | admin | Revokes an unredeemed code; answers its view. 404 for an unmanaged code, 409 if redeemed or already revoked |
| `PUT /api/v1/oauth/codes/{id}/user` | admin | Body `{user}`. Gives the code (in any state) that user; answers its view. A grant the code already became keeps its own user (`PUT /api/v1/keys/{id}/user`). 404 for an unmanaged code or an unknown user, 422 without `user` |

```sh
curl -fsS -H "Authorization: Bearer $OKF_MCP_KEY" -H 'Content-Type: application/json' \
    -d '{"name":"laptop","user":"alice","access":"read","catalogs":["main"]}' \
    https://okf-mcp.example.com/api/v1/oauth/codes
```

### Users and teams API

The routes behind `okf-mcp user ...`, `team ...` and `unassigned`
([Users and teams](#users-and-teams) has the model and the
[parity table](#cli-api-and-console-parity)). Writes and `GET /unassigned`
need an admin key that reaches every catalog (403 for any other key), since
users and teams are not catalog-scoped; reads need any admin key (403 below
admin). Every change takes effect at once. A refusal under the
[deletion rules](#deletion-rules) is 409, and an unknown user or team 404
(`UnknownName`).

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/users` | admin | `?email=` (optional) answers only the user with that email, in any letter case: zero or one, in the same view; text that cannot be an address finds none, and an unknown parameter is 400. `{users}`, by name: `name`, `display_name`, `created_at`, `credentials` (keys, grants and codes naming it, whoever manages them), `teams`, `email` (`null` when unset), `is_admin`, `has_password` (never the password or its hash) |
| `POST /api/v1/users` | admin, every catalog | Body `{name, display_name}` (`display_name` optional). Creates the user and its personal team; 201 with the user. 409 for an invalid or taken name, 422 without `name` |
| `GET /api/v1/users/{name}` | admin | `{user, keys, codes, entitlements}`: the user, the keys, grants and codes naming it that the caller manages, and its entitlements (`[{catalog, access, created_at}]`, by catalog) |
| `DELETE /api/v1/users/{name}` | admin, every catalog | Deletes the user with its personal team; 204. 409 while something refers to it |
| `PUT /api/v1/users/{name}/admin` | admin, every catalog | Body `{admin: bool}`. Answers `{name, previous, admin}`. A change signs the user out of every console session (they sign in again with the new flag). 409 for clearing the last admin user (only the CLI may), 422 without `admin` |
| `PUT /api/v1/users/{name}/email` | admin, every catalog | Body `{email}` (`null` clears; the field is required). Answers the user. A change ends the user's outstanding emailed links. 409 "Another user has this email" (in any letter case), 422 for an address that is not one or without `email` |
| `POST /api/v1/users/{name}/invite` | admin, every catalog | Queues a setup email with a single-use link (replacing any older one) to the user's email; 202 `{name, email}`. 409 without an email or without `public_url` |
| `PUT /api/v1/users/{name}/entitlements/{catalog}` | admin, every catalog | Body `{access: "read"\|"write"}`. Adds the entitlement or changes its level; answers `{name, entitlements}`. 400 for another level, 404 for an unknown user or catalog |
| `DELETE /api/v1/users/{name}/entitlements/{catalog}` | admin, every catalog | Removes it (none given: no change); answers `{name, entitlements}` |
| `GET /api/v1/users/{name}/sessions` | admin | `{sessions}`, most recently used first: `id_prefix`, `created_at`, `last_seen_at`, `ip`, `user_agent`, `current` (always `false` here). Never a session id, token or hash |
| `DELETE /api/v1/users/{name}/sessions` | admin, every catalog | Signs the user out of every console session (keys and grants keep working); answers `{name, revoked}` |
| `DELETE /api/v1/users/{name}/sessions/{id_prefix}` | admin, every catalog | Signs one session out; 204. 404 for a malformed prefix or one that matches none of the user's sessions, 409 for one that matches several (none is ended) |
| `GET /api/v1/users/{name}/sign-ins` | admin | `{sign_ins}`, newest first (at most 50): `method`, `outcome`, `ip`, `at` |
| `GET /api/v1/teams` | admin | `{teams}`, by name: `name`, `display_name`, `personal_of` (the user whose personal team it is, or `null`), `created_at`, `members` (`[{user, role}]`, by user), `catalogs` (the names it owns), `suspended_at` (when it was suspended, `null` while active) and `suspension_notice` (`null` when there is none; see [Suspending a team](#suspending-a-team)) |
| `POST /api/v1/teams` | admin, every catalog | Body `{name, display_name, owner}` (`owner` required: the first owner's user name). 201 with the team. 404 for an unknown owner, 409 for an invalid or taken name, 422 without `owner` |
| `GET /api/v1/teams/{name}` | admin | The team, as in the list |
| `DELETE /api/v1/teams/{name}` | admin, every catalog | Deletes a shared team that owns no catalog; 204. 409 for a personal team or one that owns a catalog |
| `PUT /api/v1/teams/{name}/members/{user}` | admin, every catalog | Body `{role: "owner"\|"member"}`. Adds the user, or gives a member that role; answers `{change: "added"\|"role_changed"\|"unchanged", team}`. 409 for demoting the last owner, 422 for a missing or unknown `role` |
| `DELETE /api/v1/teams/{name}/members/{user}` | admin, every catalog | Removes a member; answers the team. 409 for a non-member, the last owner, or a personal team's user |
| `PUT /api/v1/teams/{name}/suspension` | admin, every catalog | Body `{suspended: bool, notice?}`. `true` suspends the team, or replaces the notice of a suspended one (its `suspended_at` is kept); `false` lifts it (no change when active). Answers the team. Its catalogs then refuse every non-admin credential with 403 `Suspended`, except export. 404 for an unknown or malformed team name, 409 for a notice over 2000 characters or holding a NUL, 422 without `suspended` or with an unknown field |
| `GET /api/v1/unassigned` | admin, every catalog | `{keys, codes, catalogs}`: the keys and grants (live and revoked) and codes with no user, as in their own lists, and the names of the catalogs with no owner |

```sh
curl -fsS -H "Authorization: Bearer $OKF_MCP_KEY" -H 'Content-Type: application/json' \
    -d '{"name":"family","owner":"alice"}' \
    https://okf-mcp.example.com/api/v1/teams
```

### Your account API

The routes behind My keys, My grants and Account, for a person signed in
with an account (a console session). A bearer key or OAuth token gets 403
from every one but `GET /api/v1/me`, since a key must not mint keys for
itself. Each acts only on the signed-in user's own keys, grants, passkeys,
identities and sessions: anyone else's id answers 404, as an unknown one
does. Limits are in [Signing in and self-service](#signing-in-and-self-service).

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/me` | console session, or a key that belongs to a user | `{user, display_name, email, admin, has_password, teams, ceiling, session, password_sign_in, limits}`: `ceiling` is `[{catalog, access}]`, `session` says whether a console session asked, and `limits` is `{max_keys_per_user, max_key_lifetime_secs}` (`null` for no lifetime limit). 404 for a key without a user |
| `GET /api/v1/me/keys` | console session | `{keys}`: the user's keys, revoked and expired ones included, as `GET /api/v1/keys` shows them (with `catalog_access`, `expires_at`, `self_issued`) |
| `POST /api/v1/me/keys` | console session | Body `{name, catalogs: [{catalog, access}], default_catalog, actor, expires_at}` (`actor` defaults to `name`; `expires_at` RFC 3339, and absent means the lifetime limit, or none). Answers `{key, secret}`, the secret once. 400 for no catalogs, a repeated catalog, an unknown level, or a malformed or past expiry; 409 for a bad or taken name or a default that is not one of `catalogs`; 422 above the ceiling (`admin` included), past the key limit or the lifetime limit (`limit` is `ceiling`, `max_keys_per_user` or `max_key_lifetime`), or for an unknown field such as `all`, `user` or `maintenance_report` |
| `PATCH /api/v1/me/keys/{id}` | console session | Body `{name}`. Renames a live key; answers its view. 409 for a bad or taken name or a revoked key |
| `DELETE /api/v1/me/keys/{id}` | console session | Revokes it (the row stays, revoked); answers its view. 409 if already revoked |
| `GET /api/v1/me/grants` | console session | `{grants}`: the user's OAuth grants, revoked ones included, as key views (`client_name`, `catalog_access`, `self_issued`, `revoked_at`, ...) |
| `DELETE /api/v1/me/grants/{id}` | console session | Revokes the grant and every token it holds; its client is refused from its next request. Answers its view; 409 if already revoked |
| `PUT /api/v1/me/password` | console session | Body `{current_password, new_password, keep_sessions}` (`current_password` required while the user has a password). 204. 403 for a wrong current password, a locked account or a password changed meanwhile (never 401, which the console would read as signed out); 401 once this session has ended; 422 for the policy (`limit: "password"`). Signs the other sessions out unless `keep_sessions`, and queues the notice, in the same transaction |
| `GET /api/v1/me/passkeys` | console session | `{passkeys}`, oldest first: `id` (unpadded base64url), `name`, `created_at`, `last_used_at` |
| `POST /api/v1/me/passkeys/register/start` | console session | Body `{name, keep_sessions}`. The WebAuthn creation options; the ceremony is bound to a 5-minute `okf_passkey` cookie and to this session. 404 while passkeys are off, 422 for a bad name |
| `POST /api/v1/me/passkeys/register/finish` | console session | Body: the browser's credential. Stores the passkey; 201 `{id, name}`. Signs the other sessions out unless the start said `keep_sessions`. 403 for another session's ceremony, 409 for a passkey already registered, 410 for an expired ceremony, 422 for a credential that does not verify (`limit: "passkey"`), 401 only once the session has ended |
| `PATCH /api/v1/me/passkeys/{id}` | console session | Body `{name}`. Renames it; 204. 422 for an empty, over-64-character or control-character name (`limit: "passkey_name"`) |
| `DELETE /api/v1/me/passkeys/{id}` | console session | Removes it; 204. 409 for the last sign-in method |
| `GET /api/v1/me/identities` | console session | `{identities, providers}`: the linked identities (`provider`, `email_at_link`, `created_at`, `last_used_at`; never the subject) and the configured providers (`id`, `label`) |
| `POST /api/v1/me/identities/{provider}/link` | console session | Body `{keep_sessions}`. Starts linking: answers `{url}`, the provider's sign-in URL (the flow cookie is set); the callback links the identity and returns to `/ui/#/account`. 404 for an unknown provider or with no providers, 429 past the per-user allowance, 503 when the provider cannot be reached |
| `DELETE /api/v1/me/identities/{provider}` | console session | Unlinks it; 204. 404 when none is linked there, 409 for the last sign-in method |
| `GET /api/v1/me/sessions` | console session | `{sessions}`, most recently used first: `id_prefix` (12 hex characters, never the id), `created_at`, `last_seen_at`, `ip`, `user_agent`, `current` (this browser's) |
| `DELETE /api/v1/me/sessions/{id_prefix}` | console session | Signs one session out, this browser's included; 204. 404 for a malformed prefix or one none of the user's sessions has; 409 for one several share (none is ended) |
| `GET /api/v1/me/sign-ins` | console session | `{sign_ins}`, newest first (at most 50): `method`, `outcome`, `ip`, `at` |

### Settings and mail API

The routes behind `okf-mcp settings ...` and `okf-mcp mail ...`. Reads need
any admin key; changes need an admin key that reaches every catalog (403 for
any other key). An admin user's console session counts as an admin that
reaches every catalog.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/settings` | admin | `{settings, oidc, mail}`: each setting's `key`, `value` (the effective value, as text), `overridden`, `updated_at`, `updated_by`; the OIDC providers' `id`, `label`, `issuer`; the mail `provider` and `from`. Never a secret |
| `PUT /api/v1/settings/{key}` | admin, every catalog | Body `{value}` (a string, as on the command line). Answers the setting. 404 for an unknown key, 422 for a value the setting refuses or without `value` |
| `DELETE /api/v1/settings/{key}` | admin, every catalog | Drops the override; answers the setting at its default. 404 for an unknown key |
| `POST /api/v1/mail/test` | admin, every catalog | Body `{to}`. Queues the test email; 202 `{to}`. 422 for an address that is not one |
| `GET /api/v1/mail/queue` | admin | `{mail}`: every mail not yet sent, oldest due first: `id`, `to`, `template`, `status` (`pending`, `sending`, `retrying` or `failed`), `attempts`, `last_error`, `run_at`. Never a body |
| `POST /api/v1/mail/queue/{id}/retry` | admin, every catalog | Sends failed mail again now, with a new 24-hour retry window; 204. 404 for an unknown id, 409 for mail that has not failed |
| `DELETE /api/v1/mail/queue/{id}` | admin, every catalog | Deletes mail that is not being sent right now; 204. 404 for an unknown id, 409 while a worker is sending it |

### Health

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/catalogs/{catalog}/health` | read | `{catalog, lint_errors, lint_warnings, sprawl, orphans, broken_links, oldest, open_questions}`. `sprawl` counts `lint`'s `sprawl` warnings; `oldest` is the stalest concept's `oldest` row (its newest `generated`, `verified` or `reviewed` stamp; `null` without concepts); links are counted without mentions |
| `GET /api/v1/catalogs/{catalog}/reports/{report}` | read | `report` is `lint`, `sprawl`, `links`, `oldest` or `questions`; the body is that tool's result unchanged. `?n=` for `oldest` and `sprawl` (default 20, clamped to 1..=200; the MCP tools default to 5), `?mentions=false` for `links`. Another name is 404 `NotFound` |

A catalog the key does not reach, or that does not exist, is 404 on both.

```sh
curl -s -H "Authorization: Bearer $OKF_MCP_KEY" \
    "https://okf-mcp.example.com/api/v1/catalogs/main/reports/oldest?n=10"
```

The Health page is the console's landing page. Its five tiles are these
counts for the selected catalog (lint errors with the warning count, sprawling
concepts, orphans with the broken-link count, the oldest stamp, open
questions). The switcher below them shows one report at a time as a sortable
table, and a row opens its concept in Browse. Refresh reloads the counts and
the reports.

### Browse and search

Browse (`#/browse/<catalog>/<path>`) shows a catalog's tree beside one
document, with tabs Content, Links, History and Raw. A history row opens
the revision's diff in a drawer. Choosing another catalog in the switcher
while browsing opens that catalog's root `index.md`. Search
(`#/search/<query>`) searches every catalog the key reaches.

Concepts are rendered to HTML on the server (`src/render.rs`):

- raw HTML in a document is shown as text, never run;
- links may use only `http`, `https` and `mailto`, or point at a document
  of the same catalog, which opens in the console; every other link
  (`javascript:`, `data:`, a path leaving the catalog, a link to a file
  that is not markdown) is shown as plain text;
- an outside image is shown as a link to it, because the console's CSP
  loads no outside images;
- a link to a missing document is struck through.

A body over `concept_outline_chars` is shown as its outline until you
choose "Load the whole body" or follow a section link, which loads the
whole body and scrolls to that heading.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/catalogs/{catalog}/tree` | read | Every concept, `index.md` and `log.md`, and the root `AGENTS.md`: `path`, `kind`, `title` |
| `GET /api/v1/catalogs/{catalog}/concepts/{path}?full=` | read | `get_concept` with `raw`, plus `body_html` and the whole `outline`. Over `concept_outline_chars` without `full=true`: no `body`, `raw` or `body_html` |
| `GET /api/v1/catalogs/{catalog}/links/{path}` | read | Outbound `links` and `backlinks`, as `get_concept` with `links` |
| `GET /api/v1/catalogs/{catalog}/history/{path}?limit=` | read | The `history` tool |
| `GET /api/v1/catalogs/{catalog}/revisions/{n}/{path}` | read | `text` at revision `n` (null for a deletion or non-text bytes) and a unified `diff` from revision `n-1` |
| `GET /api/v1/catalogs/{catalog}/documents/{path}` | read | `read_document` (any `index.md` or `log.md`, the root `AGENTS.md`) plus `body_html` |
| `GET /api/v1/search?q=&catalogs=&limit=` | read | The `search` tool. `catalogs` is comma-separated and defaults to every catalog the key reaches; a named catalog it does not reach is a 404 |

A catalog the key does not reach, or that does not exist, is 404 on all
seven.

The per-document routes put their kind before the path (`/links/{path}`,
not `/concepts/{path}/links`), because the path is a wildcard and must come
last. For example:

```sh
curl -s -H "Authorization: Bearer $OKF_MCP_KEY" \
    https://okf-mcp.example.com/api/v1/catalogs/main/revisions/2/systems/alpha.md
```

### Catalogs and import/export

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/catalogs` | any | The catalogs the key reaches, by name: `name`, `description`, `owner_team` (`null` when unassigned), `files`, `concepts`, `generation` (of the loaded snapshot, what `expect_generation` compares against), `last_change` (`null` when empty) and `suspended` (`{team, notice}` while its owner team is suspended, otherwise `null`: a suspended catalog is listed, never hidden; see [Suspending a team](#suspending-a-team)) |
| `POST /api/v1/catalogs` | admin, every catalog | `{name, description, owner}` (`description` optional; `owner` required, a team's name). 201 with the new catalog's summary; 403 for a key granted named catalogs, since it would not reach the new one; 404 for an unknown owner; 409 for an invalid or taken name; 422 without `owner` |
| `PATCH /api/v1/catalogs/{catalog}` | admin | `{description}`. Answers `{name, previous, description}` |
| `PUT /api/v1/catalogs/{catalog}/owner` | admin, every catalog | `{team}`. Makes the team the owner; answers `{name, previous, owner_team}` (`previous` is `null` for a legacy catalog). 404 for an unknown catalog or team, 422 without `team`. Ownership grants no access |
| `GET /api/v1/catalogs/{catalog}/member-access` | admin | `{name, member_access}` (`none`, `read` or `write`): what members of the owner team reach through their own keys and grants |
| `PUT /api/v1/catalogs/{catalog}/member-access` | admin, every catalog | Body `{access: "none"\|"read"\|"write"}`. Sets it, and every member's own keys and grants are capped again at once; answers `{name, previous, member_access}`. 400 for another value, 404 for an unknown catalog, 422 without `access` |
| `DELETE /api/v1/catalogs/{catalog}[?force=true]` | admin | Deletes the catalog with its documents, history, changesets and grants; answers `{name}`. 409 while an open changeset holds unpublished changes, unless `force=true` |
| `GET /api/v1/catalogs/{catalog}/export` | read | The same handler and 120 s timeout as `/catalogs/{name}/export` |
| `POST /api/v1/catalogs/{catalog}/import[?replace=true&only_machinery=true&expect_generation=N]` | admin | The same handler, body cap and upload deadline as `/catalogs/{name}/import` |

A catalog the key does not reach, or that does not exist, is 404 on every
route but the list, the create, the owner change and the member-access
change (which reach every catalog).

- Catalog changes take effect at once: the handler polls after each one, so
  the next request (and the console's catalog switcher) sees it.
- Catalog rows record no actor, as with `okf-mcp catalog`; the server's log
  line for each change names the key id and its actor.
- `only_machinery=true` is `--only-machinery`: only the archive's machinery
  is compared and written. On a non-empty catalog it needs `replace=true`,
  as on the command line.
- A stale `expect_generation` is 409, with a message that says to export
  again; nothing is imported. Both import paths take these options.

```sh
curl -s -H "Authorization: Bearer $OKF_MCP_KEY" https://okf-mcp.example.com/api/v1/catalogs
```

The Catalogs page lists the catalogs with their owner team and counts. An
admin opens a row to change its description or delete it (typing its name to
confirm, with a force box for unpublished changes); an admin key that reaches
every catalog also gets New catalog (with a required owner team) and the
drawer's Change owner (Assign for an unassigned catalog). The Import & export page exports the selected catalog
as a `.tar.gz` download, and, for an admin, uploads one with the import
options, a progress bar and the import report (or the refusal and its
problems). It pre-fills the expected generation from `GET /api/v1/catalogs`,
so an import is refused if the catalog moved since the page loaded; clear the
field to skip the check.

### Findings and runs

The Findings page (`/ui/#/findings`) lists the selected catalog's
maintenance findings, open ones first, with a filter for each other
disposition and one for all. A finding's drawer shows its claim, its evidence
at the pinned revisions and its decision history, and a write key records a
decision there with a rationale; the console makes the retry key. A decision
is recorded only: it never edits the catalog. The Runs page (`/ui/#/runs`)
lists runs newest first, and a run's drawer lists its work items with links
to their concepts and findings. The nav badge shows the selected catalog's
open findings.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/catalogs/{catalog}/maintenance/runs?after=&limit=` | read | As `maintenance_list_runs`: oldest first; `limit` 1..=200 (default 20; outside is 409); `after` is the previous page's `next_after` (an unknown one is 404) |
| `GET /api/v1/catalogs/{catalog}/maintenance/runs/{run}` | read | As `maintenance_get_run`: the run summary and up to 50 work items, with their finding ids |
| `GET /api/v1/catalogs/{catalog}/maintenance/findings?after=&limit=&disposition=` | read | As `maintenance_list_findings`, optionally narrowed to one current disposition (`open`, `accepted_for_follow_up`, `dismissed`, `needs_input`; another value is 400). `limit` and `next_after` count matching findings only |
| `GET /api/v1/catalogs/{catalog}/maintenance/findings/{finding}` | read | As `maintenance_get_finding`: claim, evidence with pinned revisions, decision history, `outdated` |
| `GET /api/v1/catalogs/{catalog}/maintenance/counts` | read | `{catalog, open, accepted_for_follow_up, dismissed, needs_input}` by current disposition (the nav badge) |
| `POST /api/v1/catalogs/{catalog}/maintenance/findings/{finding}/disposition` | write | Body `{disposition, rationale, idempotency_key}` (an unknown field is 422). As `maintenance_dispose`: appends a decision and never edits the catalog. A retry with the same key and body returns the original response; the same key with a different body is 409. The reporting flag grants nothing here: a read-level reporter key gets 403 |

A catalog the key does not reach, or that does not exist, is 404 on all six.

```sh
curl -fsS -H "Authorization: Bearer $OKF_MCP_KEY" -H 'Content-Type: application/json' \
    -d '{"disposition":"dismissed","rationale":"Covered elsewhere.","idempotency_key":"dismiss-1"}' \
    https://okf-mcp.example.com/api/v1/catalogs/main/maintenance/findings/FINDING_ID/disposition
```

### Changesets

The Changesets page (`/ui/#/changesets`) lists the selected catalog's open
changesets: a key sees its own, an admin every key's. It marks the idle ones
(no change within `session_idle_timeout`), and a row opens the changeset's
pending diff in a drawer, with Discard. These are the `list_changesets`,
`pending_changes` and `discard` tools over HTTP, with the same rules: the
diff is for the key that opened the changeset or an admin (a read key opens
none, so it sees none), and another key's handle answers 404, as an unknown
one does, never 403.

| Method and path | Key | Does |
|---|---|---|
| `GET /api/v1/changesets?catalog=` | read | The key's own open changesets; an admin's: every key's. `catalog` is optional (absent or empty: every catalog the key reaches; an unreached one is 404). As `list_changesets`: `changeset`, `catalog`, `key`, `key_name`, `actor`, `created`, `last_change`, `idle`, `paths` |
| `GET /api/v1/changesets/{handle}/diff` | owner or admin | As `pending_changes`: `{catalog, changeset, files}`, each file with its unified `diff`. 404 for another key's handle; 409 once published or discarded |
| `DELETE /api/v1/changesets/{handle}` | write (owner) or admin | As `discard` without `paths`: drops every entry and closes the handle for good (`{catalog, changeset, removed, closed}`). An admin closing another key's changeset is recorded in the [audit log](#audit-log). 404 for another key's handle; 409 once published or discarded |

```sh
curl -s -X DELETE -H "Authorization: Bearer $OKF_MCP_KEY" \
    https://okf-mcp.example.com/api/v1/changesets/$HANDLE
```

### Testing the console

`just test-ui` runs the browser journeys (`tests/ui_e2e`) against a real
server in headless Chromium, driven by `rustwright`. They need Docker (for
the throwaway Postgres) and Chromium. `rustwright` finds Chromium through
`RUSTWRIGHT_CHROMIUM` (the binary's path), `CHROME`, `CHROMIUM`, the
Playwright cache, then `PATH`; without one, each journey prints a skip line
and passes. A journey fails on any script error, unhandled rejection or CSP
violation in the page. `tests/ui_e2e/smoke.rs` visits every page as an admin;
each page also has its own journeys.

On some distributions `chromium-browser` on `PATH` is a stub that installs a
snap package and cannot launch headless. If journeys fail at launch, leave
`RUSTWRIGHT_CHROMIUM` unset and install a Playwright Chromium (the
Playwright cache comes before `PATH`), or point the variable at a real
Chromium binary, never at the stub.

## OAuth connectors

Some MCP clients can only sign in with OAuth: ChatGPT's custom connectors,
Claude.ai's custom connectors, and the Claude and ChatGPT connector
directories. For them the server is also an OAuth 2.1 authorization server,
following the MCP authorization specification (2026-07-28). Clients that send
an API key (`Authorization: Bearer okf_...`) are not affected: keys and OAuth
work side by side on the same `/mcp` URL.

A person connecting a client signs in on the consent page with their
account: `/oauth/authorize` sends them to `/signin` first, after checking the
client and its redirect URI. They tick catalogs, choose `read` or `write` for
each within their ceiling, pick a default and approve. The grant is
**self-issued**: it belongs to them, its actor is the client's name, and it
is capped at their ceiling and lowered when the ceiling is
([Signing in and self-service](#signing-in-and-self-service)). The approval
re-reads the ceiling, so a level lowered while the page was open is
refused, and each page approves at most once.

A redirect URI must match one the client registered exactly, with one
exception: a loopback `http` URI (`localhost`, `127.0.0.0/8` or `[::1]`)
matches on any port (RFC 8252 section 7.3), since native clients such as
Claude Code register `http://localhost/callback` and listen on a port the
operating system picks. Scheme, host as written, path and query must still
match, and the token request must repeat the port the authorization used.

Alternatively ("Use a sign-in code instead" on the consent page) an admin
mints a **sign-in code** that says what the connection may do: its
catalogs, `read` or `write` access, the default catalog, and the actor its
writes are recorded as. The person connecting types the code into the
server's consent page. Redeeming a code creates an **OAuth grant**, which
works like an API key with exactly the code's access: it is listed with the
keys and revoked like one. A grant from a sign-in code is admin-issued: it
keeps the code's access, uncapped, as before. A code works once, and expires
unused after 24 hours unless it was minted with another expiry.

OAuth grants are never `admin` and never submit maintenance reports (the
database refuses both). The console and the maintenance runner keep using API
keys.

### Turning it on

1. Set `public_url` to the origin clients reach the server at, and make sure
   its host is in `allowed_hosts` (section 3):

   ```toml
   allowed_hosts = ["okf-mcp.example.com"]
   public_url = "https://okf-mcp.example.com"
   ```

   The server refuses to start if the host is missing from `allowed_hosts`:
   the MCP `Host` check would otherwise refuse every OAuth client.

2. Restart the server. These now answer, with no key:

   ```sh
   curl -s https://okf-mcp.example.com/.well-known/oauth-protected-resource
   curl -s https://okf-mcp.example.com/.well-known/oauth-authorization-server
   ```

   The first names the resource, `https://okf-mcp.example.com/mcp`, and its
   authorization server, `https://okf-mcp.example.com`. A request to `/mcp`
   without a token now gets a 401 whose `WWW-Authenticate` header points
   clients at that document
   (`Bearer resource_metadata="https://okf-mcp.example.com/.well-known/oauth-protected-resource"`).

**Routing.** The example ingress (section 5) routes every path, so it needs
no change. A proxy that routes only some paths must also pass
`/.well-known/oauth-protected-resource` (and the same path with `/mcp`
appended), `/.well-known/oauth-authorization-server`, `/oauth/` and `/ui/`
(the consent page's stylesheets), and must let the server's own
`Content-Security-Policy` through on `/oauth/`.

**Egress.** A client that identifies itself by a Client ID Metadata Document
(an `https://` `client_id`) makes the server fetch that document from the
client's host over HTTPS. The fetch ignores `HTTP_PROXY` and the other proxy
variables, so a network policy that blocks the pod's direct outbound HTTPS
breaks such clients. Clients that register themselves (dynamic client
registration) need no egress.

**State.** The server stays single-replica. Consent pages in progress and
authorization codes not yet exchanged live in memory, so a restart interrupts
at most a sign-in in progress. The person starts again from the client, with
the same code if they had not yet chosen Approve (a code is spent only by
Approve). Grants, tokens, codes and clients are in the database.

### Connecting a client

The steps are the same for ChatGPT, Claude.ai and any other client that
supports remote MCP servers with OAuth. The client's own screens differ and
change over time, so they are described generically here.

A person with an account skips step 1: in step 3 they sign in (if their
browser has no console session yet), choose catalogs and levels within their
ceiling, and approve. The steps below are the sign-in code path, for anyone
without an account.

1. **Mint a sign-in code** for the connection, on the server:

   ```sh
   kubectl -n okf-mcp exec deploy/okf-mcp -- \
       okf-mcp oauth code create chatgpt --actor example-actor \
       --access write --catalogs notes --default notes --user alice
   ```

   The command prints the code (`okfsc_...`) once on stdout, and its id and
   expiry on stderr. Only its SHA-256 is stored. Give it to the person
   connecting over a private channel. An admin can also mint one in the web
   console: API keys page, "Sign-in codes", "New sign-in code".

2. **Add the connector** in the client (usually in its settings, where custom
   connectors or remote MCP servers are added). Give it the URL
   `https://okf-mcp.example.com/mcp`, that is `<public_url>/mcp`, and choose
   OAuth if the client asks how to authenticate. Leave any client ID and
   client secret fields empty: the server accepts clients that identify
   themselves by a Client ID Metadata Document and clients that register
   themselves, and needs no secret from either.

3. **Sign in.** The client opens the server's consent page in a browser. It
   names the client and the host it will send the browser back to. A client
   that registered itself, rather than publishing a metadata document, is
   marked "unverified": its name is whatever it said. Enter the sign-in code
   and choose Continue. The next page states what will be granted (access,
   catalogs, actor and default catalog). Choose Approve, and the browser goes
   back to the client, which is now connected. Deny sends the client an
   `access_denied` error and leaves the code unused.

The sign-in must finish within 10 minutes of the client opening the page, and
five wrong codes end it; either way the client has to start again. The page
gives the same message for a mistyped, expired, revoked or already used code,
on purpose.

The client renews its access itself: access tokens last
`oauth.access_token_ttl` and are renewed with a refresh token, which lasts
`oauth.refresh_token_ttl` from its last use. A connection used at least that
often stays connected.

Each connection needs its own code. Connecting ChatGPT and Claude.ai, two
people, or the same client again after a revoke, takes a new code each time.

### Managing codes and grants

| Command | Does |
|---|---|
| `okf-mcp oauth code create NAME --user USER [--actor ACTOR] --access read\|write (--catalogs C,C... \| --all) [--default C] [--expires 24h]` | Mints a single-use code. Prints the `okfsc_` code once (stdout) and its id and expiry (stderr). `NAME` is a label in `[A-Za-z0-9._-]+`; `--user` names the user the code and its grant belong to (it must exist); `--actor` defaults to `NAME`; `--catalogs` is comma-separated, as for `key create`; `--default` must be granted. `--access admin` is refused. `--expires` (`24h`, `3d`) defaults to 24 hours and may not exceed `oauth.code_max_expiry` |
| `okf-mcp oauth code list` | Every code, never the code itself. Tab-separated: name, actor, id, access, state (`live`, `expired`, `redeemed` or `revoked`), catalogs (`*` for all), `default D`, `expires T`, `grant G` (the grant a redeemed code became), `user U`; an empty cell is `-` |
| `okf-mcp oauth code revoke ID` | Revokes a code nobody has redeemed. Refuses a redeemed or already revoked one |
| `okf-mcp oauth code assign ID --user USER` | Gives a code, in any state, a user. The grant a redeemed code became keeps its own user: assign that with `okf-mcp key assign` ([Users and teams](#users-and-teams)) |
| `okf-mcp oauth grant list` | The OAuth grants, revoked ones included, with `key list`'s columns (ending in `user U`) and `client <name>` |
| `okf-mcp oauth grant revoke ID` | Revokes a grant by its exact id, and with it every token it holds. Refuses a plain key's id |

Each takes `--database-url` and `--config` like every subcommand; inside the
pod, run them with `kubectl exec` as in step 1.

A grant is named `<code name>.<client host>-<first 8 characters of the code's
id>`, so a code named `chatgpt` redeemed by a client at `client.example.com`
becomes a grant like `chatgpt.client.example.com-1a2b3c4d`. The client host is
the metadata document's host, or a registered client's first redirect URI's. The suffix tells
apart grants made from codes of the same name. `okf-mcp key revoke`,
`grant`, `ungrant` and `maintenance-report` act on API keys only: they never
match a grant, even one whose name and actor equal a key's. Reach a grant by
its id, with `okf-mcp oauth grant revoke` or the `/api/v1/keys/{id}` routes
("API keys"). The one `key` subcommand that does reach grants is
`okf-mcp key assign`, which changes only a grant's user.

**Revoking.**

- **A connection**: revoke its grant. Made with the CLI, the revoke takes
  effect within `poll_interval`; made in the console, at once. The client
  then gets 401 and must be connected again with a new code.
- **A code** that was never redeemed: `okf-mcp oauth code revoke`, or its
  drawer in the console. It takes effect at once (codes are read from the
  database on every use). A redeemed code cannot be revoked; revoke the grant
  it became (its `grant` column).
- **From the client**: disconnecting in the client may call `/oauth/revoke`.
  Revoking an access token ends only that token: the refresh token keeps
  working and mints a new access token, so the client stays connected.
  Revoking a refresh token ends its whole family, which disconnects the
  client; the grant then stays listed, with no working tokens, until an
  admin revokes it. Only a refresh-token revocation or revoking the grant
  disconnects a client.

Unused registered clients are deleted 24 hours after registration, and tokens
and unredeemed codes 7 days after they expire, by an hourly pass of the
server's poll loop.

### What an operator sees in the console

The API keys page (`/ui/#/keys`):

- **Sign-in codes**, below the keys. "New sign-in code" mints one (name,
  user, an optional actor, `read` or `write`, catalogs, default, an expiry of
  1 hour to 7 days)
  and shows it once, in a dialog that keeps it only until "I have stored it"
  is clicked. The list shows live codes; "Show redeemed, expired and revoked
  codes" shows the rest. A live or expired code's drawer revokes it, and
  every code's drawer has Assign or Change user.
- **OAuth grants**, in the key list, marked `OAuth` with their client's name.
  A grant's drawer shows its kind ("OAuth grant") and client, revokes it, and
  changes its catalogs. It has no maintenance-reporting switch: a grant never
  reports.

The Users page (`/ui/#/users`) has a filter box that narrows the list, as
you type, to users whose name, display name or email contains the text, in
any letter case. Its user drawer has the sign-in controls: the
Admin switch, Email (Save, Clear), "Send setup email", Entitlements, the
console sessions with Sign out and "Sign out everywhere", and Recent
sign-ins. A catalog's drawer (`/ui/#/catalogs`) has Team member access. The
Settings page (`/ui/#/settings`) edits the policy settings, lists the OIDC
providers and the mail provider read-only, sends a test email and shows the
mail queue with Retry and Delete. A scoped admin sees these controls
disabled, with the reason.

`GET /api/v1/whoami` and the key list carry each credential's `kind` (`key`
or `oauth`) and `client_name`. The console's codes JSON API is under "Web
console" ([OAuth sign-in codes](#oauth-sign-in-codes)).

### What the server checks

- **Clients.**
  - A `client_id` that is an `https://` URL is fetched as a Client ID
    Metadata Document: over `https`, with no redirects and no proxy, within 5
    seconds and 16 KiB, and only from a public address (loopback, private,
    link-local and unique-local addresses are refused, unless
    `cimd_allow_private`). The connection goes only to the addresses checked,
    so DNS rebinding cannot swap them. The document must be a JSON object
    whose `client_id` equals its URL. It is cached for its `Cache-Control`
    max-age, clamped between 5 minutes and 24 hours; if a refetch fails, the
    cached copy is used for up to a day after it was fetched.
  - Any other `client_id` must have been registered at `/oauth/register`:
    public clients only, 1 to 10 redirect URIs, each `https` or `http` on a
    loopback host.
  - Either way the client must be able to authenticate with `none`: its
    `token_endpoint_auth_method` is absent or `none`, or its
    `token_endpoint_auth_methods_supported` lists `none` (as ChatGPT's
    document does). Grant types other than `authorization_code` and
    `refresh_token` are ignored. A refused client gets a generic "not known"
    page; the server logs the reason at `warn` (`refused an OAuth client`,
    with the `client_id`).
- **Redirects.** The `redirect_uri` must equal one the client declared,
  character for character. An error found before that check (an unknown
  client, a redirect URI it did not declare) is shown on the server's own
  error page, never sent to an unchecked address. Later errors go back to
  the client with `error`, `state` and `iss`.
- **PKCE.** S256 only, on every authorization.
- **Audience.** `resource` is required at authorize and at the code
  exchange, and must be `<public_url>/mcp` (RFC 8707). Every token is issued
  for it and refused with any other.
- **Replay.**
  - An authorization code works once, within 60 seconds. Presenting it again
    revokes the tokens issued from it, even if the two requests race. A
    failed exchange (a wrong verifier, client, redirect URI or resource)
    also spends it.
  - A refresh token works once. Presenting a used one revokes the whole
    connection's tokens, and the client must be connected again with a new
    code. A refresh by another client, or for another `resource`, is refused
    and changes nothing.
- **Rate limits and caps.** Registration (a burst of 10, then one every 6
  seconds), the token and revoke endpoints (60, then 2 a second, shared) and
  the consent form (20, then 1 a second) are limited globally, not per
  address. Over the limit, the JSON endpoints answer 429 `slow_down` and the
  consent form a 429 page. Registration is also refused (429 `slow_down`)
  while 500 registered clients have no grant. At most 10,000 consent pages
  and 10,000 unexchanged authorization codes are held in memory; past that a
  new sign-in gets a 503 page.

Tokens and codes start with `okfat_` (access), `okfrt_` (refresh), `okfac_`
(authorization code) and `okfsc_` (sign-in code), so secret scanners can
find them. The server stores only their SHA-256 and never logs them.

### OAuth routes

| Route | Key | Does |
|---|---|---|
| `GET /.well-known/oauth-protected-resource`, `GET /.well-known/oauth-protected-resource/mcp` | none | Protected Resource Metadata (RFC 9728) |
| `GET /.well-known/oauth-authorization-server` | none | Authorization server metadata (RFC 8414). `registration_endpoint` is absent when `dcr_enabled = false` |
| `POST /oauth/register` | none | Dynamic client registration (RFC 7591), public clients only: 201 with the client's metadata and `client_id`. 404 when `dcr_enabled = false` |
| `GET /oauth/authorize`, `POST /oauth/authorize` | none (a console session, or a sign-in code on the form) | The consent page; without a session it redirects to `/signin` first (`signin=code` takes the sign-in code path instead). Approve and Deny redirect (303) to the client |
| `POST /oauth/token` | none (PKCE, or a refresh token) | `grant_type=authorization_code` or `refresh_token`: answers `access_token`, `token_type: Bearer`, `expires_in` and `refresh_token` |
| `POST /oauth/revoke` | none | Revokes a token (RFC 7009): 200 for any token, an unknown one included; 400 `invalid_request` without `token` |

All seven paths exist only while `public_url` is set; without it they are 404.
Every response is `Cache-Control: no-store` (the discovery documents also
carry `Access-Control-Allow-Origin: *`), and every JSON error is
`{"error": ..., "error_description": ...}` with the OAuth error code:
`invalid_client` is 401, `slow_down` 429, `server_error` 500 and the rest
400.

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
- **0.12.0 (web console)** adds no migration and no setting. It serves
  `/ui/` and `/api/v1/` on the existing port, and `/` now redirects to
  `/ui/` (it was 404). The ingress above routes them with no change. A proxy
  that sets its own `Content-Security-Policy` must keep the console's sources
  ("Web console").
- **0.13.0 (OAuth)** adds migration `0008_oauth.sql`: existing keys become
  keys of kind `key` and work as before, and an older binary refuses the
  migrated database. It adds the `public_url` setting and the `[oauth]`
  table. Without `public_url` nothing else changes: the OAuth routes are 404
  and a 401 still carries a plain `WWW-Authenticate: Bearer`. With it, see
  "OAuth connectors" (and allow the pod outbound HTTPS if clients use
  metadata documents). Every tool now carries a title and behaviour hints
  (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`),
  which clients may show; no tool's arguments or results changed.
- **User login (the first release after 0.14.0)** adds migration
  `0010_user_login.sql` and apalis' `apalis` schema, and an older binary
  refuses the migrated database. Before upgrading:
  - check that the database owner can create the trusted extensions
    `citext`, `pgcrypto` and `hstore` (Postgres 13 or later), and that the
    database's `max_connections` leaves room for the mail worker's 4 extra
    connections per server process (section 4);
  - decide on `[mail]`: without it the `log` provider logs every message,
    links included, so set `smtp` or `postmark` (and its secret) before you
    send anyone a setup link on a shared server;
  - add the `[auth]` and `[[auth.oidc]]` tables if wanted (section 3). A
    secret variable a table names must be set, or `serve` refuses to start.

  The migration only adds: existing keys, grants, codes and catalogs work as
  before, every existing key is admin-issued (uncapped), every catalog's
  member access is `write`, and the pasted-key console sign-in is unchanged.
  `/api/v1` now also takes a console session cookie, and `whoami` may answer
  `kind: "session"`. The new routes are `/signin`, `/account/*`,
  `/auth/passkey/*` and `/auth/oidc/*`; a proxy that routes only some paths
  must pass them. Then follow
  [Rollout](#rollout) to give the first admin a sign-in.
- **Team suspension (the first release after 0.15.0)** adds migration
  `0011_team_suspension.sql`, which only adds two nullable columns to
  `teams` (`suspended_at`, `suspension_notice`): every team stays active and
  nothing else changes until an admin suspends one. An older binary refuses
  the migrated database. `GET /api/v1/teams`, `GET /api/v1/catalogs` and
  `list_catalogs` gain fields, and `okf-mcp team list` a `suspended` cell
  before the display name, so a script that reads that listing by position
  (`cut -f`) takes the display name from field 6, not 5
  ([Suspending a team](#suspending-a-team)).

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
| 401 on `/mcp` or an archive route | No `Authorization: Bearer` header, or the key or OAuth access token is unknown, expired or revoked. The body is plain text, with `WWW-Authenticate: Bearer` (and `resource_metadata="..."` when OAuth is on) |
| 403 on every `/mcp` request through the ingress | The `Host` is not in `allowed_hosts` (rmcp's DNS-rebinding check). Add the public host name and restart the pod |
| 404 on `/mcp` with "Session not found" | The `Mcp-Session-Id` was opened by another key, idled out past `session_idle_timeout`, or predates a restart. Reconnect with the key that opened it |
| A tool returns `UnknownChangeset`, or `Refused` naming a closed changeset ("is published", "is discarded", "was published or discarded while this write waited", "is no longer open") | `UnknownChangeset`: the handle is mistyped or belongs to another key (only its key, and an `admin` that reaches its catalog, may use it). `Refused`: its changeset is closed, by `publish`, by `discard`, or by the prune of changesets left empty for 24 hours, possibly while the call waited for it. `list_changesets` lists the open handles the key may use; otherwise call `open_changeset` |
| A tool or `/api/v1` route returns 403 `Suspended` | The catalog's owner team is suspended (the body names the `catalog`, the `team` and the operator's `notice`). Export still works. An admin lifts it with `okf-mcp team unsuspend TEAM` ([Suspending a team](#suspending-a-team)) |
| A tool returns `CatalogNotSelected` or `Forbidden` | The key is not granted that catalog, or its access level is too low (a `read` key calling a write tool). `okf-mcp key grant`, or a key with more access |
| 404 on an archive route | The catalog doesn't exist or the key isn't granted it (never 403, so a key can't probe which catalogs exist) |
| 403 on an archive route | The key reaches the catalog but its level is too low: import needs `admin` |
| 413 on import | The body is over `max_import_bytes` plus slack (a plain-text axum rejection), or, if the answer comes from nginx, over the ingress's `proxy-body-size`. Raise both together |
| 409 on import | The catalog is not empty: add `?replace=true` (or `--replace` on the command line). Or, with `?expect_generation=N`, the catalog has moved since that export: export it again |
| `/healthz` is 503 and the pod is not ready | The database did not answer within 2 seconds: check the `Cluster`'s state, the `okf-mcp-db-app` secret and network policies |
| The pod never becomes ready on an IPv6 cluster | `listen` is `0.0.0.0:8080`; set `"[::]:8080"` |
| The server exits with a configuration error | The file failed validation; the message names the file and the key |
| The server exits naming a migration it does not know | The database was migrated by a newer version: deploy that version or newer |
| TLS errors at startup (`UnknownIssuer`, a name mismatch) | `PGSSLROOTCERT` names the wrong CA, or the URL's host is not one the certificate names (with CloudNativePG, use the `-rw` service from the `-app` secret's `uri`). Check that the URL names no `sslmode` or `sslrootcert` |
| `clock.ok` is false | Read the pod's `warn` line, "the clock check failed": often the name doesn't resolve from the pod, or UDP port 123 is blocked. `time_server` can be an address |
| Database tests fail locally with TLS errors | `PGSSLMODE=verify-full` is exported in your shell; unset it |
| `just test-ui` fails to launch Chromium | `RUSTWRIGHT_CHROMIUM` (or `CHROME`, `CHROMIUM`, or `PATH`'s `chromium-browser`) names a snap stub that cannot run headless. Unset it so rustwright uses the Playwright cache, or name a real Chromium ("Testing the console") |
| The console's fonts fall back, or the console logs CSP violations behind a proxy | The proxy adds its own `Content-Security-Policy`. Keep the console's sources ("Web console") |
| `/.well-known/oauth-protected-resource` is 404 | `public_url` is not set (OAuth is off), or a proxy does not route `/.well-known/`. Set it, restart, and check the proxy ("OAuth connectors") |
| The server exits naming `public_url` | It is not an `https` origin (a path, query or user info, or `http` off loopback), or its host is not in `allowed_hosts`. Give the bare origin and add its host to `allowed_hosts` |
| A client says it cannot register, or the consent page says "This client is not known to the server." | With `dcr_enabled = false`, the client must publish a Client ID Metadata Document. Otherwise its document could not be fetched or was refused: it must be `https`, on a public address, a JSON object under 16 KiB naming its own URL as `client_id`, and answer within 5 seconds without a redirect. Check that the pod can make outbound HTTPS connections |
| The consent page says the redirect URI is not one the client registered | The client sent a `redirect_uri` that is not, character for character, one of those it declared. Fix the client's registration or metadata document |
| The consent page refuses a sign-in code | The code is mistyped, expired, already redeemed or revoked (the page does not say which, on purpose). `okf-mcp oauth code list` shows its state; mint a new one |
| The consent page says "This sign-in request has expired or is not valid." or "Too many wrong sign-in codes." | The sign-in took longer than 10 minutes, the server restarted, or a code was refused five times. Start the connection again in the client |
| An OAuth client was connected but now gets 401 and asks to sign in | Its grant was revoked, it was unused for longer than `oauth.refresh_token_ttl`, the client revoked its refresh token, or a used refresh token was presented again (a sign of a copied token), which ends the connection's tokens. Mint a new code and reconnect |
| The browser shows a CSP error after Approve, and the client never connects | A proxy replaced the consent page's `Content-Security-Policy`. Its `form-action` must allow the client's redirect origin; let the server's header through on `/oauth/` |
| `/oauth/register` or `/oauth/token` answers 429 `slow_down` | The global rate limit, or 500 registered clients without a grant (each is deleted 24 hours after registration). Wait and retry; look for a client registering in a loop |
