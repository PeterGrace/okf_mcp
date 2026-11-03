# okf_mcp

A Rust port of `tools/okf.py`, the Open Knowledge Format catalog CLI, on its
way to becoming a shared MCP server (design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`).

Status: milestones 1 (core library), 2 (analysis), 3 (storage) and 4
(read-only server) are delivered. The library reads a catalog into an
in-memory `Snapshot` and runs `okf.py`'s analyses on it with the same JSON
output (compared as parsed values): `lint`, the links report, `oldest`,
`sprawl` and the secret scan, plus ranked `search`, which `okf.py` does not
have. Catalogs live in Postgres: every file is a row holding its exact bytes
and execute bit, with an append-only revision history, and each catalog's
snapshot is built from its rows and swapped when the catalog changes. The
`okf-mcp` command line administers catalogs and API keys and imports and
exports catalogs, and `okf-mcp serve` serves the catalogs over MCP
streamable HTTP with bearer-key auth: every read tool, the export and import
routes and `/healthz`. Writes (changesets and publish) arrive in milestone 5.

## Command line

    okf-mcp migrate
    okf-mcp catalog create pg --description "pgrace's catalog"
    git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD | okf-mcp import pg -
    okf-mcp key create laptop --access write --catalogs pg --default pg --actor claude-code/claude-opus-5-5
    okf-mcp export pg pg.tar.gz

Every subcommand reads the database URL from `DATABASE_URL` (or
`--database-url`) and the optional TOML configuration file from
`OKF_MCP_CONFIG` (or `--config`); its fields are listed under "Running the
server" (the subcommands read only `max_import_bytes`, default `64MiB`, the
largest uncompressed import, but a file that fails validation fails every
subcommand). Every subcommand applies the embedded migrations first, so
`migrate` is only needed as a separate deploy step. `okf-mcp --help` and
`okf-mcp <command> --help` list the rest: `catalog list|delete`,
`key list|revoke|grant|ungrant`, `import --replace`.

`import` takes a `.tar.gz` file, `-` for one on stdin, or a directory. A
`git archive` tarball imports exactly the checkout's tracked files, and a
tarball breaking the import rules (a symlink, a `..` path, a duplicate, ...)
is refused with every problem listed. A directory is read leniently, through
the same listing as the analyses: untracked files are included, and a
symlink, special file or non-UTF-8 name it cannot store is flattened or
skipped with a warning (see `src/archive.rs`). `key create` prints the new key
once; only its SHA-256 is stored.

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
session_idle_timeout = "2h"        # an idle MCP session is closed after this
max_import_bytes = "64MiB"         # uncompressed total of an import
```

A zero `poll_interval` or `session_idle_timeout`, an empty `allowed_hosts`
or a blank entry in it is refused at load. Behind an ingress,
`allowed_hosts` must name the public hostname (rmcp's DNS-rebinding check
answers any other `Host` on `/mcp` with 403).

Create a catalog, fill it and make a key with the subcommands; the server
notices each change within `poll_interval`, without a restart (a revoked key
stops working at the next poll too):

    okf-mcp catalog create pg --description "pgrace's catalog"
    git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD | okf-mcp import pg -
    okf-mcp key create laptop --access read --catalogs pg --default pg

`key create` prints the key once. Keep it in an environment variable, here
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

Every tool needs a `read` key. Results are JSON (`structuredContent`, and the
same JSON as the text block); errors are results with `isError: true` and
`{"error": ..., "message": ...}`.

| Tool | Does |
|---|---|
| `list_catalogs` | The catalogs the key reaches, and this connection's selection |
| `search` | Ranked search across the selected catalogs (`query`, `catalogs`, `type`, `tags`, `status`, `limit` up to 200) |
| `get_concept` | A concept's frontmatter and body; with `links`, its links and backlinks; with `raw`, the file |
| `read_document` | The text of an `index.md`, `log.md` or `AGENTS.md` (default `/index.md`) |
| `history` | A document's revisions, newest first (`limit` up to 200) |
| `open_questions` | Every queued `open_questions` entry, by concept |
| `oldest` | `okf.py oldest --json` |
| `sprawl` | `okf.py sprawl --json` |
| `links_report` | `okf.py links --json` (`mentions`, default true) |
| `lint` | `okf.py lint --json` on the published catalog |

`get_concept`, `read_document` and `history` without `catalog` answer from
the first selected catalog that has the path; the reports run on the first
selected catalog.

### Routes

| Route | Key | Does |
|---|---|---|
| `GET /healthz` | none | `{"database": "ok", "catalogs": N}`: 200, or 503 when the database does not answer within 2 s |
| `POST/GET/DELETE /mcp` | any | MCP streamable HTTP |
| `GET /catalogs/{name}/export` | `read` on the catalog | The catalog as `application/gzip`, `Content-Disposition: attachment; filename="<name>.tar.gz"` |
| `POST /catalogs/{name}/import[?replace=true]` | `admin` on the catalog | Imports the uploaded `.tar.gz` and answers with the import report |

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
for a non-empty catalog without `?replace=true` (the message says to add it).
A body over `max_import_bytes` plus a little gzip slack is 413, axum's
plain-text rejection rather than the error JSON. Only the upload is bounded:
if reading and checking it takes more than 120 s the answer is 503 with
`{"error": "Timeout", ...}` and nothing is imported; the import and the
snapshot refresh after it have no deadline, so a committed import always gets
its report, and the next read sees the new snapshot. `/healthz` and the export
time out after 120 s with 503; `/mcp` has no timeout, since its `GET` is a
long-lived event stream.

## Layout

- `src/okf/` - the document model (`yaml`, `doc`, `markdown`, `paths`,
  `workspace`, `timestamp`), the `snapshot`, `analysis::*` (`links`, `lint`,
  `oldest`, `sprawl`, `secrets`) and `search`.
- `src/db.rs`, `migrations/` - the Postgres pool and the schema, embedded at
  compile time.
- `src/store/` - async functions over the pool: `catalogs`, `keys`,
  `documents`, `import`, `export`.
- `src/archive.rs` - reading a `.tar.gz` or a directory with the import rules,
  and writing a `.tar.gz`.
- `src/keys.rs` - API key generation, hashing and constant-time comparison.
- `src/catalog.rs` - `CatalogState`: a catalog's snapshot, loaded from its
  rows and rebuilt and swapped when its generation moves; lint with revision
  times.
- `src/auth.rs` - the key cache and the bearer middleware; `src/selection.rs` -
  which catalogs a call acts on; `src/app.rs` - the shared state and the poll
  loop.
- `src/mcp/` - the rmcp server handler, the tool context and the read tools;
  `src/http/` - the router, `/healthz`, the archive routes and `serve`.
- `src/cli.rs`, `src/config.rs`, `src/main.rs` - the `okf-mcp` command line
  and its configuration file.
- `src/clock.rs`, `src/error.rs` - the time source and the error type.
- `tests/parity.rs` - differential tests against `okf.py`: goldens for the
  synthetic corpus in `tests/fixtures/catalog`, and live tests that run
  `okf.py` itself.
- `tests/roundtrip.rs` - import then export reproduces a catalog's files
  exactly, and importing the export again changes nothing.
- `tests/server.rs` - end-to-end tests of the server over HTTP with rmcp's
  client: keys, catalog selection, the tools and the archive routes.
- `tests/smoke.rs` - every analysis and a search on a real catalog, the
  storage path (import, load, links, lint, export) through Postgres, and the
  server (every read tool and the export over HTTP), timed.
- `tests/support/` - a throwaway Postgres container per test, and a test
  server on a free port with rmcp clients.
- `docs/superpowers/` - the design and the per-milestone plans.
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
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --test parity -- --ignored    # live parity against okf.py
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --test roundtrip -- --ignored # the real catalog round-trips
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --release --test smoke -- --ignored --nocapture   # timings
    UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens

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

Before a push, also run `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.
