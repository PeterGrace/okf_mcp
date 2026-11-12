//! Import then export reproduces a catalog's files exactly (content and
//! execute bit), and importing the export again changes nothing.
//!
//! `the_real_catalog_round_trips` is `#[ignore]`d: it needs a git checkout of
//! a catalog in `OKF_PARITY_CATALOG` (read in place, never copied here):
//!
//! `OKF_PARITY_CATALOG=~/repos/pg_knowledge_catalog cargo test --test roundtrip -- --ignored`

mod support;

use std::path::Path;
use std::process::Command;

use chrono::Utc;
use okf_mcp::archive::{self, Files};
use okf_mcp::okf::workspace::StoredFile;
use okf_mcp::store::import::{import, ImportOptions, ImportReport, ImportScope};
use okf_mcp::store::{catalogs, export};
use support::pg::TestDb;

/// Imports `files` into a new catalog, exports it, reads the export back,
/// and imports the export again with `--replace`.
///
/// # Returns
/// The files read from the export, and the second import's report.
async fn round_trip(files: &Files) -> (Files, ImportReport) {
    let db = TestDb::start_migrated().await;
    support::server::migrate_with_owner(&db.pool).await;
    catalogs::create(&db.pool, "rt", "", support::server::OWNER, Utc::now())
        .await
        .unwrap();
    let first = ImportOptions {
        replace: false,
        scope: ImportScope::Everything,
        expect_generation: None,
        actor: "test/roundtrip",
        now: Utc::now(),
    };
    import(&db.pool, "rt", files.clone(), &first).await.unwrap();

    let (tarball, _generation) = export::export(&db.pool, "rt", Vec::new()).await.unwrap();
    let exported = archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap();
    let again = ImportOptions {
        replace: true,
        scope: ImportScope::Everything,
        expect_generation: None,
        actor: "test/roundtrip",
        now: Utc::now(),
    };
    let report = import(&db.pool, "rt", exported.clone(), &again)
        .await
        .unwrap();
    (exported, report)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_synthetic_corpus_round_trips() {
    let root = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/catalog"
    ));
    // `mut`: the test adds one file below. Bindings are immutable by default.
    let mut files = archive::read_directory(root, u64::MAX).unwrap();
    assert!(files.len() > 40, "the fixture corpus is there");
    // The markdown walk skips these root directories; import still keeps them.
    for skipped in ["install/setup.md", "docs/plan.md", "tests/case.md"] {
        assert!(files.contains_key(skipped), "{skipped} is read");
    }
    // The fixture has no executable file, and adding one on disk would change
    // M2's goldens, so the execute bit is exercised with an in-memory script.
    files.insert(
        "tools/check.sh".to_string(),
        StoredFile {
            content: b"#!/bin/sh\nexit 0\n".to_vec(),
            executable: true,
        },
    );

    let (exported, again) = round_trip(&files).await;

    assert_eq!(exported, files);
    assert!(again.added.is_empty() && again.updated.is_empty() && again.deleted.is_empty());
    assert_eq!(again.unchanged, files.len());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a catalog checkout in OKF_PARITY_CATALOG"]
async fn the_real_catalog_round_trips() {
    let root = std::env::var_os("OKF_PARITY_CATALOG").expect("set OKF_PARITY_CATALOG");
    let archived = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["archive", "--format=tar.gz", "HEAD"])
        .output()
        .expect("run git archive");
    assert!(archived.status.success(), "git archive failed");
    let tracked = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
        .expect("run git ls-files");
    let tracked_count = tracked
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .count();
    let files = archive::read_tarball(archived.stdout.as_slice(), 256 * 1024 * 1024).unwrap();
    assert_eq!(files.len(), tracked_count, "every tracked file is imported");
    assert!(
        files.values().any(|file| file.executable),
        "the catalog has scripts"
    );

    let (exported, again) = round_trip(&files).await;

    assert_eq!(exported, files);
    assert!(again.added.is_empty() && again.updated.is_empty() && again.deleted.is_empty());
    assert_eq!(again.unchanged, files.len());
}
