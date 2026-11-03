# OKF MCP server: design

- **Date:** 2026-10-26; storage redesigned 2026-11-02 (see "Revision: Postgres storage")
- **Status:** approved design. M1 (core library), M2 (analysis), M3 (storage) and M4
  (read-only server) are delivered; see "Milestones"
- **Catalog concept:** `/projects/okf-mcp-server.md` in `pg_knowledge_catalog`
- **Replaces:** `tools/okf.py` in every OKF catalog (how the existing git catalogs hand
  over to the server is decided once the server is complete)

## Goal

A shared HTTP MCP server, running on the home k8s cluster, that gives any agent harness
that speaks HTTP MCP, including ones with no filesystem and no git, a full read-write
interface to one or more OKF knowledge catalogs. It enforces the catalog rules
(server-clock timestamps, byte-preserving frontmatter edits, secret rejection, lint
before publish) instead of relying on agents following instructions.

Catalogs live in Postgres. The server never runs git and knows nothing about forges.
An existing OKF catalog (a git checkout, from any forge) comes in through import, and
any catalog goes out as a tarball through export.

## Revision: Postgres storage (2026-11-02)

The first version of this design kept each catalog as a git clone on a PVC, with one
worktree per MCP session, `pull --rebase` on publish, forge credentials and a Forgejo
webhook. Before M3 was planned, pgrace replaced that with Postgres storage:

- Adding or removing a catalog was a ConfigMap edit, a Secret, key changes and a
  restart. Now a catalog is a row, created and imported with one command.
- Catalogs may come from repositories on any forge, so nothing in the server may be
  forge-specific. With no git at runtime there are no credentials, clones or webhooks.
- Concurrency comes from transactions and per-document revisions instead of rebases;
  history is a revisions table instead of `git log` (the same shape as pr-review-bot's
  knowledge bundles, which store OKF concepts in Postgres).
- Export is an authenticated route that builds a tarball on request. There is no
  automated mirror to git.

What did not change: HTTP transport, bearer keys, catalog selection, the existing tools
(this revision adds `history`, `revert`, `adopt_changeset` and an `admin` access level), the
write rules, the time rules, and everything M1 and M2 built. Documents are stored as
their exact text, so `okf::doc`'s byte-preserving edits, the snapshot and every analysis
work unchanged over an in-memory workspace loaded from the database.

## Decisions

| Question | Decision |
|---|---|
| Where it runs | One shared service on the k3s cluster (`kubectl --context default`), deployed by Argo CD |
| Transport | MCP streamable HTTP via `rmcp` 3.8, mounted in an `axum` 0.8 router, stateful sessions |
| Storage | Postgres (a CNPG cluster), via `sqlx` with embedded migrations. Postgres is the source of truth |
| Document form | Each file's exact text; type, title, status and tags are derived columns. Nothing is re-rendered |
| Auth | Bearer API keys stored as SHA-256 hashes in the database; per-key catalog grants and `read`/`write`/`admin` access, each level including the ones before it and always limited to the key's granted catalogs |
| Scope | Full read-write: every `okf.py` command plus content editing, search and concept retrieval |
| Implementation | Port everything to Rust. No Python at runtime |
| Write grouping | One changeset per (catalog, key, label), created on first write and kept in the database until published or discarded; explicit `publish`. The label is the connection's `X-OKF-Changeset` header, else `?changeset=`, else `default` (never the MCP session id, which changes on every reconnect) |
| Concurrency | Publishes to a catalog are serialized by a per-catalog lock; each checks its documents' base revisions, replays bookkeeping edits of `log.md` and `index.md` onto the current text, and re-checks links into deleted or moved paths |
| Getting data in and out | Import from an OKF directory or tarball; export as a tarball through an authenticated route. No git anywhere in the server |
| Administration | `okf-mcp` subcommands against the database (`migrate`, `catalog`, `key`, `import`, `export`), plus the HTTP import and export routes |

## Catalog selection

Every tool resolves the catalogs it acts on in this order, stopping at the first that is set:

1. The tool call's `catalog` argument (`catalogs`, a list, for `search`).
2. The connection: the `X-OKF-Catalogs: pg,work` header, or the `?catalogs=pg,work` query
   parameter on the MCP URL. `mcp.json` sets either; Claude Code expands `${OKF_CATALOGS}`
   there, which covers "env var in the harness". The first name is the default write
   target; the whole list is the read set.
3. The API key's `default_catalog`.
4. The server's `default_catalog`.

Every step is limited to the key's granted catalogs. Naming a catalog the key can't reach,
or one that doesn't exist, is an error. Catalogs are never silently dropped. Write
tools act on exactly one catalog: the explicit `catalog` argument, or the default write
target.

Example `mcp.json` entry:

```json
{
  "mcpServers": {
    "okf": {
      "type": "http",
      "url": "https://okf-mcp.example/mcp",
      "headers": {
        "Authorization": "Bearer ${OKF_MCP_KEY}",
        "X-OKF-Catalogs": "${OKF_CATALOGS:-pg}"
      }
    }
  }
}
```

The hostname is illustrative; the real one is chosen at deployment.

## Architecture

### Process and routes

One binary, `okf-mcp`, on tokio with an axum router:

| Route | Access | Purpose |
|---|---|---|
| `POST/GET/DELETE /mcp` | any key | rmcp `StreamableHttpService`: stateful (`Mcp-Session-Id`) for a client on a protocol version before `2026-07-28`, stateless for one on `2026-07-28` (see "Open questions for M5") |
| `GET /catalogs/{name}/export` | `read` on the catalog | The catalog as `application/gzip` with `Content-Disposition: attachment; filename="<name>.tar.gz"`, built in memory on request from the published documents |
| `POST /catalogs/{name}/import[?replace=true]` | `admin` on the catalog | Replaces or fills an existing catalog from an uploaded `.tar.gz` (see "Import") and answers with the import report once the new snapshot is swapped in. The key is checked before the body is read; then the tarball is read and checked (400 with every problem), then the catalog is looked up (404), so a key granted every catalog that posts a bad archive to an unknown one gets 400. A non-empty catalog without `?replace=true` is 409, its message naming `?replace=true`. A body over `max_import_bytes` plus 0.1% and 64 KiB of gzip slack is 413, axum's plain-text rejection rather than the error JSON. Creating and deleting catalogs is subcommand-only |
| `GET /healthz` | none | Liveness and readiness: `{"database": "ok"\|"unreachable", "catalogs": <count>}` (a count, no names: it needs no key), 200 while the database answers within 2 seconds (a pool connection included), else 503. The clock offset joins it in M7 |

`tower` middleware: request tracing, timeouts, and the auth layer on every route but
`/healthz`. rmcp's DNS-rebinding check (`allowed_hosts`, which defaults to localhost
only) is configured from `allowed_hosts` in the config, so the server works behind the
ingress.

`tower-http` traces every request (one `info` line each, by default). `/healthz` and the
export have a 120-second timeout (`timeout_layer()`), answered with 503. The import is
not behind it: only reading and checking its upload is bounded by the same 120 seconds,
answered with 503 and `{"error": "Timeout", "message": ...}` with nothing imported; the
import and the snapshot refresh after it have no deadline, so a committed import is never
answered with a timeout (a retry would get 409). `/mcp` has no timeout: its `GET` is a
long-lived event stream, bounded by rmcp's keep-alive and `session_idle_timeout`, and
every M4 tool reads an in-memory snapshot. SIGINT or SIGTERM cancels one token that ends rmcp's
sessions, the poll loop and the listener.

Subcommands:

| Subcommand | Does |
|---|---|
| `serve [--listen ADDR]` (default) | Runs migrations, loads snapshots, starts the poll loop, serves HTTP (`--listen`, or `OKF_MCP_LISTEN`, overrides `listen`) |
| `migrate` | Runs migrations and exits |
| `catalog create NAME [--description D]`, `catalog list`, `catalog delete NAME [--force]` | Manage catalogs. Delete refuses while the catalog has a non-empty open changeset, unless `--force`; it then removes the catalog's documents, revisions, changesets and grants (cascading foreign keys) |
| `import NAME PATH [--replace]` | Imports a directory or `.tar.gz` into a catalog (see "Import") |
| `export NAME OUT.tar.gz` | Writes the export tarball |
| `key create ID --access read\|write\|admin --catalogs pg,work\|--all [--default pg] [--actor A]` | Generates a random key, stores its SHA-256, prints the key once |
| `key list`, `key revoke ID`, `key grant ID CATALOG`, `key ungrant ID CATALOG` | Manage keys (`grant` and `ungrant` refuse a key created with `--all`) |

Every subcommand that touches the database applies the embedded migrations first
(idempotent); `migrate` exists for deploy hooks that want them as a separate step.

The subcommands talk to the database directly (`DATABASE_URL`), so in the cluster they
run with `kubectl exec`. They need no API key: database access is the authority.

### Configuration

A TOML file (k8s ConfigMap), overridable with `clap` flags and environment variables:

```toml
listen = "0.0.0.0:8080"
allowed_hosts = ["okf-mcp.example"]
default_catalog = "pg"
session_idle_timeout = "2h"
time_server = "mosaic-x5-3819907.internal"
poll_interval = "10s"
max_import_bytes = "64MiB"   # uncompressed total of an import
```

Each milestone reads only its own fields (M3: `max_import_bytes`; M4: the rest except
`time_server`, which M7's clock check reads); unknown keys are ignored, so one file
serves every version. A zero `poll_interval` or `session_idle_timeout`, or an empty
`allowed_hosts` (which rmcp would read as "accept any host") or a blank entry in it, is
refused at load.

Catalogs and keys are not in the config; they are rows. The only secret is
`DATABASE_URL`, from an environment variable backed by a k8s Secret (CNPG generates
it). Raw API keys are never stored or logged; incoming keys are hashed and looked up by
hash. A timing difference in the lookup can at most hint at how a guess's *hash*
compares with stored hashes, which does not help recover a key (SHA-256 is
preimage-resistant, and `std`'s `HashMap` hasher is randomly keyed), so there is no
constant-time scan.

### Storage

One Postgres database. Migrations live in `migrations/` and are embedded with
`sqlx::migrate!`.

| Table | Holds |
|---|---|
| `catalogs` | `id`, `name` (unique, `[a-z0-9-]+`), `description`, `generation` (bumped by every publish and import), `created_at` |
| `documents` | One row per file: `catalog_id`, `path` (catalog-relative, `/`-separated), `content` (`bytea`, the file's exact bytes), `executable` (the file's execute bit, so scripts round-trip), `revision` (+1 per change, never reused for a path), `deleted_at` (a tombstone: a deleted document keeps its row, so a path created again continues its revision numbering), `updated_at`, `updated_by`; derived `kind`, `concept_type`, `title`, `status`, `tags`. Primary key `(catalog_id, path)` |
| `revisions` | Append-only history: `catalog_id`, `path`, `revision`, `op` (`create`, `update`, `delete`, `import`), `content` (NULL for a delete), `actor`, `changeset_id`, `message`, `created_at` |
| `changesets` | One open per (catalog, key, label): `id`, `catalog_id`, `label`, `key_id`, `actor`, `state` (`open`, `published`, `discarded`), `version` (bumped by every change to it or its entries; overlay caches key on it), `created_at`, `updated_at` |
| `changeset_entries` | Per changed path: `changeset_id`, `path`, `base_revision` (NULL for a path with no live document), `content` (NULL for a delete), `replay` (the entry's listing and log operations, JSON, while every change to the entry came from one; NULL once any other edit touches it) |
| `api_keys` | `id`, `sha256`, `access`, `all_catalogs`, `default_catalog_id` (references `catalogs`; cleared when that catalog is deleted, or when the key loses its grant), `actor`, `created_at`, `revoked_at` |
| `key_catalogs` | `(key_id, catalog_id)` grants |
| `server_state` | One row: `admin_generation`, bumped by every `catalog` and `key` subcommand |

Constraints the design relies on: `revisions` is unique on `(catalog_id, path,
revision)`; `changesets` has a partial unique index on `(catalog_id, key_id, label)
WHERE state = 'open'`, so a racing get-or-create cannot open two changesets for one
identity; `api_keys.sha256` is unique and indexed. Every table that refers to a catalog
does so with `ON DELETE CASCADE`.

**Lock order.** Every transaction that writes changesets or entries takes the catalog
row `FOR KEY SHARE` first, then the changeset row (the `UPDATE` that bumps `version`),
then entries. `catalog delete` takes the catalog's advisory publish lock
(`catalogs::lock`), then the catalog row `FOR UPDATE`, then the open changesets in id
order; publish (M5 Task 10) takes the in-process mutex, the same advisory lock, then the
catalog row and the changeset. Plain writes never take the advisory lock, so no order is
reversed: writes and deletes wait for each other instead of deadlocking; a write that
waited finds the catalog gone and writes nothing. The 24-hour prune takes no catalog lock
and skips changesets other transactions hold (`SKIP LOCKED`). Deleting a key sets its
changesets' `key_id` to NULL without touching the catalog row; writes never change
`key_id`, and the insert's `ON CONFLICT DO NOTHING` fires no foreign-key check, so this
forms no cycle either. A deadlock or
serialization failure Postgres still reports (`40P01`, `40001`) is the `Busy` error,
which says to retry.

Every file of a catalog is a document: concepts, `index.md`, `log.md`, `AGENTS.md`, and
non-markdown files such as `tools/okf.py`. Content is stored as bytes (`bytea`, since
Postgres `text` cannot hold NUL), so any file round-trips exactly. A file that is valid
UTF-8 is text (NUL bytes included: content is `bytea`): it is loaded into the snapshot,
scanned for secrets and, for markdown, analyzed. Any other file (an image, say) is
stored and exported but not analyzed, as `okf.py`'s secret walk skips files it can't
read; a markdown file that is not text is an import error. The derived columns are
recomputed from the content on every write, so they can never disagree with it.

### In-memory snapshot

Each catalog holds an `RwLock<Arc<Snapshot>>` built from its `documents` rows. A
snapshot holds every document's parsed frontmatter, body, headings and anchors, the
link graph (outbound and inbound), the search fields, the secret findings and, built
lazily once, the unlinked-mention matcher. It is rebuilt off the reactor
(`spawn_blocking`, `rayon` across documents) after each publish or import, and swapped
in atomically. Readers clone the `Arc` and never wait on the database.

The snapshot is built through M1's `Workspace` trait over the rows held in memory, so
`Snapshot::build` and every analysis are unchanged. The in-memory workspace, test-only
in M2, becomes production code. It holds every file's bytes and execute bit, so a link
to a non-text file resolves, reading such a file as text fails as it does on disk, and
a write through the workspace keeps an existing file's execute bit (a new file is not
executable).

A session with an open changeset for a catalog reads that catalog through an overlay:
the published snapshot plus its changeset entries, rebuilt lazily after the session's
writes, so an agent sees its own pending edits.

The server runs one replica. It notices changes made outside it (a CLI import) through
the catalog's `generation` counter (see "Publish"). A second replica would need the same
check before every read, or Postgres `LISTEN`/`NOTIFY`; that is not planned.

### Crate layout

A library crate with a thin `main.rs`:

| Module | Responsibility |
|---|---|
| `config` | Load, merge (file, env, flags) and validate configuration |
| `db` | Pool, migrations, and the typed queries for every table |
| `store` | Catalog store over `db`: load a catalog's documents, publish a changeset in one transaction, record revisions, import, export |
| `auth` | Bearer-key middleware; inserts a `Principal { key_id, catalogs, access, default_catalog, actor }` into request extensions |
| `selection` | Resolve the catalog set and write target from tool argument, header/query, key default, server default |
| `catalog` | Per-catalog state: the `RwLock<Arc<Snapshot>>`, rebuilds, session overlays, commit times for lint |
| `okf::yaml`, `okf::doc`, `okf::markdown`, `okf::paths`, `okf::pystr`, `okf::timestamp` | M1 and M2: the document model, unchanged |
| `okf::ops` | `stamp`, `review`, `question`, `unquestion`, `source`, `log`, `index`, `create`, `edit`, `replace_body`, `set_frontmatter`, `mv`, `split`, `delete`, as pure functions over a `Workspace` |
| `okf::snapshot`, `okf::analysis`, `okf::search` | M2: the snapshot, links report, `oldest`, `sprawl`, `lint`, secret scan, search |
| `okf::workspace` | `Workspace` trait, the disk implementation (import and parity tests) and the in-memory one (snapshots, overlays, tests) |
| `archive` | Tarball reading (import) and writing (export) |
| `mcp` | rmcp `ServerHandler` and `#[tool]` definitions; thin delegation to the modules above |
| `clock` | `Utc::now()` source (injectable for tests) and the NTP offset check |

`okf::*` is pure logic over `Workspace` and a clock, so it needs neither the database
nor disk in tests.

## Tools

All tools take an optional `catalog` (`search` takes `catalogs`). Results are structured
JSON (rmcp `Json<T>`, `schemars` schemas). Concept paths accept
`/infra/time-server.md`, `infra/time-server` or `/infra/time-server`, as `okf.py` does.

Each result's `structuredContent` is the JSON, and its one text block holds the same JSON
(rmcp's `Json<T>`), rather than a hand-written summary. Errors are results with
`isError: true` and a body `{"error": <variant>, "message": <text>}` plus the `path`,
`catalog` or `problems` they concern; an internal error (database, migration,
filesystem or configuration: the ones the HTTP routes answer with 500) says only that the
log has the details. Without a `catalog` argument, `get_concept`, `read_document` and `history` look
through the selected catalogs in order and answer from the first that has the path, naming
it; the reports (`open_questions`, `oldest`, `sprawl`, `links_report`, `lint`) run on the
first selected catalog.

### Read tools (need `read`)

| Tool | Parameters | Result |
|---|---|---|
| `list_catalogs` | none | Catalogs the key can reach (name, description, file count, last change); this connection's selection and default write target, or why there is none |
| `search` | `query`, `catalogs?`, `type?`, `tags?`, `status?`, `limit?` (default 20, at most 200) | Ranked hits across catalogs: catalog, path, title, description, snippet. Every whitespace-separated term must occur (case-insensitive substring) in some field; the tier is the best field holding the whole query, else the best holding a term (fields, best first: title/alias, tag, description, body); deprecated concepts last; ties by catalog order, then path. The `tags` filter matches whole tags only, while free text matches tags by substring: the tool description must say so |
| `get_concept` | `path`, `links?`, `raw?` | Frontmatter as JSON and the body; with `links`, resolved outbound links (path, title, description, broken flag) and inbound backlinks; with `raw`, the file verbatim |
| `read_document` | `path?` (default `/index.md`) | Raw text of an `index.md`, `log.md` or `AGENTS.md` |
| `open_questions` | none | Every queued `open_questions` entry, grouped by concept, 1-based |
| `oldest` | `n?` (default 5), `open_questions_only?` | Port of `oldest --json` |
| `sprawl` | `path?`, `n?` (default 5) | Port of `sprawl --json` |
| `links_report` | `mentions?` (default true) | Port of `links --json` |
| `lint` | `pending?` | Port of `lint --json`, on the published catalog or (with `pending`) on this session's overlay |
| `history` | `path`, `limit?` (default 20, at most 200) | The document's revisions, newest first: revision, op, actor, message, time |
| `pending_changes` | none | This session's changeset in the catalog: changed paths and a unified diff against the published text |
| `stranded_sessions` | none | Open changesets whose MCP session has ended: id, catalog, actor, last change, changed paths |

Lint's `generated-after-commit` check reads each document's last revision time where
`okf.py` reads its last git commit time; paths in the session's changeset count as
uncommitted.

### Write tools (need `write`)

Each acts on this session's changeset for the target catalog, creating it on first use.
Nothing is visible to other sessions until `publish`.

| Tool | Parameters | Behavior |
|---|---|---|
| `create_concept` | `path`, `frontmatter` (object), `body`, `index_summary?`, `index_section?`, `by?` | Validates the path (`new_concept_path` rules: inside the catalog, not reserved, not machinery, must not exist; warns on a non-kebab-case name). Requires `type`. Writes `generated: { by, at: now }` itself and rejects `generated`, `verified`, `reviewed` in `frontmatter`. Renders frontmatter in `KEY_ORDER`. Other keys follow in the caller's order. Strings are quoted only when they must be; mappings and lists of scalars are written in flow style, lists of collections as block lists; JSON numbers and booleans are stored as their text. With `index_summary`, lists it in the folder's `index.md`, creating folder indexes and parent listings as `ensure_index` does, and lists it with its `title`; a summary without a title is refused. Index edits are recorded as replay operations, the concept itself as a free edit |
| `edit_document` | `path`, `old_str`, `new_str`, `replace_all?` | Exact replacement in a concept body or an `index.md`. Without `replace_all`, `old_str` must occur exactly once (`EditMismatch` gives the count); an empty `old_str` is refused. Refuses a match that touches frontmatter. Refuses `log.md` and machinery |
| `replace_body` | `path`, `body` | Replaces a concept's body; frontmatter unchanged byte for byte |
| `set_frontmatter` | `path`, `key`, `value` (JSON, or null to delete) | Sets or deletes a descriptive key with a textual edit. Refuses `generated`, `verified`, `reviewed`, `open_questions`, `sources` (dedicated tools) and refuses deleting `type`. Values render as in `create_concept`. A document without frontmatter is refused with `okf.py`'s 'has no frontmatter; add it with your editor first' |
| `stamp` | `path`, `field` (`generated` or `verified`), `by?` | Port of `stamp` |
| `review` | `path`, `outcome`, `note`, `questions?`, `by?` | Port of `review` |
| `question` | `path`, `question` | Port of `question` |
| `unquestion` | `path`, `question` (1-based index or exact text) | Port of `unquestion` |
| `add_source` | `path`, `id`, `resource`, `title?`, `author?` | Port of `source` |
| `log` | `text`, `kind?` (`Update`, `Creation`, `Change`, `Import`) | Port of `log`. The only way to write `log.md`. Replayable |
| `index` | `path`, `summary`, `section?`, `title?` | Port of `index`. Replayable |
| `move_concept` | `old`, `new` | Port of `mv`; returns touched files |
| `split_concept` | `path`, `section`, `to`, `type?`, `title?`, `description?`, `stub?`, `sources?`, `by?` | Port of `split`; returns touched files and notes |
| `delete_concept` | `path` | Removes the concept and its listing in its folder's `index.md`. Refuses while any other document links to it, and lists those links |
| `publish` | `message` | See "Publish" below |
| `discard` | `paths?` | Drops this session's changeset entries (all, or the named paths) |
| `revert` | `path`, `revision` | Adds the document's content at `revision` (or its deletion) to this session's changeset, for review and publish like any other edit. Subject to every write rule, so machinery cannot be reverted |
| `adopt_changeset` | `id` | Moves a stranded changeset created with this same key into this session; an `admin` key may adopt any stranded changeset in its granted catalogs |

`by` defaults to the key's `actor`. A call may override it (for example `human:pgrace`
when a person really did the verifying); AGENTS.md's honesty rule still applies.

### Rules enforced on every write, before anything is stored

- **Secret scan** (port of `SECRET_RULES`, `ASSIGN_RE`, `_literal_secret`) on the new
  file content; any hit refuses the write and reports the rule and line, with the scan's
  message, which shows the value masked to its first four characters. The scan reads the
  text with universal newlines, as `lint` does, so both report the same line.
- **Frontmatter still parses** after the edit (as `Doc.save` checks).
- **Paths stay inside the catalog**: normalized, no `..`, no absolute paths.
- **Machinery is read-only** through every tool, `revert` included (`okf::rules::is_machinery`):
  `AGENTS.md`, `CLAUDE.md` and `README.md` at the root; the root's `tools/`, `docs/` and
  `tests/` (the template's own development docs and tests); `.claude/`, `install/` (the
  template's setup machinery) and `.git` at any depth, and any other `.git*` component. Only
  import writes them. (`install/`, `docs/` and `tests/`: M5 Decision 4, matching `okf.py`,
  which skips them as non-catalog content since 481357b.) Names match case-sensitively, as
  in `okf.py` and on the case-sensitive store: `agents.md` or `.GIT/` is not machinery. An
  export unpacked onto a case-insensitive filesystem (macOS, Windows) could let such a file
  collide with the real `AGENTS.md`; the server itself never sees that collision.
- **Timestamps come from the server clock** and no tool accepts one.

## Data flow

### Read

1. `auth` hashes the bearer key, looks it up (keys are cached in memory and reloaded
   when `admin_generation` changes; see "Noticing changes made outside the server"),
   and inserts a `Principal`; failures are HTTP 401 (missing or unknown
   key) or 403 (no access to the route).
2. rmcp inserts the HTTP request's `http::request::Parts`, including the extensions the
   auth layer added, into every tool call's request context (verified against rmcp
   3.8.0 while planning M3).
3. `selection` resolves the catalog set.
4. For each catalog, the handler takes an `Arc` of the current snapshot, or of the
   session's overlay if it has an open changeset.
5. The work runs in memory (search uses `rayon` across documents, then merges and ranks
   across catalogs).

### Write

1. Auth, `write` access check, single target catalog.
2. `store` gets or creates the open changeset for (session, catalog).
3. The `okf::ops` function reads through the overlay `Workspace`, computes the new
   content, runs the write rules, and the changed files become changeset entries, each
   with the published revision it was based on. Edits to `log.md` and `index.md` that
   come from bookkeeping are also recorded as operations in the entry's `replay`:
   `log` adds a log entry; `index`, `create_concept` (with `index_summary`) and the
   listing changes of `move_concept`, `split_concept` and `delete_concept` add, rename
   or remove an `index.md` bullet. The operations are `Log { date, bullet }`,
   `ListBullet { target, bullet, section, create_section }` and `CreateIndex { folder }`
   (`okf::ops::ReplayOp`). They are stored as JSON with an `op` tag and re-applied by
   `okf::ops::replay`, which shares its text edits with the ops themselves. Text that
   no operation changes keeps its bytes; changed text gets `\n` line endings, as
   `okf.py` writes. A free-text edit of an `index.md` (`edit_document`) clears the
   entry's `replay`, so that entry conflicts like any other on a stale base.
4. The session's overlay is marked stale.

### Publish

Every write to a catalog's published state (publish, import) runs under that catalog's
**publish lock**: an in-process mutex, plus a Postgres advisory lock
(`pg_advisory_xact_lock` on the catalog id) held by the transaction, so a CLI import
running beside the server is serialized too. Publishes to one catalog therefore run one
at a time, each against the current published state.

1. Nothing changed: return "nothing to publish".
2. Take the publish lock and begin the transaction. If the catalog's `generation` (a
   counter bumped by every publish and import) differs from the in-memory snapshot's,
   rebuild the snapshot first, so the checks below see the current state.
3. Check each entry against the current document:
   - an entry with a `replay` whose base is stale (the base revision is not the current
     one, or the base is NULL and the path now has a live document, such as a new
     folder's `index.md` that another session created first) has its operations
     re-applied to the current content, as git's `union` merge did for concurrent
     `log.md` lines. The operations are idempotent where that matters (creating a
     folder index that exists, listing a concept already listed);
   - any other entry whose base is stale (including a NULL base on a path that now has
     a live document) is a conflict: roll back and return `Conflict { files }` with
     each file's base and current revision. The changeset stays open; the agent
     re-reads, re-applies and publishes again.
4. Build the overlay (the current snapshot plus the entries, replays applied) and run
   the checks on it:
   - the write rules (secret scan, frontmatter parses) on every replayed result;
   - lint: refuse if any **changed** file has errors; errors elsewhere are returned as
     warnings (as `okf.py publish` does);
   - links into deleted or moved paths: refuse if any document outside the changeset
     still links to a path this changeset deletes or moves away from. This catches a
     link another session published after this session's `delete_concept` check ran.
   A failure rolls back and is returned like any other refusal.
5. Write each document (revision + 1), a `revisions` row carrying the message and actor,
   bump `generation`, mark the changeset `published`, and commit.
6. Still holding the in-process lock (the advisory lock ended with the transaction),
   rebuild and swap the catalog snapshot. Return the new revisions and changed paths.

The checks run while the lock is held, so they stay small: the overlay shares the
current snapshot's documents and re-reads only the changed ones (about 10 ms for a full
snapshot plus lint of the real catalog in M2's smoke test). Reads never take the lock.
The unique constraints are a backstop: a violation still maps to `Conflict`, never to
the opaque `Database` error.

### Noticing changes made outside the server

The subcommands write to the database directly, beside a running server. Two counters
let the server notice:

- each catalog's `generation`, bumped by every publish and import;
- `admin_generation`, a single-row counter (`server_state`) bumped in the same
  transaction as every `catalog` and `key` subcommand (create, delete, revoke, grant,
  ungrant).

Every `poll_interval` (default 10 s), the server reads both. A changed
`admin_generation` reloads the key cache and the catalog list (loading new catalogs,
dropping deleted ones); a changed `generation` rebuilds that catalog's snapshot. So a
revoked key or ungranted catalog stops working within `poll_interval`, and a CLI import
or new catalog is visible within the same bound, with no restart. Publish also checks
its catalog's `generation` under the lock (step 2), so it never acts on a stale
snapshot.

### Import

`okf-mcp import NAME PATH` or `POST /catalogs/{name}/import`:

1. Read every file of the directory or tarball, with its execute bit. From a
   directory, `.git` (a directory, or the link file a git worktree has), `.worktrees/`
   and `__pycache__/` are skipped, symlinks follow M1's listing contract (inside the
   catalog only), and every other file is kept, dotfiles included (a catalog tracks `.claude/skills/`, `.gitattributes` and
   `.okf-catalog.json`). A tarball's paths are taken as they are, with no prefix
   stripped, so `git archive --format=tar.gz HEAD` on a checkout imports exactly its
   tracked files (the user runs git, the server never does), and so does an export.
   The whole import is refused, listing every offending entry, if the tarball has an
   entry that is a symlink, hardlink, device, FIFO or anything but a regular file or a
   directory; a path with a `..` component, a leading `/`, a non-UTF-8 name or a form
   that `normpath` would change (`./a`, `a//b`); or two entries with the same path. The
   uncompressed total is counted while reading and the import stops once it passes
   `max_import_bytes`, so a small compressed upload cannot expand without bound (the
   HTTP route also caps the request body at the same size).
2. Build a snapshot of it and lint it; report the findings (import does not refuse on
   lint, since it brings in what exists).
3. In one transaction: into an empty catalog, insert every file; with `--replace`,
   write the differences as `import` revisions (unchanged files keep their revision;
   missing ones are deleted, with `delete` revisions whose message is `import`).
   Without `--replace`, importing into a non-empty catalog is refused.
4. Rebuild the snapshot.

### Export

`GET /catalogs/{name}/export` or `okf-mcp export NAME OUT.tar.gz` streams a `.tar.gz`
of the published documents at the archive root (no top-level directory, as `git
archive` writes them), file modes 0755 for executable documents and 0644 otherwise,
mtimes set to each document's `updated_at`. Importing an export into an empty catalog
reproduces every file's content and mode; history and revision numbers are not
exported.

### Session lifecycle

- A changeset is created on the first write by a (key, label) to a catalog (see "Write
  grouping"), and stays open until `publish` or `discard`, across sessions and restarts:
  closing an MCP session strands nothing, and a client on MCP `2026-07-28` (no sessions)
  gets the same changeset on every request through its label.
- `stranded_sessions` lists open, non-empty changesets not changed within
  `session_idle_timeout`: the key's own, or for an `admin` key every one in the catalogs
  it reaches. `adopt_changeset` moves one to the calling connection's key and label (the
  key's own with `write`, another key's with `admin`).
- The poll loop discards empty changesets whose last change is more than 24 hours old.
  Non-empty ones are never pruned: they hold unpublished work, and only `discard` (after
  `adopt_changeset`, if need be) or `catalog delete --force` removes them.

## Errors

One `thiserror` enum, `OkfError`:

| Variant | Meaning |
|---|---|
| `NotFound { path }` | No such concept or document |
| `Forbidden { reason }` | Key lacks access to the catalog or operation |
| `CatalogNotSelected { catalog }` | Named catalog does not exist or is not granted |
| `InvalidPath { path, reason }` | Outside the catalog, reserved, machinery, already exists |
| `FrontmatterParse { path, line, message }` | Existing or edited frontmatter does not parse |
| `SecretDetected { path, line, rule, message }` | Write refused by the secret scan; `message` shows the value masked |
| `EditMismatch { path, matches }` | `old_str` found 0 or more than 1 times |
| `LintFailed { errors, warnings }` | Publish refused; findings as structured lists |
| `Conflict { files }` | Publish found documents changed since this session read them |
| `Busy` | The database aborted the request to resolve a lock conflict, or the changeset closed under a write; nothing was stored; retry (HTTP 503) |
| `Database` | Any database failure (details logged, not returned) |
| `Io` | Filesystem errors (import, export) |

The ported `okf.py` commands (`stamp`, `review`, `question`, `unquestion`, `add_source`,
`log`, `index`) refuse with `okf.py`'s own words: its `die(msg)` is `Refused { reason: msg }`,
except where a variant above already names the problem (`NotFound`, `InvalidPath`,
`FrontmatterParse`, and `Shape` for an `open_questions` or `sources` that is not a list).

Mapping:

- Auth failures are HTTP 401 or 403 before MCP is involved.
- Domain errors are MCP tool results with `is_error: true` and a structured body, so the
  agent can correct itself.
- Protocol-level JSON-RPC errors are only for malformed calls.

Logging uses `tracing`. Never logged: API keys, `DATABASE_URL`, or matched secret text.

## Time

All timestamps come from `chrono::Utc::now()` through the `clock` module (formatted
`%Y-%m-%dT%H:%M:%SZ`). At startup and every 15 minutes, the server queries the GPS
stratum 1 time server (`time_server` in config) over NTP and logs at `error` if the
offset exceeds 2 seconds. `/healthz` reports the last offset. Lint's future-timestamp
check keeps `okf.py`'s 2-minute slack. Revision times are written by the server
(`Utc::now()`), not by Postgres's `now()`, so they follow the same clock.

## Testing

- **Unit tests** in `#[cfg(test)]` modules for every function, against the in-memory
  `Workspace` and an injected clock: YAML parsing and rendering edge cases, byte
  preservation, slugs and repeat numbering, footnote handling, `insert_index_bullet`
  placement, `log` heading insertion, secret rules, sprawl scoring, path validation,
  replay of `log` and `index` operations onto changed text.
- **Property tests** (Hegel): render then parse returns the same values; textual edits
  change only the target key's lines; any frontmatter the server writes parses;
  replaying `log` operations onto any interleaving keeps every entry.
- **Differential parity tests against `okf.py`** (M1 and M2): a committed synthetic
  corpus with golden outputs, and the real catalog from the local checkout named by
  `OKF_PARITY_CATALOG`, never copied into this repository.
- **Database tests** against a throwaway Postgres started by `testcontainers` (the
  machine has Docker): migrations, publish with and without conflicts, concurrent
  publishes of `log.md`, import then export round trip byte for byte (including the
  real catalog when `OKF_PARITY_CATALOG` is set), key grants, changeset survival across
  a restart.
- **End-to-end tests** of the HTTP server with rmcp's client: missing, wrong and
  read-only keys; catalog precedence (argument over header over key over server
  default); a full create, link, index, log, publish cycle; export of the result.

## Deployment

- Multi-stage Dockerfile: Rust build stage, then a slim runtime image with CA
  certificates (no git). Pushed to Harbor (`r.gfpd.us`), as the cluster's Kyverno policy
  expects.
- Manifests in Argo CD's source repository (`infrastructure/argo2025`): one-replica
  Deployment, a CNPG database (a new cluster or a database in an existing one, chosen at
  deployment), ConfigMap, the CNPG-generated Secret for `DATABASE_URL`, Service, and an
  ingress-nginx Ingress with a cert-manager certificate. No PVC.
- Backups are CNPG's.

## Milestones

| Milestone | Delivers |
|---|---|
| M1. Core library (delivered) | `okf::yaml`, `okf::doc`, `okf::markdown`, `okf::workspace`, `clock`; the parity test harness ([plan](../plans/2026-10-26-okf-mcp-m1-core-library.md)) |
| M2. Analysis (delivered) | `lint`, links report, `oldest`, `sprawl`, secret scan; snapshot and search; `okf::timestamp` ([plan](../plans/2026-10-27-okf-mcp-m2-analysis.md); PRs #17-#30) |
| M3. Storage (delivered) | `config`, `db` and migrations, `store`, `archive`; `catalog` with snapshot swap and the cached mention matcher; import and export subcommands; `catalog` and `key` subcommands; revision-based commit times for lint. Export then import reproduces the real catalog's file contents and modes byte for byte ([plan](../plans/2026-11-02-okf-mcp-m3-storage.md); PRs #48, #49 and #51-#61) |
| M4. Read-only server (delivered) | axum, rmcp, auth, `selection`, the read tools that need no changeset (including `history`), the import and export routes, `/healthz`, the poll loop, `okf-mcp serve`. Deployable and useful on its own ([plan](../plans/2026-11-02-okf-mcp-m4-read-only-server.md); PRs #76-#86) |
| M5. Writes | Changesets, overlays, `okf::ops`, the content and bookkeeping write tools, `publish` with conflicts and replay, `discard`, `revert`, `pending_changes`, `stranded_sessions`, `adopt_changeset`, `lint pending` |
| M6. Restructuring | `move_concept`, `split_concept`, `delete_concept` |
| M7. Deployment | Dockerfile, k8s manifests, CNPG database, NTP check, operator docs |

Per the project's `CLAUDE.md`, each iteration adds a dated note under `changelog/`.

CI is deferred (pgrace, 2026-11-03): the gates (`cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, `cargo test`, `cargo doc` with `-D warnings`, live parity,
the real-catalog round trip and, at a milestone's end, the release smoke test) run
locally before each merge.

### What M2 shipped differently

The M2 plan's "Simplifications and deviations relative to the design" has the full list;
the ones that change this design:

- The snapshot is `okf::snapshot`, not part of `catalog` (see "In-memory snapshot"), and
  search is its own module, `okf::search`.
- The secret scan runs on every file once, inside `Snapshot::build`, so `lint` reads the
  findings rather than walking the catalog again. `tools/okf.py` is skipped, as `okf.py`
  skips itself.
- `lint` takes commit times as an input (`Option<&CommitTimes>`); M3 fills it from
  revisions.
- Parity compares parsed JSON values, not bytes: `json.dumps` escapes non-ASCII and
  serde_json writes it raw. Key order is checked separately.
- Known divergences from `okf.py`, kept on purpose and documented in the module that has
  each: a list or mapping `status` is reported with its `repr` (`okf.py`'s lint crashes);
  a naive timestamp in `oldest` is ignored, as is a fallback `at:` value that is not a
  date (`okf.py` crashes on both); ISO week dates and fractional-second offsets are
  rejected; a link escaping the catalog root is always broken; symlinks follow M1's
  listing contract, and non-UTF-8 file names are skipped; `resolve_concept` only finds
  snapshot documents (not a file in a dot-directory); the root `AGENTS.md`, `CLAUDE.md`
  and `README.md` are in the snapshot but outside the link graph. Sprawl scores match
  `okf.py` on Python 3.12 or later, whose float `sum()` is compensated.

### What M3 shipped differently

The M3 plan's "Simplifications and deviations relative to the design" and its dated
`Corrected` notes have the full list; the ones that change this design (most are
already written into the sections above):

- The in-memory workspace holds every file's bytes and execute bit itself, rather than
  the store keeping them beside a text-only workspace, so a link to an image resolves
  (see "In-memory snapshot"). `MemWorkspace::from_stored` refuses the trees a disk
  write can never produce: two paths that normalize to the same one, and a file that
  is also another file's directory (`a` beside `a/b.md`).
- Text means valid UTF-8; NUL no longer matters, since content is `bytea` (see
  "Storage").
- The configuration file holds only `max_import_bytes` until later milestones add
  their fields; unknown keys are ignored (see "Configuration").
- Every subcommand that touches the database runs the embedded migrations on connect;
  `migrate` stays for deploy hooks (see "Process and routes").
- `api_keys.default_catalog_id` references `catalogs` (`ON DELETE SET NULL`) instead of
  naming the catalog (see "Storage"). `key grant` and `key ungrant` refuse a key
  created with `--all`, whose grants are not per catalog.
- The schema indexes the foreign keys a catalog or key delete looks up, and
  `revisions`' key is its primary key (named `revisions_catalog_id_path_revision_key`)
  rather than a separate unique constraint.
- Import writes rows one at a time inside one transaction. On the real catalog (225
  files) the whole import, lint included, took 79 ms in release mode. It skips a file
  whose bytes and execute bit are unchanged, so with `--replace` an unchanged file keeps
  its revision and its derived columns; a later change to how the derived columns are
  computed must therefore ship a backfill, since re-importing identical bytes cannot
  repair them. After taking the catalog lock, import (like `catalog delete`) looks the
  catalog up again, so a concurrent delete is reported as an unknown catalog.
- A tarball import also refuses a file that is also another file's directory and a
  path holding a control character (a pax `path` record can carry a NUL, which
  Postgres `text` cannot store); names are shown escaped in the problem list. A
  directory import stays lenient on purpose (M1's listing contract, documented in
  `src/archive.rs`): a symlink inside the catalog is flattened to its target's bytes,
  and a dangling or outside symlink, a special file and a non-UTF-8 name are skipped
  with a warning, so `git archive --format=tar.gz HEAD | okf-mcp import NAME -` is the
  exact path.
- A file dropped by `import --replace` gets a `delete` revision with `message =
  'import'`, as step 3 of "Import" says; a tombstone row keeps an empty `content`.
- Export reads the catalog and its documents in one `REPEATABLE READ, READ ONLY`
  transaction, so a catalog deleted concurrently is either unknown to the export or
  exported whole as it was, never half-deleted.
  The CLI builds the archive in memory and writes a file through a temp file renamed
  into place, so an unknown catalog or a failed write leaves no truncated file.
- `CatalogState` builds its snapshot in a `REPEATABLE READ, READ ONLY` transaction and
  serializes rebuilds of one catalog with an async mutex: concurrent `refresh` calls
  build and swap once, so M4's poll loop and M5's publish need no lock of their own
  for it.
- `catalog delete` locks the catalog row and its open changesets `FOR UPDATE` after the
  advisory lock, so a concurrent changeset write cannot slip past the unpublished-work
  check. M5's write path must take `catalogs::lock` before touching changesets, or touch
  the catalog row (or create the changeset) before writing entries, or retry on
  SQLSTATE `40P01`, or it can deadlock with a delete (the M3 plan's note for M5).
- Lint's commit time is the document's last revision time, so after a first import
  every document's "commit" is the import time, and `generated-after-commit` cannot
  flag a stamp that falls between the real git commit and the import. The server has
  no git; this is the closest equivalent.
- The markdown walk and `new_concept_path` follow `okf.py` 481357b (the catalog
  template's adoption): `install/` is skipped at any depth and `docs/` and `tests/` at
  the catalog root, and `new_concept_path` refuses those first components as it does
  `tools/`. So `Snapshot::resolve_concept`, which sees only walked documents, does not
  find a file there; `paths::resolve_concept` checks the workspace and still does, as
  `okf.py`'s does. Import, export and the secret scan still see every file.
- Open question for M5: the read-only machinery list in "Rules enforced on every
  write" (`AGENTS.md`, `CLAUDE.md`, `README.md`, `tools/`, `.claude/`, `.git*`) does not
  include `install/` or the root `docs/` and `tests/`, which `okf.py` now skips (#50).
  `new_concept_path` already refuses a concept there; whether every write tool treats
  them as read-only machinery is decided with the write path. Decided in M5 (Decision 4):
  they are machinery, at the depths given in "Rules enforced on every write".

### What M4 shipped differently

The M4 plan's "Simplifications and deviations relative to the design" and its dated
`Corrected` notes have the full list; most are already written into the sections above.

Deviations planned:

- Keys are looked up by their SHA-256 in a `HashMap`, not compared in constant time
  (see "Configuration"); `keys::matches` stays for callers comparing one stored hash.
- A tool result's text block is the JSON itself (rmcp's `Json<T>`), not a hand-written
  summary (see "Tools").
- Report tools wrap `okf.py`'s JSON lists in an object (`oldest` gives `{"rows": [...]}`,
  `sprawl` `{"reports": [...]}`), so every output schema has an object root; the rows are
  unchanged.
- `open_questions` reuses `oldest` with `open_questions_only`, numbering each concept's
  questions; concepts come stalest first.
- The export is built in memory and returned with a `Content-Length`, not streamed (the
  real catalog's is about 450 KB, 39 ms through the route).
- The import route's body cap is `max_import_bytes` plus 0.1% and 64 KiB of gzip slack;
  the exact limit is enforced on the decompressed stream.
- The poll loop reads `admin_generation` and the catalog list (`id`, `name`,
  `generation`) every tick, so a new, deleted or re-created catalog shows up whether or
  not `admin_generation` moved; a moved `generation` triggers `CatalogState::refresh`.
  Passes are serialized: each `poll` (the loop's or a test's) holds one async mutex.
- A catalog that does not exist or that the key does not reach is 404 on the routes and
  `CatalogNotSelected` in tools, never 403, so a key cannot probe which catalogs exist;
  403 is only for a reachable catalog the key's level does not allow.
- No timeout on `/mcp` (see "Process and routes").

Plan corrections (each a dated `Corrected` note in the plan):

- **#65 (config):** `max_import_bytes`' doc names the route's cap with slack;
  `allowed_hosts` defaults through `Vec::from`; a blank `allowed_hosts` entry is refused
  at load (review of PR #76).
- **#66 (auth):** the code shipped as planned; its tests also cover a stored hash that is
  not 32 bytes, a revoked key, a `Basic` header, `WWW-Authenticate: Bearer` on every
  401, and a failed `reload` keeping the old cache.
- **#67 (selection):** an empty connection list counts as unset, so no `Selection` is
  empty; a header naming nothing falls back to `?catalogs=`; repeated headers and
  parameters are one list; a non-ASCII header is decoded as UTF-8 so a garbled name
  fails by name instead of silently falling back; the `Refused` message names
  `?catalogs=`.
- **#68 (app and poll loop):** a failed key reload or `admin_generation` read is logged
  and the catalog half of the pass goes on, so `poll` fails only on reading the catalog
  list; a generation whose stored data fails to build is remembered as broken and not
  rebuilt every tick (the old snapshot stays, or a never-loaded catalog stays out) until
  the generation moves, while database errors are retried; five more tests.
- **#69 (mcp core):** a `#[tool]` method must write `Result<Json<T>, CallToolResult>`,
  since rmcp's macro finds the output schema in the written return type and a type alias
  hides it; `ToolResult`, `tool_error` and `reply` are `pub`; `read_set` and `one` take
  the `catalog` argument as a one-element slice; `clippy::result_large_err` is allowed
  where a function returns `CallToolResult` as its error.
- **#70 (http and serve):** `AppError::status()` is public; a timed-out request gets 503,
  not 408, through `http::timeout_layer()`; every 500 (`is_internal` now covers `Io` and
  `Config`) is logged with its details and redacted in the body; Ctrl-C that cannot be
  registered no longer shuts the server down at once; requests are logged at INFO by
  default (`DEFAULT_LOG_FILTER`); `serve` cancels the shutdown token and awaits the poll
  loop on every return; `/healthz` has its own 2-second deadline around `SELECT 1` (pool
  acquire included). The crate is Unix-only (`tokio::signal::unix`).
- **#34 (search):** doc-comment line breaks stay in schemars descriptions, so
  `tags`' comment keeps "whole tags only" on one line; the merge sorts unstably on a total
  order; `limit` is capped at 200 (the `search` row above says so).
- **#71 (get_concept, read_document):** shipped as planned, with `Doc::body`'s doc test
  and more assertions (first catalog wins, refusals do not stop the search, a
  duplicate-key frontmatter gives null `frontmatter` with `frontmatter_error`), and a test
  that both tools list object output schemas.
- **#72 (history):** `limit` is capped at 200 and 0 counts as 1; the shared test helper
  `files` lives in `archive` (test-only); the tool description says `log.md`, indexes
  and other files have a history too.
- **#73 (reports):** shipped as planned; doc comments gained Arguments and Returns, and a
  test calls the `#[tool]` methods with request `Parts` to cover selection and path
  errors.
- **#74 (archive routes):** the routes shipped as planned, then review of PR #86 changed
  the import: the key is checked before the body is read (a key that may not import
  cannot make the server buffer an upload), the import is not behind `timeout_layer()`,
  only its upload is bounded (503 with the `Timeout` error JSON, nothing imported), and
  the import and refresh have no deadline. The export answers with
  `Content-Disposition: attachment; filename="<name>.tar.gz"`. A refresh that fails
  after a committed import is logged and the report still returned.

### Open questions for M5

- **Binding it to a key.** Whatever the identity, M5 binds it to the key that created it.
  In M4 each request is authorized by its own key, but a session's streams are not bound
  to the key that opened it: another valid key that learned an `Mcp-Session-Id` (a random
  UUID) could attach to the session's `GET /mcp` stream, or resume it with
  `Last-Event-ID`, and receive messages meant for the first key. M5 records the creating
  key with the session and refuses any other, before changesets depend on sessions.
- **Carried from M3** (see "What M3 shipped differently"): whether
  `install/` and the root `docs/` and `tests/` are read-only machinery for the write
  tools; and a backfill of the derived columns whenever `Derived::of` changes, since an
  import skips files whose bytes are unchanged.

### Carried into M3 and later

Raised in M2 reviews and filed as issues (label `milestone-3`):

- **#31 Commit times for lint** (done in M3, PR #61): `Loaded::lint` fills
  `CommitTimes` from each document's last revision time; marking the session's
  changeset paths dirty stays for M5. The doc comments that described the git plan
  (`src/okf/analysis/lint.rs` module docs and `CommitTimes::new`,
  `src/okf/workspace.rs`, `src/okf/mod.rs`) no longer describe it; `CommitTimes::new`
  names both sources (`okf.py`'s git, the server's revisions).
- **#32 Snapshot swapping** (done in M3, PR #58): `catalog::CatalogState` holds an
  `RwLock<Arc<Loaded>>`, rebuilt by `refresh` when the catalog's `generation` moves.
- **#33 Mention matcher cache** per snapshot (done in M3, PR #52):
  `Snapshot::mention_matcher` builds it once, lazily.
- **#34 `search` tool description** (done in M4, PR #81): the `tags` filter matches whole
  tags only, while free text matches tags by substring; the tool and parameter
  descriptions say so, and a test checks them.
- **#35 `test-util` for integration tests** (done in M3, PR #48): the in-memory
  workspace is production code and the feature is gone.
- From the M2 plan's "Deliberately deferred" table: cross-catalog search merge (done in
  M4, PR #81: `okf::search::search_catalogs`, best tier first, then catalog order, then
  path); secret refusal on write (M5 Task 5); the no-frontmatter `set_block` error (done in M5 Task 4: `set_frontmatter` refuses such a document). The per-session
  write lock, `core.symlinks=false` clones and `.okf-*.tmp` handling before `git add -A`
  no longer apply: there is no git and no on-disk worktree.

## Out of scope

- Handing the existing git catalogs and the Claude Code setup (the global
  `~/.claude/CLAUDE.md`, hooks, `okf-*` skills, AGENTS.md) over to the server. The server
  is first tested from other harnesses; the cutover is decided once it is complete.
- Removing `okf.py` from the catalog and the catalog template.
- An automated mirror of a catalog to git. Export on request covers it.
- Admin MCP tools (catalog and key management stay subcommands and routes).
- MCP resources and prompts (tools only for now).
- More than one replica.
