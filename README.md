# okf_mcp

A Rust port of `tools/okf.py`, the Open Knowledge Format catalog CLI, on its
way to becoming a shared MCP server (design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`).

Status: milestones 1 (core library), 2 (analysis) and 3 (storage) are done.
The library reads a catalog into an in-memory `Snapshot` and runs `okf.py`'s
analyses on it with the same JSON output (compared as parsed values): `lint`,
the links report, `oldest`, `sprawl` and the secret scan, plus ranked
`search`, which `okf.py` does not have. Catalogs live in Postgres: every file
is a row holding its exact bytes and execute bit, with an append-only revision
history, and each catalog's snapshot is built from its rows and swapped when
the catalog changes. The `okf-mcp` command line administers catalogs and API
keys and imports and exports catalogs. There is no server yet: `okf-mcp
serve` arrives in milestone 4.

## Command line

    okf-mcp migrate
    okf-mcp catalog create pg --description "pgrace's catalog"
    git -C ~/repos/pg_knowledge_catalog archive --format=tar.gz HEAD | okf-mcp import pg -
    okf-mcp key create laptop --access write --catalogs pg --default pg --actor claude-code/claude-opus-5-5
    okf-mcp export pg pg.tar.gz

Every subcommand reads the database URL from `DATABASE_URL` (or
`--database-url`) and the optional TOML configuration file from
`OKF_MCP_CONFIG` (or `--config`); in milestone 3 the file holds only
`max_import_bytes` (default `64MiB`, the largest uncompressed import). Every
subcommand applies the embedded migrations first, so `migrate` is only needed
as a separate deploy step. `okf-mcp --help` and `okf-mcp <command> --help`
list the rest: `catalog list|delete`, `key list|revoke|grant|ungrant`,
`import --replace`.

`import` takes a `.tar.gz` file, `-` for one on stdin, or a directory. A
`git archive` tarball imports exactly the checkout's tracked files, and a
tarball breaking the import rules (a symlink, a `..` path, a duplicate, ...)
is refused with every problem listed. A directory is read leniently, through
the same listing as the analyses: untracked files are included, and a
symlink, special file or non-UTF-8 name it cannot store is flattened or
skipped with a warning (see `src/archive.rs`). `key create` prints the new key
once; only its SHA-256 is stored.

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
- `src/cli.rs`, `src/config.rs`, `src/main.rs` - the `okf-mcp` command line
  and its configuration file.
- `src/clock.rs`, `src/error.rs` - the time source and the error type.
- `tests/parity.rs` - differential tests against `okf.py`: goldens for the
  synthetic corpus in `tests/fixtures/catalog`, and live tests that run
  `okf.py` itself.
- `tests/roundtrip.rs` - import then export reproduces a catalog's files
  exactly, and importing the export again changes nothing.
- `tests/smoke.rs` - every analysis and a search on a real catalog, and the
  storage path (import, load, links, lint, export) through Postgres, timed.
- `tests/support/` - a throwaway Postgres container per test.
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

`cargo test` needs Docker: every test that touches the database starts its
own throwaway `postgres:17-alpine` container (via `testcontainers`) and
removes it when done; without Docker those tests fail at once with "is docker
reachable?". The roundtrip and storage smoke runs need Docker too, and the
real-catalog round trip needs `git` (it imports `git archive` of the
checkout).

The live parity tests need `python3` (3.12 or later: sprawl's scores depend
on its float `sum()`) and `okf.py` (`OKF_PY`, default
`~/repos/pg_knowledge_catalog/tools/okf.py`). The real catalog is read in
place and never copied into this repository. `#[ignore]` marks a test that
plain `cargo test` skips; `-- --ignored` runs only those.

Before a push, also run `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.
