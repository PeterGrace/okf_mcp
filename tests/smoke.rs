//! End-to-end smoke tests on a real catalog, printing how long each step
//! takes: `every_analysis_runs_on_a_real_catalog` builds the snapshot from the
//! checkout and runs every M2 analysis and a search on it;
//! `storage_round_trip_on_a_real_catalog` (M3) imports the checkout into a
//! throwaway Postgres, loads the snapshot from the rows, runs the links report
//! twice (the second with the cached mention matcher) and lint with revision
//! times, and exports the catalog again; `server_on_a_real_catalog` (M4)
//! serves the imported checkout over HTTP and times each read tool and an
//! export through rmcp's client; `writes_on_a_real_catalog` (M5) imports the
//! checkout into a throwaway catalog, creates a concept with a new folder
//! index, stamps it, logs it and publishes, timing each write tool and the
//! publish through rmcp's client; `restructuring_on_a_real_catalog` (M6)
//! imports the checkout into a throwaway catalog, publishes a probe concept,
//! then moves it, splits a section out of it, tries a delete that must be
//! refused while linked, and publishes, timing each tool through rmcp's
//! client (`rewrite_links` and the delete's link check walk the whole
//! catalog).
//!
//! All five tests are `#[ignore]`d because they need a catalog checkout (and
//! the storage, server, write and restructuring tests need Docker). Run them
//! in release mode, since the timings are the point:
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

/// Serve a real catalog over HTTP and time every read tool and the export.
///
/// The tools go through rmcp's client and the real router, so each timing
/// includes the HTTP round trip, the auth layer, catalog selection and the
/// JSON encoding, not just the analysis. Only sizes and timings are printed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn server_on_a_real_catalog() {
    use okf_mcp::archive::read_directory;
    use okf_mcp::config::Config;
    use okf_mcp::store::keys::Access;
    use serde_json::json;
    use support::server::{call, structured, TestServer};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let files = read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    let count = files.len();
    let started = Instant::now();
    // `vec![(name, files)]`: one catalog, `pg`, imported before the server
    // starts. The time includes starting the Postgres container.
    let server = TestServer::start(vec![("pg", files)], Config::default()).await;
    eprintln!(
        "start Postgres, import {count} files and start the server: {:?}",
        started.elapsed()
    );
    let key = server.key("smoke", Access::Read, &[], Some("pg")).await;
    let client = server.client(&key, None).await.unwrap();

    let started = Instant::now();
    let found = call(&client, "search", json!({"query": "time server"})).await;
    let search_time = started.elapsed();
    let hits = &structured(&found)["hits"];
    eprintln!(
        "search: {} hits in {search_time:?}",
        hits.as_array().map_or(0, Vec::len)
    );
    // The best hit's path, so `get_concept` times a real concept.
    let first = hits[0]["path"]
        .as_str()
        .expect("the search finds a concept")
        .to_string();

    // `links_report` runs twice: the second call reuses the snapshot's cached
    // mention matcher (M3, #33).
    let calls = [
        ("list_catalogs", json!({})),
        ("get_concept", json!({"path": first, "links": true})),
        ("read_document", json!({})),
        ("history", json!({"path": "/log.md"})),
        ("open_questions", json!({})),
        ("oldest", json!({})),
        ("sprawl", json!({})),
        ("links_report", json!({})),
        ("links_report", json!({})),
        ("lint", json!({})),
    ];
    let mut failed = Vec::new();
    for (tool, arguments) in calls {
        let started = Instant::now();
        let result = call(&client, tool, arguments).await;
        let elapsed = started.elapsed();
        // The size of the structured JSON, not its content.
        let bytes = result
            .structured_content
            .as_ref()
            .map_or(0, |value| value.to_string().len());
        eprintln!("{tool}: {bytes} bytes of JSON in {elapsed:?}");
        // An error result's content is the `{error, message}` JSON (paths and
        // catalog names, never document text), kept so a failure in this
        // manual run says why without a rerun. `result` is owned, so its
        // content moves into the list rather than being cloned.
        if result.is_error == Some(true) {
            failed.push((tool, result.content));
        }
    }

    let started = Instant::now();
    let response = reqwest::Client::new()
        .get(format!("{}/catalogs/pg/export", server.base))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let export = response.bytes().await.unwrap();
    eprintln!(
        "export route: {} bytes in {:?}",
        export.len(),
        started.elapsed()
    );
    client.cancel().await.unwrap();

    assert!(failed.is_empty(), "these tools returned errors: {failed:?}");
    assert_eq!(status, reqwest::StatusCode::OK);
    // Every gzip stream starts with the bytes 0x1f 0x8b.
    assert!(export.starts_with(&[0x1f, 0x8b]), "the export is gzip");
}

/// Write to a real catalog over HTTP and time the write path and publish.
///
/// The checkout is imported into the test's own Postgres container, so the
/// real catalog is never written. The new concept goes in a new folder:
/// `create_concept` with `index_summary` then also creates the folder's
/// `index.md` and lists it in the root index, as `ensure_index` does, so
/// publish replays nothing but checks the changed files against the whole
/// catalog's lint. Only sizes and timings are printed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn writes_on_a_real_catalog() {
    use okf_mcp::archive::read_directory;
    use okf_mcp::config::Config;
    use okf_mcp::store::keys::Access;
    use serde_json::json;
    use support::server::{call, TestServer};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let files = read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    let count = files.len();
    let started = Instant::now();
    let server = TestServer::start(vec![("pg", files)], Config::default()).await;
    eprintln!(
        "start Postgres, import {count} files and start the server: {:?}",
        started.elapsed()
    );
    let key = server.key("smoke", Access::Write, &[], Some("pg")).await;
    let client = server
        .client_with(&key, &[("x-okf-changeset", "smoke")])
        .await
        .unwrap();

    // The first write builds the overlay (a full snapshot); the later ones
    // rebuild it after each version bump, so their times show that cost.
    let calls = [
        (
            "create_concept",
            json!({
                "path": "/m5-smoke/probe.md",
                "frontmatter": {
                    "type": "Note",
                    "title": "M5 smoke probe",
                    "description": "Written by the M5 smoke test.",
                },
                "body": "# M5 smoke probe\n\nSee the [log](/log.md).\n",
                "index_summary": "the M5 smoke test's probe",
            }),
        ),
        (
            "stamp",
            json!({"path": "/m5-smoke/probe.md", "field": "verified"}),
        ),
        ("log", json!({"text": "M5 smoke probe", "kind": "Creation"})),
        ("pending_changes", json!({})),
        ("lint", json!({"pending": true})),
        ("publish", json!({"message": "M5 smoke test"})),
    ];
    let mut failed = Vec::new();
    // `Value::Null` until the publish answers; indexing a `Value` with a
    // missing key gives `Null` rather than panicking, so the asserts below
    // fail with a message instead.
    let mut published = serde_json::Value::Null;
    for (tool, arguments) in calls {
        let started = Instant::now();
        let result = call(&client, tool, arguments).await;
        let elapsed = started.elapsed();
        let bytes = result
            .structured_content
            .as_ref()
            .map_or(0, |value| value.to_string().len());
        eprintln!("{tool}: {bytes} bytes of JSON in {elapsed:?}");
        if result.is_error == Some(true) {
            // The `{error, message}` JSON names paths and rules, never document text.
            failed.push((tool, result.content));
        } else if tool == "publish" {
            published = result.structured_content.unwrap_or_default();
        }
    }
    client.cancel().await.unwrap();

    assert!(failed.is_empty(), "these tools returned errors: {failed:?}");
    assert_eq!(published["published"], true);
    // `as_array().map_or(&[][..], Vec::as_slice)`: the revisions as a slice,
    // or an empty one if the field is missing, so the asserts below say what
    // was published instead of panicking on the lookup.
    let revisions = published["revisions"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    eprintln!(
        "publish wrote {} revisions, {} warnings",
        revisions.len(),
        published["warnings"].as_array().map_or(0, Vec::len)
    );
    let mut paths: Vec<&str> = revisions
        .iter()
        .filter_map(|revision| revision["path"].as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "/index.md",
            "/log.md",
            "/m5-smoke/index.md",
            "/m5-smoke/probe.md"
        ],
        "the probe, its folder index, the root index and log.md"
    );
}

/// Restructure a real catalog over HTTP and time each tool and the publish.
///
/// The checkout is imported into the test's own Postgres container, so the
/// real catalog is never written. A probe concept is created and published
/// first; then it is moved (which rewrites links across the whole catalog),
/// a section is split out of it, and a delete of the split-out concept is
/// refused because the probe's stub links it. Only sizes and timings are
/// printed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG and Docker"]
async fn restructuring_on_a_real_catalog() {
    use okf_mcp::archive::read_directory;
    use okf_mcp::config::Config;
    use okf_mcp::store::keys::Access;
    use serde_json::json;
    use support::server::{call, TestServer};

    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let files = read_directory(std::path::Path::new(&root), u64::MAX).unwrap();
    let count = files.len();
    let started = Instant::now();
    let server = TestServer::start(vec![("pg", files)], Config::default()).await;
    eprintln!(
        "start Postgres, import {count} files and start the server: {:?}",
        started.elapsed()
    );
    let key = server.key("smoke", Access::Write, &[], Some("pg")).await;
    let client = server
        .client_with(&key, &[("x-okf-changeset", "smoke")])
        .await
        .unwrap();

    let calls = [
        (
            "create_concept",
            json!({
                "path": "/m6-smoke/probe.md",
                "frontmatter": {
                    "type": "Note",
                    "title": "M6 smoke probe",
                    "description": "Written by the M6 smoke test.",
                },
                "body": "# M6 smoke probe\n\n## Part one\n\nOne.\n\n## Part two\n\nTwo.\n",
                "index_summary": "the M6 smoke test's probe",
            }),
        ),
        ("publish", json!({"message": "M6 smoke test: probe"})),
        (
            "move_concept",
            json!({"old": "/m6-smoke/probe", "new": "/m6-smoke/moved.md"}),
        ),
        (
            "split_concept",
            json!({
                "path": "/m6-smoke/moved.md",
                "section": "Part two",
                "to": "/m6-smoke/part-two.md",
                "description": "The second part.",
            }),
        ),
        ("delete_concept", json!({"path": "/m6-smoke/part-two.md"})),
        ("pending_changes", json!({})),
        ("publish", json!({"message": "M6 smoke test: restructure"})),
    ];
    let mut failed = Vec::new();
    let mut refusal = serde_json::Value::Null;
    // `Value::Null` until the last publish answers; indexing a missing key
    // of a `Value` gives `Null`, so the asserts below fail with a message.
    let mut published = serde_json::Value::Null;
    for (tool, arguments) in calls {
        let started = Instant::now();
        let result = call(&client, tool, arguments).await;
        let elapsed = started.elapsed();
        let bytes = result
            .structured_content
            .as_ref()
            .map_or(0, |value| value.to_string().len());
        eprintln!("{tool}: {bytes} bytes of JSON in {elapsed:?}");
        // A tuple `match`: the tool name and whether it failed, together.
        match (tool, result.is_error == Some(true)) {
            // The one refusal the test expects; its JSON names paths and lines only.
            ("delete_concept", true) => refusal = result.structured_content.unwrap_or_default(),
            (_, true) => failed.push((tool, result.content)),
            ("publish", false) => published = result.structured_content.unwrap_or_default(),
            (_, false) => {}
        }
    }
    client.cancel().await.unwrap();

    assert!(failed.is_empty(), "these tools returned errors: {failed:?}");
    assert_eq!(refusal["error"], "StillLinked");
    // Line 15 of `moved.md` is the stub `split_concept` leaves under the
    // section's heading: lines 1-6 are the frontmatter `create_concept`
    // writes (`---`, `type`, `title`, `description`, `generated`, `---`) and
    // lines 7-14 the probe's body up to and including the blank line after
    // `## Part two`. A change to that frontmatter or to the stub's layout
    // moves it. The folder's own listing in `index.md` is not a link here
    // (plan Decision 4), so the stub is the only one.
    assert_eq!(
        refusal["links"],
        json!([{"source": "m6-smoke/moved.md", "line": 15}]),
        "the stub is the split-out concept's only link"
    );
    assert_eq!(published["published"], true);
    let revisions = published["revisions"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    eprintln!(
        "publish wrote {} revisions, replayed {}, {} warnings",
        revisions.len(),
        published["replayed"].as_array().map_or(0, Vec::len),
        published["warnings"].as_array().map_or(0, Vec::len)
    );
    let mut paths: Vec<&str> = revisions
        .iter()
        .filter_map(|revision| revision["path"].as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "/m6-smoke/index.md",
            "/m6-smoke/moved.md",
            "/m6-smoke/part-two.md",
            "/m6-smoke/probe.md"
        ],
        "the folder index, the moved probe, the split-out part and the old path"
    );
}
