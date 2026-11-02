//! End-to-end smoke tests on a real catalog, printing how long each step
//! takes: `every_analysis_runs_on_a_real_catalog` builds the snapshot from the
//! checkout and runs every M2 analysis and a search on it;
//! `storage_round_trip_on_a_real_catalog` (M3) imports the checkout into a
//! throwaway Postgres, loads the snapshot from the rows, runs the links report
//! twice (the second with the cached mention matcher) and lint with revision
//! times, and exports the catalog again.
//!
//! Both tests are `#[ignore]`d because they need a catalog checkout (and the
//! storage test needs Docker). Run them in release mode, since the timings
//! are the point:
//!
//! `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --release --test smoke -- --ignored --nocapture`
//!
//! `--nocapture` lets the `eprintln!` timings through (the test harness
//! captures a passing test's output otherwise). They print counts and
//! timings only, never catalog content. Parity with `okf.py` is
//! `tests/parity.rs`'s job and exactness of import and export is
//! `tests/roundtrip.rs`'s; these tests only check that everything runs on a
//! real catalog and how fast. The one ranking check (a search for "time
//! server" puts `/infra/time-server.md` first) runs only when the catalog
//! has that concept with that alias, as `pg_knowledge_catalog` does.

mod support;

use std::time::{Duration, Instant};

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
///
/// Returns the time `run` took, measured before printing.
fn timed(name: &str, run: impl FnOnce() -> usize) -> Duration {
    let started = Instant::now();
    let count = run();
    let elapsed = started.elapsed();
    eprintln!("{name}: {count} in {elapsed:?}");
    elapsed
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
    let lint_time = timed("lint (no commit times): findings", || {
        let report = lint(&snapshot, now, None);
        report.errors.len() + report.warnings.len()
    });
    eprintln!("snapshot plus lint: {:?}", build_time + lint_time);
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
    // `pg_knowledge_catalog`'s time server concept has "time server" as an
    // alias, so it must rank first; another catalog only has to run.
    // `is_some_and` is `Option`'s "is there a value, and does it pass?".
    let has_time_server = snapshot.get("infra/time-server.md").is_some_and(|doc| {
        doc.aliases()
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case("time server"))
    });
    if has_time_server {
        assert_eq!(
            hits.first().map(|hit| hit.path.as_str()),
            Some("/infra/time-server.md"),
            "the time server concept has 'time server' as an alias"
        );
    }
}

/// Import, load, analyze and export a real catalog through Postgres, timed.
///
/// The directory read includes untracked files in the checkout; the exact
/// round trip (`git archive`) is `tests/roundtrip.rs`'s. Here only the
/// timings matter, plus one check: the cached matcher makes the second links
/// report cheaper than the first.
// `flavor = "multi_thread"`: `import` and `export` move CPU work to
// `spawn_blocking`, and the store's tests all run on the multi-threaded
// runtime. `async fn` tests need a runtime; `#[tokio::test]` starts one.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn storage_round_trip_on_a_real_catalog() {
    use okf_mcp::archive;
    use okf_mcp::catalog::CatalogState;
    use okf_mcp::store::import::{import, ImportOptions};
    use okf_mcp::store::{catalogs, export};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let db = support::pg::TestDb::start().await;
    okf_mcp::db::migrate(&db.pool).await.unwrap();
    catalogs::create(&db.pool, "pg", "", Utc::now())
        .await
        .unwrap();

    let started = Instant::now();
    let files = archive::read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    eprintln!(
        "read directory: {} files in {:?}",
        files.len(),
        started.elapsed()
    );

    let started = Instant::now();
    let options = ImportOptions {
        replace: false,
        actor: "smoke",
        now: Utc::now(),
    };
    let report = import(&db.pool, "pg", files, &options).await.unwrap();
    eprintln!(
        "import: {} added, lint {} errors / {} warnings in {:?}",
        report.added,
        report.lint.errors.len(),
        report.lint.warnings.len(),
        started.elapsed()
    );

    let started = Instant::now();
    let state = CatalogState::load(&db.pool, "pg").await.unwrap();
    eprintln!("load snapshot: {:?}", started.elapsed());
    // `current()` hands out an `Arc` (a shared pointer) to the loaded
    // snapshot, so the borrows below stay valid while `state` could swap.
    let loaded = state.current();

    let first = timed(
        "links (first call, builds the matcher): unlinked mentions",
        || {
            links_report(loaded.snapshot(), true)
                .summary
                .unlinked_mentions
        },
    );
    let second = timed("links (cached matcher): unlinked mentions", || {
        links_report(loaded.snapshot(), true)
            .summary
            .unlinked_mentions
    });
    // Wall-clock timing (about 10x apart); the caching itself is checked
    // deterministically by `the_mention_matcher_is_built_once_per_snapshot`.
    assert!(
        second < first,
        "the cached matcher makes the second report cheaper: {first:?} then {second:?}"
    );
    timed("lint with revision times: findings", || {
        let report = loaded.lint(Utc::now());
        report.errors.len() + report.warnings.len()
    });

    let started = Instant::now();
    let tarball = export::export(&db.pool, "pg", Vec::new()).await.unwrap();
    eprintln!("export: {} bytes in {:?}", tarball.len(), started.elapsed());
}
