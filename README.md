# okf_mcp

A Rust port of `tools/okf.py`, the Open Knowledge Format catalog CLI, on its
way to becoming a shared MCP server (design:
`docs/superpowers/specs/2026-10-26-okf-mcp-server-design.md`).

Status: milestones 1 (core library) and 2 (analysis) are done. The library
reads a catalog into an in-memory `Snapshot` and runs `okf.py`'s analyses
on it with the same JSON output (compared as parsed values): `lint`, the
links report, `oldest`, `sprawl` and the secret scan, plus ranked `search`,
which `okf.py` does not have. There is no server yet, and the binary is a
stub that only sets up logging (milestone 3 adds the server).

## Layout

- `src/okf/` - the document model (`yaml`, `doc`, `markdown`, `paths`,
  `workspace`, `timestamp`), the `snapshot`, `analysis::*` (`links`, `lint`,
  `oldest`, `sprawl`, `secrets`) and `search`.
- `src/clock.rs`, `src/error.rs` - the time source and the error type.
- `tests/parity.rs` - differential tests against `okf.py`: goldens for the
  synthetic corpus in `tests/fixtures/catalog`, and live tests that run
  `okf.py` itself.
- `tests/smoke.rs` - every analysis and a search on a real catalog, timed.
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

    cargo test                                   # unit, doc and golden tests
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --test parity -- --ignored    # live parity against okf.py
    OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog \
        cargo test --release --test smoke -- --ignored --nocapture   # timings
    UPDATE_GOLDEN=1 cargo test --test parity -- --ignored regenerate_goldens

The live tests need `python3` (3.12 or later: sprawl's scores depend on its
float `sum()`) and `okf.py` (`OKF_PY`, default
`~/repos/pg_knowledge_catalog/tools/okf.py`). The real catalog is read in
place and never copied into this repository. `#[ignore]` marks a test that
plain `cargo test` skips; `-- --ignored` runs only those.

The in-memory `MemWorkspace` test double is compiled only for the crate's
own unit tests, or with the `test-util` feature
(`cargo test --features test-util`) for an integration test that needs it.

Before a push, also run `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.
