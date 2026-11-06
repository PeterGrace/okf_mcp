//! The Postgres connection pool and the schema migrations (`migrations/`).

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::error::OkfError;

// The library's unit tests share the integration tests' container helper:
// `#[path]` compiles that file here as a module (relative to `src/`).
#[cfg(test)]
#[path = "../tests/support/pg.rs"]
pub(crate) mod pg_support;

/// The schema migrations in `migrations/`, embedded at compile time.
// `migrate!` reads the directory while compiling (`build.rs` makes cargo
// notice new files), so the binary needs no SQL files at runtime.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Connects a pool to the database.
///
/// # Arguments
/// * `url` - a `postgres://` or `postgresql://` URL (CNPG's generated secret).
///
/// # Returns
/// A pool of up to 8 connections. `async fn` returns a future: the caller
/// `.await`s it, like a Python coroutine.
///
/// # Errors
/// [`OkfError::Database`] if the URL is malformed or the server is unreachable.
pub async fn connect(url: &str) -> Result<PgPool, OkfError> {
    let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
    Ok(pool)
}

/// Applies every migration not yet applied (a no-op when up to date).
///
/// # Errors
/// [`OkfError::Migrate`] if a migration fails, or the database has a
/// migration this binary does not know (it is older than the schema).
pub async fn migrate(pool: &PgPool) -> Result<(), OkfError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// A migrated throwaway database for one test.
#[cfg(test)]
pub(crate) async fn test_db() -> pg_support::TestDb {
    let db = pg_support::TestDb::start().await;
    migrate(&db.pool).await.expect("migrations apply");
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn migrations_create_the_schema_and_rerun_as_a_no_op() {
        let db = test_db().await;

        migrate(&db.pool).await.unwrap();

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations' \
             ORDER BY table_name",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            [
                "api_keys",
                "catalogs",
                "changeset_entries",
                "changesets",
                "documents",
                "key_catalogs",
                "revisions",
                "server_state",
            ]
        );
        let admin_generation: i64 = sqlx::query_scalar("SELECT admin_generation FROM server_state")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(admin_generation, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn constraints_hold() {
        let db = test_db().await;
        let catalog: i64 = sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at) VALUES ('pg', $1) RETURNING id",
        )
        .bind(chrono::Utc::now())
        .fetch_one(&db.pool)
        .await
        .unwrap();

        let bad_name =
            sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('Bad Name', $1)")
                .bind(chrono::Utc::now())
                .execute(&db.pool)
                .await
                .unwrap_err();
        assert!(bad_name.as_database_error().unwrap().is_check_violation());

        // A key, for the changesets below and `key_catalogs` further down.
        sqlx::query(
            "INSERT INTO api_keys (id, name, sha256, access, all_catalogs, default_catalog_id, \
             actor, created_at) VALUES ('k1', 'k1', $1, 'read', false, $2, 'a', $3)",
        )
        .bind(vec![0u8; 32])
        .bind(catalog)
        .bind(chrono::Utc::now())
        .execute(&db.pool)
        .await
        .unwrap();
        // One key may hold several open changesets in a catalog (M8
        // Decision 7), but no two changesets share a handle.
        let open = "INSERT INTO changesets (catalog_id, handle, key_id, actor, state, created_at, \
                    updated_at) VALUES ($1, $2, 'k1', 'a', 'open', $3, $3)";
        for handle in ["h1", "h2"] {
            sqlx::query(open)
                .bind(catalog)
                .bind(handle)
                .bind(chrono::Utc::now())
                .execute(&db.pool)
                .await
                .unwrap();
        }
        let second = sqlx::query(open)
            .bind(catalog)
            .bind("h1")
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap_err();
        let second = second.as_database_error().unwrap();
        assert!(second.is_unique_violation());
        assert_eq!(second.constraint(), Some("changesets_handle_key"));

        let revision = "INSERT INTO revisions (catalog_id, path, revision, op, content, \
                        executable, actor, message, created_at) \
                        VALUES ($1, 'a.md', 1, 'import', $2, false, 'a', 'm', $3)";
        sqlx::query(revision)
            .bind(catalog)
            .bind(b"x".to_vec())
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap();
        let duplicate = sqlx::query(revision)
            .bind(catalog)
            .bind(b"y".to_vec())
            .bind(chrono::Utc::now())
            .execute(&db.pool)
            .await
            .unwrap_err();
        let duplicate = duplicate.as_database_error().unwrap();
        assert!(duplicate.is_unique_violation());
        assert_eq!(
            duplicate.constraint(),
            Some("revisions_catalog_id_path_revision_key")
        );

        // One row in every other table that refers to the catalog.
        sqlx::query(
            "INSERT INTO documents (catalog_id, path, content, executable, revision, \
             updated_at, updated_by) VALUES ($1, 'a.md', $2, false, 1, $3, 'a')",
        )
        .bind(catalog)
        .bind(b"x".to_vec())
        .bind(chrono::Utc::now())
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO key_catalogs (key_id, catalog_id) VALUES ('k1', $1)")
            .bind(catalog)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO changeset_entries (changeset_id, path, content) \
             SELECT id, 'a.md', $1 FROM changesets",
        )
        .bind(b"z".to_vec())
        .execute(&db.pool)
        .await
        .unwrap();

        // Deleting the catalog cascades to every row that refers to it, and
        // clears the key's default instead of deleting the key.
        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(catalog)
            .execute(&db.pool)
            .await
            .unwrap();
        let left: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM documents), (SELECT count(*) FROM key_catalogs), \
             (SELECT count(*) FROM changesets), (SELECT count(*) FROM changeset_entries), \
             (SELECT count(*) FROM revisions)",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(left, (0, 0, 0, 0, 0));
        // `Option<i64>` decodes a nullable column: `None` is SQL NULL.
        let default_catalog: Option<i64> =
            sqlx::query_scalar("SELECT default_catalog_id FROM api_keys WHERE id = 'k1'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(default_catalog, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_name_migration_preserves_existing_credentials_and_history() {
        let db = pg_support::TestDb::start().await;
        for migration in [
            include_str!("../migrations/0001_schema.sql"),
            include_str!("../migrations/0002_changeset_identity.sql"),
            include_str!("../migrations/0003_changeset_handles.sql"),
            include_str!("../migrations/0004_drop_changeset_labels.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&db.pool).await.unwrap();
        }
        let now = chrono::Utc::now();
        let catalog: i64 = sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at) VALUES ('pg', $1) RETURNING id",
        )
        .bind(now)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let secret = "okf_existing-holder-secret";
        sqlx::query(
            "INSERT INTO api_keys \
             (id, sha256, access, all_catalogs, actor, created_at) \
             VALUES ('legacy', $1, 'write', true, 'legacy-actor', $2)",
        )
        .bind(crate::keys::hash(secret).as_slice())
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO changesets \
             (catalog_id, handle, key_id, actor, state, created_at, updated_at) \
             VALUES ($1, 'existing-handle', 'legacy', 'legacy-actor', 'open', $2, $2)",
        )
        .bind(catalog)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        sqlx::raw_sql(include_str!("../migrations/0005_key_names.sql"))
            .execute(&db.pool)
            .await
            .unwrap();

        let record = crate::store::keys::list(&db.pool).await.unwrap().remove(0);
        assert_eq!(
            (record.id.as_str(), record.name.as_str()),
            ("legacy", "legacy")
        );
        assert!(crate::keys::matches(&record.sha256, secret));
        let owner: (Option<String>, String) =
            sqlx::query_as("SELECT key_id, actor FROM changesets WHERE handle = 'existing-handle'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            owner,
            (Some("legacy".to_string()), "legacy-actor".to_string())
        );
    }
}
