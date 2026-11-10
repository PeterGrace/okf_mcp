# okf_mcp

A Rust port of `tools/okf.py`, the Open Knowledge Format catalog CLI, as a
shared MCP server (design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`).

Status: every milestone of the design is delivered: 1 (core library), 2
(analysis), 3 (storage), 4 (read-only server), 5 (writes), 6 (restructuring)
and 7 (deployment).

Deployed: `https://okf-mcp.k.vsix.me` (every route but `/healthz` needs a
bearer key), from the image `ghcr.io/petergrace/okf-mcp`, with its database
backed up continuously and a restore drill done.

The library reads a catalog into an in-memory `Snapshot` and runs `okf.py`'s
analyses on it with the same JSON output (compared as parsed values): `lint`,
the links report, `oldest`, `sprawl` and the secret scan, plus ranked
`search`, which `okf.py` does not have. Catalogs live in Postgres: every file
is a row holding its exact bytes and execute bit, with an append-only revision
history, and each catalog's snapshot is built from its rows and swapped when
the catalog changes. The `okf-mcp` command line administers catalogs and API
keys and imports and exports catalogs, and `okf-mcp serve` serves the catalogs
over MCP streamable HTTP with bearer-key auth: every read tool, the export and
import routes and `/healthz`, and the write tools: each connection's edits
collect in a changeset held in the database, which reads through that
connection see and which `publish` turns into published revisions (see
"Writing"). Concepts can also be moved, split and deleted, with every link and
listing kept in step. The server checks its clock against an NTP server and
reports the offset on `/healthz`, and connects to Postgres over verified TLS
when the environment asks for it.

## Running it

`docs/operations.md` is the operator guide: the container image
(`ghcr.io/petergrace/okf-mcp:<version>`), every configuration key and
environment variable, TLS to Postgres, a complete Kubernetes example with
CloudNativePG and an ingress, the first catalog and keys, `/healthz` and the
clock check, upgrades, backup and restore, and troubleshooting. The sections
below cover local use.

## Command line

    okf-mcp migrate
    okf-mcp catalog create pg --description "pgrace's catalog"
    git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD | okf-mcp import pg -
    okf-mcp key create claude --access write --catalogs pg --default pg --actor claude-code/claude-opus-5-5
    okf-mcp export pg pg.tar.gz

Every subcommand reads the database URL from `DATABASE_URL` (or
`--database-url`) and the optional TOML configuration file from
`OKF_MCP_CONFIG` (or `--config`); its fields are listed under "Running the
server" (the subcommands read only `max_import_bytes`, default `64MiB`, the
largest uncompressed import, but a file that fails validation fails every
subcommand). Every subcommand applies the embedded migrations first, so
`migrate` is only needed as a separate deploy step. `okf-mcp --help` and
`okf-mcp <command> --help` list the rest: `catalog update|list|delete`,
`key list|revoke|grant|ungrant|maintenance-report`, `import --replace
[--only-machinery] [--expect-generation N]`.
`key create --maintenance-report` opts a key into reporting independently of
its access level; `key maintenance-report ID --enabled true|false [--actor ACTOR]`
changes the flag. Reporting still requires catalog read access and never
confers write access.

`import` takes a `.tar.gz` file, `-` for one on stdin, or a directory. A
`git archive` tarball imports exactly the checkout's tracked files, and a
tarball breaking the import rules (a symlink, a `..` path, a duplicate, ...)
is refused with every problem listed. A directory is read leniently, through
the same listing as the analyses: untracked files are included, and a
symlink, special file or non-UTF-8 name it cannot store is flattened or
skipped with a warning (see `src/archive.rs`). `key create` prints the new key
once and its immutable id on stderr; only the key's SHA-256 is stored. The
positional value is a grouping name: several live credentials may share it
when their actors differ. `revoke`, `grant` and `ungrant` accept the immutable
id, an unambiguous live name, or `NAME --actor ACTOR`. A revoked name/actor
pair can be issued again without changing the old credential's history.

## Report-only maintenance

Maintenance stores revision-pinned investigator reports separately from canonical
content. It never edits, merges, splits or deletes concepts, writes verification
stamps, or publishes changesets. `accepted_for_follow_up` records a decision,
not an applied change. No automatic canonical edits or resident scheduler exist;
external URLs are not fetched. Reported support is an attributed assessment, not
server or human verification.

The ten MCP tools are `maintenance_claim`, `maintenance_renew`,
`maintenance_read_revision`, `maintenance_submit`, `maintenance_close`,
`maintenance_list_runs`, `maintenance_get_run`, `maintenance_list_findings`,
`maintenance_get_finding` and `maintenance_dispose`. Every call requires an
explicit `catalog`, even on a connection with a default. Reporting permission
is separate from ordinary Read/Write/Admin access; writers do not automatically
become reporters.

One claim opens one batch: default 10, range 1–50 primary concepts, 900-second
leases and a 3,600-second run deadline. Explicit failed/inconclusive attempts
retry after 86,400 seconds; expired leases can be reclaimed immediately.
See [maintenance operations](docs/operations.md#report-only-maintenance) for
permissions, exact limits, JSON pagination and a synthetic end-to-end workflow.
Ordinary canonical exports omit reports; database backups include them. The
secret scanner is heuristic, not proof that text contains no secret.

### Optional report-only runner

`okf-maintenance` is a separate, opt-in one-shot controller. `cargo run` still
selects `okf-mcp`; select the runner explicitly:

```sh
cargo run --bin okf-maintenance -- --help
cargo run --bin okf-maintenance -- evaluate --cases tests/fixtures/investigator/corpus/evaluation.json
# Only after operator configuration and credential provisioning:
# okf-maintenance preflight --config /etc/okf-maintenance/config.toml
# okf-maintenance run-once --config /etc/okf-maintenance/config.toml
```

[Operator instructions](docs/operations.md#report-only-runner) describe consent,
provider API authentication, privacy, recovery and exit status.
[Examples](examples/maintenance/config.toml) have both consent flags **false**;
the accompanying service/timer are disabled examples, not installed scheduling.
Preflight checks version/help and MCP schemas, not paid-envelope compatibility or
reporting grants. The only inspected real Claude CLI version is
`2.1.319 (Claude Code)` (help/version only). Independent human review of the
synthetic corpus and a separately authorized, sanitized one-item live smoke run
remain mandatory before enabling scheduling. Generated fixture labels are not
that human review. Exit 0 never establishes factual freshness.

## Running the server

`okf-mcp serve` (also what `okf-mcp` with no subcommand runs) applies the
migrations, loads every catalog's snapshot, starts the poll loop and serves
HTTP until SIGINT or SIGTERM:

    export DATABASE_URL=postgres://okf@localhost/okf   # never logged
    okf-mcp serve --config okf-mcp.toml --listen 127.0.0.1:8080

`--listen` (or `OKF_MCP_LISTEN`) overrides the file's `listen`. The
configuration file is TOML; every field is optional, and unknown keys are
ignored, so one file serves every version:

```toml
listen = "0.0.0.0:8080"            # the default
allowed_hosts = ["okf-mcp.example"] # /mcp Host names (default: loopback)
default_catalog = "pg"             # for a key with no default of its own
poll_interval = "10s"              # how often to look for CLI changes
session_idle_timeout = "2h"        # an idle MCP session is closed after this; also
                                   # when an untouched changeset counts as idle
max_import_bytes = "64MiB"         # uncompressed total of an import
concept_outline_chars = 20000      # get_concept outlines a longer body
database_max_connections = 16      # database pool size, per process
overlay_cache_size = 64            # open changesets' views kept in memory
maintenance_review_intervals = { pg = "30d" } # per-catalog; default 30d, minimum 1d
time_server = "time.example.com"   # NTP server for the clock check (default: none)
```

A zero `poll_interval` or `session_idle_timeout`, an empty `allowed_hosts`
or a blank entry in it, or a blank or padded `time_server`, is refused at
load. Behind an ingress,
`allowed_hosts` must name the public hostname (rmcp's DNS-rebinding check
answers any other `Host` on `/mcp` with 403).

Create a catalog, fill it and make a key with the subcommands; the server
notices each change within `poll_interval`, without a restart (a revoked key
stops working at the next poll too):

    okf-mcp catalog create pg --description "pgrace's catalog"
    git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD | okf-mcp import pg -
    okf-mcp key create laptop --access read --catalogs pg --default pg

`key create` prints the key once (and its administration id on stderr). Keep it in an environment variable, here
`OKF_MCP_KEY`, and never in a file you commit.

### Connecting a client

An `mcp.json` entry (Claude Code expands the `${...}` variables):

```json
{
  "mcpServers": {
    "okf": {
      "type": "http",
      "url": "http://127.0.0.1:8080/mcp",
      "headers": {
        "Authorization": "Bearer ${OKF_MCP_KEY}",
        "X-OKF-Catalogs": "${OKF_CATALOGS:-pg}"
      }
    }
  }
}
```

`X-OKF-Catalogs` (or `?catalogs=pg,work` on the URL) names the connection's
catalogs: the whole list is the read set and the first is the default write
target. Without it the key's default catalog is used, then the server's
`default_catalog`. A tool's `catalog` (or `search`'s `catalogs`) argument
wins over all of these. Every step is limited to the key's grants, and a
catalog the key cannot reach is an error, never silently dropped.

### Tools

The read tools below need a `read` key; the write tools ("Writing") need
`write`. Results are JSON (`structuredContent`, and the
same JSON as the text block); errors are results with `isError: true` and
`{"error": ..., "message": ...}`.

Every catalog path in an error body (`path`, `files[].path`,
`links[].source`, `errors[].file`, `warnings[].file`) has a leading `/`
(`/infra/x.md`), like the paths in tool results such as `touched`. An
`InvalidPath` whose path climbs out of the catalog (`../x`) echoes it as
given. The report results (`lint`, `oldest`, `sprawl`, `links_report`) keep
`okf.py`'s paths without the `/`, so the same lint finding names `/a.md` in
a `LintFailed` error and `a.md` in `lint`'s result, on purpose.

| Tool | Does |
|---|---|
| `list_catalogs` | The catalogs the key reaches, and this connection's selection |
| `search` | Ranked search across the selected catalogs (`query`, `catalogs`, `type`, `tags`, `status`, `limit` up to 200) |
| `get_concept` | A concept's frontmatter, body and size; with `links`, its links and backlinks; with `raw`, the file. A body over `concept_outline_chars` comes back as an outline of its headings; `section` returns one section (by heading text or slug), `full` the whole body |
| `read_document` | The text of an `index.md`, `log.md` or `AGENTS.md` (default `/index.md`) |
| `history` | A document's revisions, newest first (`limit` up to 200) |
| `open_questions` | Every queued `open_questions` entry, by concept |
| `oldest` | `okf.py oldest --json` |
| `sprawl` | `okf.py sprawl --json` |
| `links_report` | `okf.py links --json` (`mentions`, default true) |
| `lint` | `okf.py lint --json` on the published catalog, or with `changeset` on that changeset's drafts applied to it |
| `pending_changes` | The changeset `changeset` names: each changed path, its base and current revision, whether it is stale or replayable, and a unified diff |
| `list_changesets` | The open changesets the key may use, empty ones included: its own, or with an `admin` key every key's in the catalogs it reaches. Each row has the handle, catalog, key name and immutable id, actor, creation and last-change times, `/`-prefixed paths, and `idle` (no change within `session_idle_timeout`) |

`get_concept`, `read_document` and `history` without `catalog` answer from
the first selected catalog that has the path; the reports run on the first
selected catalog. Every read tool but `history` and `list_catalogs` takes an
optional `changeset` handle: that changeset's catalog is then read through
its drafts, and every other catalog as published. A report given one runs on
the changeset's catalog unless `catalog` names it, and a handle whose catalog
the call does not read is refused. Without `changeset` a read is always
published, whatever the key has open (`history` reads published revisions
only).

### Routes

| Route | Key | Does |
|---|---|---|
| `GET /healthz` | none | `{"database": "ok", "catalogs": N, "clock": ...}`: 200, or 503 when the database does not answer within 2 s; `clock` is the last clock check (`docs/operations.md`, "Health and time") |
| `POST/GET/DELETE /mcp` | any | MCP streamable HTTP |
| `GET /catalogs/{name}/export` | `read` on the catalog | The catalog as `application/gzip`, `Content-Disposition: attachment; filename="<name>.tar.gz"` |
| `POST /catalogs/{name}/import[?replace=true&only_machinery=true&expect_generation=N]` | `admin` on the catalog | Imports the uploaded `.tar.gz` and answers with the import report |
| `GET /` | none | Redirects (303) to the web console, `/ui/` |
| `GET /ui/*` | none | The web console's static files ("Web console" below) |
| `/api/v1/*` | any (each endpoint checks its level) | The console's JSON API (`docs/operations.md`, "Web console") |

```sh
curl -s http://127.0.0.1:8080/healthz
curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" -o pg.tar.gz \
    http://127.0.0.1:8080/catalogs/pg/export
git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD |
    curl -sf -H "Authorization: Bearer $OKF_MCP_KEY" --data-binary @- \
        "http://127.0.0.1:8080/catalogs/pg/import?replace=true"
```

A missing or unknown key gets 401 (`WWW-Authenticate: Bearer`, a plain-text
body). A catalog that does not exist, or that the key does not reach, is 404
(never 403, so a key cannot probe which catalogs exist); a reachable catalog
the key's level does not allow (a `read` key importing) is 403. Those and
the other route errors have the body `{"error": ..., "message": ...}`; the
401, the 413, the timeout layer's 503 below and `/healthz`'s own body are the
exceptions.

The import checks the key before it reads the upload. It then reads and checks
the tarball (400 with every problem for an unreadable or rule-breaking
archive), and only then looks the catalog up, so a key granted every catalog
that posts a bad archive to an unknown one gets 400, not 404. It answers 409
for a non-empty catalog without `?replace=true` (the message says to add it),
or when `?expect_generation=N` no longer matches (the message says to export
again). `?only_machinery=true` replaces only repository machinery, as
`--only-machinery` does.
A body over `max_import_bytes` plus a little gzip slack is 413, axum's
plain-text rejection rather than the error JSON. Only the upload is bounded:
if reading and checking it takes more than 120 s the answer is 503 with
`{"error": "Timeout", ...}` and nothing is imported; the import and the
snapshot refresh after it have no deadline, so a committed import always gets
its report, and the next read sees the new snapshot. `/healthz` and the export
time out after 120 s with 503; `/mcp` has no timeout, since its `GET` is a
long-lived event stream.

## Web console

`/ui/` is a browser console for the server, and `/` redirects there. Sign in
by pasting an API key: the console acts with exactly that key's access and
hides what the key cannot do. The key stays in that browser's `localStorage`
until you sign out (or only in memory for the page's lifetime, where storage
is blocked), so sign out on a shared machine.

The Health page shows lint, sprawl, link, staleness and open-question counts,
with each report one click away.

Catalogs can be created, described, deleted, imported and exported from the
console's Administer group, with no archive piped through a shell on the
server.

Every console action is an `/api/v1` call with the same bearer key, so
anything the console does can be scripted:

    curl -s -H "Authorization: Bearer $OKF_MCP_KEY" http://127.0.0.1:8080/api/v1/whoami

The routes, the security headers and the API reference are in
[`docs/operations.md`](docs/operations.md#web-console).

## Writing

### Changesets

A `write` key's edits go to a changeset, held in Postgres. Call
`open_changeset` (its `catalog`, default the connection's write target) for a
handle, then pass it as `changeset` to every write tool, `publish`,
`discard`, `revert` and `pending_changes`. A write without one fails the
parameter check (`missing field \`changeset\``); a write never opens a
changeset by itself. A key may have several changesets open at once, and the
write tools take no `catalog`: the changeset's catalog is the one written.

The handle works on any connection of the key that opened it, so after a
reconnect or a server restart the same handle carries on. An `admin` key
that reaches the catalog may use it too. Any other key gets
`UnknownChangeset`, worded exactly as for a handle that does not exist.

Nothing in a changeset is visible to anyone else until `publish`, and a read
sees its drafts only when it passes the handle. A changeset survives
disconnects, closed sessions and server restarts until it is published or
discarded; one that gets no edit within 24 hours is discarded. A handle whose
changeset was published or discarded is refused: open a new one. Each MCP
session also answers only the key that opened it: a request naming its
`Mcp-Session-Id` with another key gets 404.

An `mcp.json` entry:

```json
{"mcpServers": {"okf": {"type": "http", "url": "https://okf.example/mcp",
  "headers": {"Authorization": "Bearer ${OKF_MCP_KEY}", "X-OKF-Catalogs": "pg"}}}}
```

A write tool's result names the catalog, the changeset's handle and the
touched paths.

### Write tools

Content (`write`):

| Tool | Does |
|---|---|
| `create_concept` | A new concept (`path`, `frontmatter` with `type`, `body`); the server writes `generated` itself. With `index_summary` (and `index_section`) it is listed in its folder's `index.md`, creating folder indexes as needed |
| `edit_document` | Exact replacement (`old_str`, `new_str`, `replace_all`) in a concept or an `index.md`; must match once unless `replace_all`; may not add or change frontmatter |
| `replace_body` | A concept's new body, its frontmatter kept byte for byte |
| `set_frontmatter` | Sets one descriptive key (`key`, `value`), or deletes it with `value: null`; the stamps, `open_questions` and `sources` have their own tools |

Bookkeeping (`write`), ports of `okf.py`'s commands with its exact output:

| Tool | Does |
|---|---|
| `stamp` | `generated` or `verified` (`field`) with `{ by, at: now }` |
| `review` | `reviewed` with `outcome` and `note`, queuing any `questions` |
| `question` | Queues one open question |
| `unquestion` | Removes one, by 1-based number or exact text |
| `add_source` | Appends a `sources` entry (`id`, `resource`, `title`, `author`) |
| `remove_source` | Removes a `sources` entry by `id`, deleting the key when none are left; refused while a footnote still cites it (not in `okf.py`) |
| `log` | `* **<kind>:** <text>` under today's date in `log.md`, the only way to write it (`kind`: `Update`, `Creation`, `Change`, `Import`) |
| `index` | Lists a concept in its folder's `index.md` |

Restructuring (`write`):

| Tool | Does |
|---|---|
| `move_concept` | Moves or renames a concept (`old`, `new`), port of `okf.py mv`: its folder listing moves with it, and every link to it is rewritten, keeping each link's style and anchor |
| `split_concept` | Carves a `section` out of a concept (`path`) into a new one (`to`), port of `okf.py split`: the parent keeps a stub linking to it, links to the section follow it, and it is listed in its folder (`type`, `title`, `description`, `stub`, `sources` as `okf.py`'s flags) |
| `delete_concept` | Deletes a concept and its listing; refused with `StillLinked`, listing each link, while any other document links to it |

Changesets:

| Tool | Key | Does |
|---|---|---|
| `open_changeset` | `write` | Opens a changeset in a catalog and returns its handle |
| `pending_changes` | `read` (the owner or an admin) | What the changeset holds, with diffs |
| `publish` | `write` | Publishes the changeset with a `message` (see below) |
| `discard` | `write` | Drops the named `paths` from the changeset, or every entry, which closes it for good: its handle is refused from then on, so call `open_changeset` for new work |
| `revert` | `write` | Adds a document's content at an earlier `revision` (from `history`), or its deletion, to the changeset |
| `list_changesets` | `read`; `admin` to see every key's | The open changesets the key may use, with their handles: how to find a handle again after a reconnect. An admin may then use another key's handle directly |

`lint` with `changeset` checks the changeset before publishing.

### Publish

`publish` runs under the catalog's publish lock, so publishes to one catalog
run one at a time against the current published state. With nothing to
publish it answers `published: false`. Otherwise it checks each changed
file's base revision against the one published now:

- `log` and `index` entries (and `create_concept`'s listing), and the listing
  changes and link rewrites that `move_concept`, `split_concept` and
  `delete_concept` make in `index.md` and `log.md`, are replayed onto newer
  text instead of conflicting, so concurrent agents logging to the same
  `log.md` all get their lines in; a moved listing whose bullet someone
  changed meanwhile refuses the publish rather than drop that change;
- any other file that changed since the changeset first read it refuses the
  publish with `Conflict`, listing each file's base and current revision
  (`pending_changes` marks it `stale`). An entry keeps the base of its first
  edit, so `discard` the conflicting paths, re-read the published text, redo
  the edit and publish again;
- a changed file with lint errors, or a link left pointing into a path the
  changeset deletes (or moves away from), refuses it with `LintFailed`; lint
  errors elsewhere and warnings come back as `warnings`.

A refused publish leaves the changeset open and unchanged. A successful one
writes a revision per changed file, carrying the message and the key's actor,
and closes the changeset. A write that was waiting on the changeset while it
was published or discarded is refused and stores nothing; open a new
changeset and redo it.

### Write rules

Every write checks these before anything is stored, and publish checks the
secret scan and frontmatter again on every changed file:

- the secret scan (a hit is reported with its rule, line and a masked value);
- frontmatter must still parse;
- paths stay inside the catalog;
- machinery is read-only through every tool, `revert` included: `AGENTS.md`,
  `CLAUDE.md`, `README.md`, `tools/`, the root `docs/` and `tests/`,
  `.claude/`, `install/` and any `.git*` path. Only an import writes them.

Every timestamp comes from the server's clock; no tool takes one. `by`
(on `create_concept`, `split_concept`, `stamp` and `review`) defaults to the
key's actor.

## Layout

- `src/okf/` - the document model (`yaml`, `doc`, `markdown`, `paths`,
  `workspace`, `timestamp`), the `snapshot`, `analysis::*` (`links`, `lint`,
  `oldest`, `sprawl`, `secrets`), `search`, the edit operations `ops`
  (ported from `okf.py`: the bookkeeping, `mv`, `split` and its link
  rewriting, with the replay of their `index.md` and `log.md` edits; and
  `delete`) and the write `rules`.
- `src/db.rs`, `migrations/` - the Postgres pool and the schema, embedded at
  compile time.
- `src/store/` - async functions over the pool: `catalogs`, `keys`,
  `documents`, `import`, `export`, `history`, `changesets` and `publish`.
- `src/archive.rs` - reading a `.tar.gz` or a directory with the import rules,
  and writing a `.tar.gz`.
- `src/keys.rs` - API key generation, hashing and constant-time comparison.
- `src/catalog.rs` - `CatalogState`: a catalog's snapshot, loaded from its
  rows and rebuilt and swapped when its generation moves; lint with revision
  times.
- `src/auth.rs` - the key cache and the bearer middleware; `src/selection.rs` -
  which catalogs a call acts on; `src/app.rs` - the
  shared state and the poll loop (which also prunes empty changesets).
- `src/overlay.rs` - a changeset's view (the published files with its entries
  applied), cached per changeset; `src/write.rs` - the one path every write
  tool takes into its changeset.
- `src/mcp/` - the rmcp server handler, the tool context, the read tools, the
  write tools (`writes`), the restructuring tools (`restructure`) and the
  changeset tools (`changesets`); `src/http/` - the router, `/healthz`, the
  archive routes, the session-to-key binding (`sessions`) and `serve`.
- `src/cli.rs`, `src/config.rs`, `src/main.rs` - the `okf-mcp` command line
  and its configuration file.
- `src/clock.rs`, `src/error.rs` - the time source and the error type.
- `src/ntp.rs` - a minimal SNTPv4 client; `src/clock_check.rs` - the clock
  check at startup and every 15 minutes, and the status `/healthz` shows.
- `tests/parity.rs` - differential tests against `okf.py`: goldens for the
  synthetic corpus in `tests/fixtures/catalog`, and live tests that run
  `okf.py` itself.
- `tests/roundtrip.rs` - import then export reproduces a catalog's files
  exactly, and importing the export again changes nothing.
- `tests/server.rs` - end-to-end tests of the server over HTTP with rmcp's
  client: keys, catalog selection, session binding, the tools and the archive
  routes.
- `tests/writes.rs` - end-to-end write tests over HTTP: create, link, index,
  log, publish and export; concurrent `log.md` publishes; a conflict; a
  changeset surviving a restart; two handles of one key; concurrent writes
  to one handle; a write without a handle; a leftover `X-OKF-Changeset`
  header ignored.
- `tests/restructure.rs` - end-to-end restructuring over HTTP: a move
  published and exported; a conflict and a replay against other sessions; a
  delete refused while linked.
- `tests/tls.rs` - `verify-full` TLS to a Postgres with test certificates:
  one signed by the trusted CA connects, one signed by another is refused.
- `tests/smoke.rs` - every analysis and a search on a real catalog, the
  storage path (import, load, links, lint, export) through Postgres, and the
  server (every read tool and the export over HTTP) and the write path
  (create, stamp, log, lint through the changeset and publish over HTTP,
  into a throwaway copy) and the restructuring tools (move, split, a refused
  delete and publish), timed.
- `tests/support/` - a throwaway Postgres container per test, and a test
  server on a free port with rmcp clients.
- `Dockerfile`, `.github/workflows/image.yml` - the container image, built and
  pushed to GHCR on a `v*` tag.
- `docs/operations.md` - the operator guide.
- `docs/superpowers/` - the design and the per-milestone plans.
- `skills/` - Claude Code skills for daily catalog maintenance
  ([`skills/README.md`](skills/README.md)).
- `changelog/` - one dated note per change.

## Using the library

Every analysis is a plain function over a `Snapshot`, with "now" passed in
rather than read from the clock:

```rust
use chrono::Utc;
use okf_mcp::okf::analysis::lint::lint;
use okf_mcp::okf::search::{search, SearchQuery};
use okf_mcp::okf::snapshot::Snapshot;
use okf_mcp::okf::workspace::DiskWorkspace;

// `Box<dyn Error>` lets `?` pass on either crate's error type.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = DiskWorkspace::new("/path/to/catalog")?;
    let snapshot = Snapshot::build(&workspace)?;
    // `None`: no git commit times, so lint skips `generated-after-commit`.
    let report = lint(&snapshot, Utc::now(), None);
    println!("{}", serde_json::to_string_pretty(&report)?);
    for hit in search(&snapshot, &SearchQuery::new("time server")) {
        println!("{} {:?}", hit.path, hit.title);
    }
    Ok(())
}
```

The report structs derive `serde::Serialize`; their JSON has `okf.py
--json`'s keys in the same order.

## Testing

    cargo test                                   # unit, doc, golden and database tests
    cargo test --test writes                     # just the end-to-end write tests
    cargo test --test restructure                # just the end-to-end restructuring tests
    cargo test --test tls                        # just the TLS-to-Postgres tests
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --test parity -- --ignored    # live parity against okf.py
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --test roundtrip -- --ignored # the real catalog round-trips
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --release --test smoke -- --ignored --nocapture   # timings
    UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens
    just test-ui                                 # the web console's browser journeys

`cargo test` needs Docker: every test that touches the database starts its own
throwaway `postgres:17-alpine` container (via `testcontainers`) and removes it
when done (the end-to-end server tests too); without Docker those tests fail
at once with "is docker reachable?". The roundtrip, storage and server smoke
runs need Docker too, and the real-catalog round trip needs `git` (it imports
`git archive` of the checkout).

The live parity tests need `python3` (3.12 or later: sprawl's scores depend
on its float `sum()`) and `okf.py` (`OKF_PY`, default
`~/repos/pg_knowledge_catalog/tools/okf.py`). The real catalog is read in
place and never copied into this repository. `#[ignore]` marks a test that
plain `cargo test` skips; `-- --ignored` runs only those.

The browser journeys (`tests/ui_e2e`, run by `just test-ui` and by plain
`cargo test`) drive headless Chromium through `rustwright`. They need Docker
too, and find Chromium through `RUSTWRIGHT_CHROMIUM`, `CHROME`, `CHROMIUM`,
the Playwright cache or `PATH`; without one, each journey prints a skip line
and passes.

Before a push, also run `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.
