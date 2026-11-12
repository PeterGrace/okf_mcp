//! A document's history: its rows in `revisions`, newest first.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// One revision of a document (its content left out).
// `sqlx::FromRow` derives the code that builds this struct from a result
// row, matching columns to fields by name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RevisionRow {
    /// The revision number, 1 for the first.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// Who made it.
    pub actor: String,
    /// The publish message (`import` for an import).
    pub message: String,
    /// When (server clock).
    pub created_at: DateTime<Utc>,
}

/// The newest `limit` revisions of one path.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `path` - catalog-relative path, as stored (no leading `/`).
/// * `limit` - at most this many rows.
///
/// # Returns
/// The rows, newest first; empty if the path never existed.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revisions(
    pool: &PgPool,
    catalog_id: i64,
    path: &str,
    limit: i64,
) -> Result<Vec<RevisionRow>, OkfError> {
    // The primary key (catalog_id, path, revision), read backwards, serves
    // both the filter and the order, so the query reads `limit` index entries.
    let rows = sqlx::query_as::<_, RevisionRow>(
        "SELECT revision, op, actor, message, created_at FROM revisions \
         WHERE catalog_id = $1 AND path = $2 ORDER BY revision DESC LIMIT $3",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One revision's content, for `revert`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RevisionContent {
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// The bytes; `None` for a `delete`.
    pub content: Option<Vec<u8>>,
}

/// One path's content at one revision.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `path` - catalog-relative path, as stored (no leading `/`).
/// * `revision` - the revision number.
///
/// # Returns
/// The revision's content, or `None` if the path has no such revision.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn content_at(
    pool: &PgPool,
    catalog_id: i64,
    path: &str,
    revision: i32,
) -> Result<Option<RevisionContent>, OkfError> {
    // One primary-key lookup: (catalog_id, path, revision).
    let row = sqlx::query_as::<_, RevisionContent>(
        "SELECT op, content FROM revisions \
         WHERE catalog_id = $1 AND path = $2 AND revision = $3",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(revision)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::files;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::import::{import, ImportOptions, ImportScope};
    use chrono::Utc;

    #[tokio::test(flavor = "multi_thread")]
    async fn revisions_come_newest_first_and_outlive_a_delete() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        for (version, replace) in [("one", false), ("two", true)] {
            let options = ImportOptions {
                replace,
                scope: ImportScope::Everything,
                expect_generation: None,
                actor: "tester",
                now: Utc::now(),
            };
            let text = format!("# {version}\n");
            let entries = files(&[("a.md", &text), ("b.md", "# B\n")]);
            import(&db.pool, "pg", entries, &options).await.unwrap();
        }
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "tester",
            now: Utc::now(),
        };
        import(&db.pool, "pg", files(&[("b.md", "# B\n")]), &options)
            .await
            .unwrap();

        let all = revisions(&db.pool, catalog.id, "a.md", 20).await.unwrap();
        let one = revisions(&db.pool, catalog.id, "a.md", 1).await.unwrap();
        let none = revisions(&db.pool, catalog.id, "c.md", 20).await.unwrap();

        let numbers: Vec<i32> = all.iter().map(|row| row.revision).collect();
        assert_eq!(numbers, [3, 2, 1]);
        // Import writes `import` for every added or changed file (M3,
        // `src/store/import.rs`) and `delete` for a file it drops, both
        // with the message `import`.
        assert_eq!(all[0].op, "delete");
        assert_eq!(all[1].op, "import");
        assert_eq!(all[0].message, "import");
        assert_eq!(all[2].actor, "tester");
        assert_eq!(one, all[..1]);
        assert!(none.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn content_at_reads_one_revision_or_its_deletion() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let rounds: [(&[(&str, &str)], bool); 2] = [
            (&[("a.md", "# One\n"), ("b.md", "# B\n")], false),
            (&[("b.md", "# B\n")], true),
        ];
        for (entries, replace) in rounds {
            let options = ImportOptions {
                replace,
                scope: ImportScope::Everything,
                expect_generation: None,
                actor: "tester",
                now: Utc::now(),
            };
            import(&db.pool, "pg", files(entries), &options)
                .await
                .unwrap();
        }

        let first = content_at(&db.pool, catalog.id, "a.md", 1).await.unwrap();
        let deleted = content_at(&db.pool, catalog.id, "a.md", 2).await.unwrap();
        let missing = content_at(&db.pool, catalog.id, "a.md", 3).await.unwrap();

        let first = first.expect("revision 1");
        assert_eq!(
            (first.op.as_str(), first.content.as_deref()),
            ("import", Some(&b"# One\n"[..]))
        );
        let deleted = deleted.expect("revision 2");
        assert_eq!((deleted.op.as_str(), deleted.content), ("delete", None));
        assert!(missing.is_none());
    }
}
