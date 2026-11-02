//! Export (design "Export"): a catalog's live documents as a `.tar.gz`.

use std::io::Write;

use sqlx::PgPool;

use crate::archive::{self, ExportFile};
use crate::error::OkfError;
use crate::store::{catalogs, documents};

/// Writes a catalog's live documents to `out` as a `.tar.gz`: no top-level
/// directory, modes 0644/0755, each file's mtime its `updated_at`, and no
/// tombstones.
///
/// The catalog lookup and the document read share one read-only
/// `REPEATABLE READ` transaction, so they see one snapshot: a catalog
/// deleted between the two is reported as unknown rather than exported
/// empty. Compressing is CPU work and runs on a blocking thread.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name.
/// * `out` - where to write (a `Vec<u8>`, a file, an HTTP body buffer).
///
/// # Returns
/// `out`, with the archive finished.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Database`]; [`OkfError::Io`] if writing fails.
// `W: Write + Send + 'static`: `out` moves to a blocking thread, so it must
// be safe to send there (`Send`) and must not borrow anything that thread
// could outlive (`'static`). `Vec<u8>` and `File` both qualify.
pub async fn export<W>(pool: &PgPool, catalog: &str, out: W) -> Result<W, OkfError>
where
    W: Write + Send + 'static,
{
    let mut tx = pool.begin().await?;
    // Must be the transaction's first statement. Under the default
    // READ COMMITTED each query would see its own snapshot.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    let rows = documents::published(&mut *tx, catalog.id).await?;
    // Nothing was written; ending the transaction now frees its connection
    // before the (possibly long) compression.
    tx.commit().await?;
    // `move` hands `rows` and `out` to the closure; `ExportFile`s borrow from
    // `rows` inside it, so nothing is copied.
    tokio::task::spawn_blocking(move || {
        let files: Vec<ExportFile<'_>> = rows
            .iter()
            .map(|row| ExportFile {
                path: &row.path,
                content: &row.content,
                executable: row.executable,
                modified: row.updated_at,
            })
            .collect();
        archive::write_tarball(out, &files)
    })
    .await
    // A panic in the blocking task is re-raised here, as if it ran inline
    // (as `import` does).
    .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::db::test_db;
    use crate::okf::workspace::StoredFile;
    use crate::store::import::{import, ImportOptions};
    use chrono::{TimeZone, Utc};

    fn stored(content: &[u8], executable: bool) -> StoredFile {
        StoredFile {
            content: content.to_vec(),
            executable,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_export_holds_the_live_documents_exactly() {
        let db = test_db().await;
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        catalogs::create(&db.pool, "pg", "", now).await.unwrap();
        let mut files = Files::new();
        files.insert("a.md".into(), stored(b"# A\n", false));
        files.insert("tools/run.sh".into(), stored(b"#!/bin/sh\n\0", true));
        files.insert("gone.md".into(), stored(b"# Gone\n", false));
        let options = ImportOptions {
            replace: false,
            actor: "t",
            now,
        };
        import(&db.pool, "pg", files.clone(), &options)
            .await
            .unwrap();
        // A later import that changes `a.md` and deletes `gone.md`:
        // `tools/run.sh` keeps its first `updated_at`.
        let later = Utc.with_ymd_and_hms(2026, 11, 2, 13, 0, 0).unwrap();
        files.remove("gone.md");
        files.insert("a.md".into(), stored(b"# A, again\n", false));
        let options = ImportOptions {
            replace: true,
            actor: "t",
            now: later,
        };
        import(&db.pool, "pg", files.clone(), &options)
            .await
            .unwrap();

        let tarball = export(&db.pool, "pg", Vec::new()).await.unwrap();

        // The tombstone is not exported; everything else reads back exactly.
        assert_eq!(
            archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap(),
            files
        );
        // Each entry's mtime is its document's `updated_at`.
        let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(tarball.as_slice()));
        let mtimes: Vec<(String, u64)> = entries
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path().unwrap().to_string_lossy().into_owned();
                (path, entry.header().mtime().unwrap())
            })
            .collect();
        let seconds = |time: chrono::DateTime<Utc>| u64::try_from(time.timestamp()).unwrap();
        assert_eq!(
            mtimes,
            [
                ("a.md".to_string(), seconds(later)),
                ("tools/run.sh".to_string(), seconds(now)),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_catalog_exports_an_empty_archive_and_unknown_ones_fail() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();

        let tarball = export(&db.pool, "pg", Vec::new()).await.unwrap();
        let unknown = export(&db.pool, "nope", Vec::new()).await;

        assert!(archive::read_tarball(tarball.as_slice(), u64::MAX)
            .unwrap()
            .is_empty());
        assert!(matches!(unknown, Err(OkfError::CatalogNotSelected { .. })));
    }
}
