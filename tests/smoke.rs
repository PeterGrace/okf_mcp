//! End-to-end smoke test of the M2 analysis surface on a real catalog: build
//! the snapshot, then run every analysis and a search on it, printing how
//! long each step takes.
//!
//! The test is `#[ignore]`d because it needs a catalog checkout. Run it in
//! release mode, since the timings are the point:
//!
//! `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`
//!
//! `--nocapture` lets the `eprintln!` timings through (the test harness
//! captures a passing test's output otherwise). Parity with `okf.py` is
//! `tests/parity.rs`'s job; this test only checks that everything runs on a
//! real catalog and how fast.

use std::time::Instant;

use chrono::Utc;
use okf_mcp::okf::analysis::links::links_report;
use okf_mcp::okf::analysis::lint::lint;
use okf_mcp::okf::analysis::oldest::{oldest, OldestQuery};
use okf_mcp::okf::analysis::sprawl::{sprawl, SprawlQuery};
use okf_mcp::okf::search::{search, SearchQuery};
use okf_mcp::okf::snapshot::Snapshot;
use okf_mcp::okf::workspace::DiskWorkspace;

/// Runs `run` and prints `name`, the count it returns and the elapsed time.
///
/// `impl FnOnce() -> usize` accepts any closure returning a count; each
/// closure gets its own compiled copy of this function (generics are
/// monomorphized), so there is no dynamic dispatch.
fn timed(name: &str, run: impl FnOnce() -> usize) {
    let started = Instant::now();
    let count = run();
    eprintln!("{name}: {count} in {:?}", started.elapsed());
}

#[test]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG"]
fn every_analysis_runs_on_a_real_catalog() {
    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let workspace = DiskWorkspace::new(root).expect("catalog root exists");

    let started = Instant::now();
    let snapshot = Snapshot::build(&workspace).expect("snapshot builds");
    let build_time = started.elapsed();
    eprintln!(
        "snapshot: {} documents, {} secret findings in {build_time:?}",
        snapshot.docs().len(),
        snapshot.secret_findings().len()
    );

    let now = Utc::now();
    timed("links (with mentions): unlinked mentions", || {
        links_report(&snapshot, true).summary.unlinked_mentions
    });
    let lint_started = Instant::now();
    timed("lint (no commit times): findings", || {
        let report = lint(&snapshot, now, None);
        report.errors.len() + report.warnings.len()
    });
    eprintln!(
        "snapshot plus lint: {:?}",
        build_time + lint_started.elapsed()
    );
    timed("oldest: rows", || {
        oldest(&snapshot, now, OldestQuery::default()).len()
    });
    timed("sprawl: rows", || {
        sprawl(&snapshot, SprawlQuery::default())
            .expect("the top-n query names no concept, so it cannot fail")
            .len()
    });
    let search_started = Instant::now();
    let hits = search(&snapshot, &SearchQuery::new("time server"));
    eprintln!(
        "search \"time server\": {} hits in {:?}",
        hits.len(),
        search_started.elapsed()
    );

    assert!(!snapshot.docs().is_empty(), "a real catalog has documents");
    assert_eq!(
        hits.first().map(|hit| hit.path.as_str()),
        Some("/infra/time-server.md"),
        "the time server concept has 'time server' as an alias"
    );
}
