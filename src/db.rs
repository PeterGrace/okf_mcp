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
/// * `max_connections` - the pool size (`database_max_connections`).
///
/// # Returns
/// A pool of up to `max_connections` connections. `async fn` returns a
/// future: the caller `.await`s it, like a Python coroutine.
///
/// # Errors
/// [`OkfError::Database`] if the URL is malformed or the server is unreachable.
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, OkfError> {
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await?;
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

/// The user (and its personal team) every unit-test database starts with,
/// so tests that create keys, codes and catalogs have an owner to name.
#[cfg(test)]
pub(crate) const TEST_USER: &str = "test-user";
/// [`TEST_USER`]'s personal team.
#[cfg(test)]
pub(crate) const TEST_TEAM: &str = TEST_USER;

/// A migrated throwaway database for one test, with [`TEST_USER`].
#[cfg(test)]
pub(crate) async fn test_db() -> pg_support::TestDb {
    let db = pg_support::TestDb::start_migrated().await;
    let fixture = crate::store::users::NewUser {
        name: TEST_USER,
        display_name: "",
    };
    crate::store::users::create(&db.pool, &fixture, chrono::Utc::now())
        .await
        .expect("create the fixture user");
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn migrations_create_the_schema_and_rerun_as_a_no_op() {
        // A bare database, not `test_db()`: its fixture user would bump the
        // admin generation this test expects migrations to leave at zero.
        let db = pg_support::TestDb::start().await;
        migrate(&db.pool).await.unwrap();

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
                "maintenance_dispositions",
                "maintenance_evidence",
                "maintenance_findings",
                "maintenance_requests",
                "maintenance_runs",
                "maintenance_state",
                "maintenance_work",
                "oauth_clients",
                "oauth_tokens",
                "revisions",
                "server_state",
                "sign_in_code_catalogs",
                "sign_in_codes",
                "team_members",
                "teams",
                "users",
            ]
        );
        let admin_generation: i64 = sqlx::query_scalar("SELECT admin_generation FROM server_state")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(admin_generation, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oauth_schema_keeps_existing_keys_and_guards_grant_rows() {
        // Arrange: the schema as of 0007, with one key, then 0008 on top.
        let db = pg_support::TestDb::start().await;
        for migration in [
            include_str!("../migrations/0001_schema.sql"),
            include_str!("../migrations/0002_changeset_identity.sql"),
            include_str!("../migrations/0003_changeset_handles.sql"),
            include_str!("../migrations/0004_drop_changeset_labels.sql"),
            include_str!("../migrations/0005_key_names.sql"),
            include_str!("../migrations/0006_maintenance.sql"),
            include_str!("../migrations/0007_maintenance_assessment_evidence.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&db.pool).await.unwrap();
        }
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at) \
             VALUES ('existing', $1, 'write', true, 'example-actor', $2)",
        )
        .bind(vec![7u8; 32])
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();

        // Act
        sqlx::raw_sql(include_str!("../migrations/0008_oauth.sql"))
            .execute(&db.pool)
            .await
            .unwrap();

        // Assert: the old key is a `key` row with its hash and no client.
        let (kind, sha256, client_id): (String, Option<Vec<u8>>, Option<String>) =
            sqlx::query_as("SELECT kind, sha256, client_id FROM api_keys WHERE name = 'existing'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(kind, "key");
        assert_eq!(sha256, Some(vec![7u8; 32]));
        assert_eq!(client_id, None);

        // A grant row needs no hash, may not have one, and may not be admin
        // or report maintenance.
        let grant = |sha256: Option<Vec<u8>>, access: &'static str, report: bool| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO api_keys \
                     (name, sha256, access, maintenance_report, all_catalogs, actor, created_at, kind) \
                     VALUES ('laptop', $1, $2, $3, true, 'example-actor', $4, 'oauth')",
                )
                .bind(sha256)
                .bind(access)
                .bind(report)
                .bind(now)
                .execute(&pool)
                .await
            }
        };
        let constraint = |result: Result<_, sqlx::Error>| {
            result
                .expect_err("the row must be refused")
                .as_database_error()
                .and_then(|error| error.constraint().map(str::to_string))
        };
        assert!(grant(None, "write", false).await.is_ok());
        // Two live grants with one name and actor: allowed (the unique index
        // covers `kind = 'key'` rows only).
        assert!(grant(None, "read", false).await.is_ok());
        assert_eq!(
            constraint(grant(Some(vec![8u8; 32]), "write", false).await).as_deref(),
            Some("api_keys_kind_hash")
        );
        assert_eq!(
            constraint(grant(None, "admin", false).await).as_deref(),
            Some("api_keys_oauth_caps")
        );
        assert_eq!(
            constraint(grant(None, "read", true).await).as_deref(),
            Some("api_keys_oauth_caps")
        );
        // A sign-in code names a grant only once redeemed, and keeps its
        // redemption when that grant row is deleted (`ON DELETE SET NULL`).
        let grant_id: String = sqlx::query_scalar(
            "INSERT INTO api_keys (name, access, all_catalogs, actor, created_at, kind) \
             VALUES ('doomed', 'read', true, 'example-actor', $1, 'oauth') RETURNING id",
        )
        .bind(now)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let code = |byte: u8, redeemed: bool| {
            let pool = db.pool.clone();
            let grant_id = grant_id.clone();
            async move {
                sqlx::query(
                    "INSERT INTO sign_in_codes \
                     (name, sha256, access, all_catalogs, actor, created_at, expires_at, \
                      redeemed_at, grant_id) \
                     VALUES ('laptop', $1, 'read', true, 'example-actor', $2, $3, $4, $5)",
                )
                .bind(vec![byte; 32])
                .bind(now)
                .bind(now + chrono::TimeDelta::hours(1))
                .bind(redeemed.then_some(now))
                .bind(grant_id)
                .execute(&pool)
                .await
            }
        };
        assert_eq!(
            constraint(code(1, false).await).as_deref(),
            Some("sign_in_codes_redeemed_grant")
        );
        code(2, true).await.unwrap();
        sqlx::query("DELETE FROM api_keys WHERE id = $1")
            .bind(&grant_id)
            .execute(&db.pool)
            .await
            .expect("deleting a redeemed code's grant clears the code's grant_id");
        let (redeemed_at, kept_grant): (Option<chrono::DateTime<chrono::Utc>>, Option<String>) =
            sqlx::query_as("SELECT redeemed_at, grant_id FROM sign_in_codes")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!((redeemed_at.is_some(), kept_grant), (true, None));
        // A key row still needs its hash, and a second live key of one
        // name and actor is still refused.
        let key = |sha256: Option<Vec<u8>>| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at) \
                     VALUES ('existing', $1, 'read', true, 'example-actor', $2)",
                )
                .bind(sha256)
                .bind(now)
                .execute(&pool)
                .await
            }
        };
        assert_eq!(
            constraint(key(None).await).as_deref(),
            Some("api_keys_kind_hash")
        );
        assert_eq!(
            constraint(key(Some(vec![9u8; 32])).await).as_deref(),
            Some("api_keys_live_name_actor")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migration_0009_keeps_every_credential_and_catalog_unassigned() {
        // Arrange: the schema as of 0008, with one catalog, one key, one grant
        // and one sign-in code, written as the pre-0009 server wrote them.
        let db = pg_support::TestDb::start().await;
        for migration in [
            include_str!("../migrations/0001_schema.sql"),
            include_str!("../migrations/0002_changeset_identity.sql"),
            include_str!("../migrations/0003_changeset_handles.sql"),
            include_str!("../migrations/0004_drop_changeset_labels.sql"),
            include_str!("../migrations/0005_key_names.sql"),
            include_str!("../migrations/0006_maintenance.sql"),
            include_str!("../migrations/0007_maintenance_assessment_evidence.sql"),
            include_str!("../migrations/0008_oauth.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&db.pool).await.unwrap();
        }
        sqlx::raw_sql(
            "INSERT INTO catalogs (name, created_at) VALUES ('legacy', now());
             INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at)
                 VALUES ('old', decode(repeat('ab', 32), 'hex'), 'write', true, 'old', now());
             INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at)
                 VALUES ('c1', 'dcr', '{https://client.example.com/cb}', '{}', now());
             INSERT INTO api_keys (name, kind, client_id, access, all_catalogs, actor, created_at)
                 VALUES ('old.grant', 'oauth', 'c1', 'read', true, 'old', now());
             INSERT INTO sign_in_codes (name, sha256, access, all_catalogs, actor, created_at, expires_at)
                 VALUES ('old-code', decode(repeat('cd', 32), 'hex'), 'read', true, 'old',
                         now(), now() + interval '1 day');",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        // Act
        sqlx::raw_sql(include_str!("../migrations/0009_users_teams.sql"))
            .execute(&db.pool)
            .await
            .unwrap();

        // Assert: every row survived, none was attributed to anyone.
        let unassigned: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM api_keys WHERE user_id IS NULL), \
                    (SELECT count(*) FROM sign_in_codes WHERE user_id IS NULL), \
                    (SELECT count(*) FROM catalogs WHERE owner_team_id IS NULL)",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(unassigned, (2, 1, 1));
        let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(users, 0, "the migration creates no user");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_with_a_credential_cannot_be_deleted() {
        // Arrange: a user and a key that names it.
        let db = test_db().await;
        sqlx::raw_sql(
            "INSERT INTO users (name, created_at) VALUES ('alice', now());
             INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at, user_id)
                 SELECT 'k', decode(repeat('ef', 32), 'hex'), 'read', true, 'k', now(), id
                 FROM users WHERE name = 'alice';",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        // Act
        let deleted = sqlx::query("DELETE FROM users WHERE name = 'alice'")
            .execute(&db.pool)
            .await;

        // Assert: `ON DELETE RESTRICT` keeps the key's user.
        let error = deleted.expect_err("RESTRICT refuses");
        assert_eq!(
            error.as_database_error().and_then(|e| e.code()).as_deref(),
            Some("23503"),
            "a foreign-key violation"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maintenance_schema_preserves_existing_rows_and_defaults_to_opt_out() {
        let db = pg_support::TestDb::start().await;
        for migration in [
            include_str!("../migrations/0001_schema.sql"),
            include_str!("../migrations/0002_changeset_identity.sql"),
            include_str!("../migrations/0003_changeset_handles.sql"),
            include_str!("../migrations/0004_drop_changeset_labels.sql"),
            include_str!("../migrations/0005_key_names.sql"),
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
        sqlx::query(
            "INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at) \
             VALUES ('existing', 'existing', $1, 'write', true, 'author', $2)",
        )
        .bind(vec![1u8; 32])
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
        for (path, content) in [("a.md", b"first".to_vec()), ("b.md", b"second".to_vec())] {
            sqlx::query(
                "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, message, created_at) \
                 VALUES ($1, $2, 1, 'import', $3, false, 'author', 'initial', $4)",
            )
            .bind(catalog)
            .bind(path)
            .bind(&content)
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO documents (catalog_id, path, content, executable, revision, updated_at, updated_by) \
                 VALUES ($1, $2, $3, false, 1, $4, 'author')",
            )
            .bind(catalog)
            .bind(path)
            .bind(content)
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        let documents =
            "SELECT to_jsonb(d) FROM (SELECT * FROM documents ORDER BY catalog_id, path) d";
        let revisions = "SELECT to_jsonb(r) FROM (SELECT * FROM revisions ORDER BY catalog_id, path, revision) r";
        let documents_before: Vec<serde_json::Value> = sqlx::query_scalar(documents)
            .fetch_all(&db.pool)
            .await
            .unwrap();
        let revisions_before: Vec<serde_json::Value> = sqlx::query_scalar(revisions)
            .fetch_all(&db.pool)
            .await
            .unwrap();
        let credentials = "SELECT to_jsonb(k) FROM (SELECT * FROM api_keys ORDER BY id) k";
        let credential_before: serde_json::Value = sqlx::query_scalar(credentials)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0006_maintenance.sql"))
            .execute(&db.pool)
            .await
            .unwrap();
        let existing_key_maintenance_report: bool =
            sqlx::query_scalar("SELECT maintenance_report FROM api_keys WHERE id = 'existing'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(!existing_key_maintenance_report);
        let credential_after: serde_json::Value = sqlx::query_scalar(credentials)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let mut expected_credential = credential_before;
        expected_credential["maintenance_report"] = serde_json::json!(false);
        assert_eq!(credential_after, expected_credential);
        let documents_after: Vec<serde_json::Value> = sqlx::query_scalar(documents)
            .fetch_all(&db.pool)
            .await
            .unwrap();
        let revisions_after: Vec<serde_json::Value> = sqlx::query_scalar(revisions)
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(documents_before, documents_after);
        assert_eq!(revisions_before, revisions_after);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maintenance_schema_rejects_cross_catalog_and_duplicate_live_claims() {
        let db = test_db().await;
        let now = chrono::Utc::now();
        let first: i64 = sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at) VALUES ('first', $1) RETURNING id",
        )
        .bind(now)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let second: i64 = sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at) VALUES ('second', $1) RETURNING id",
        )
        .bind(now)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        for catalog in [first, second] {
            sqlx::query(
                "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, message, created_at) \
                 VALUES ($1, 'a.md', 1, 'import', $2, false, 'a', 'm', $3)",
            )
            .bind(catalog)
            .bind(b"a".to_vec())
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO maintenance_runs (id, catalog_id, credential_id, actor, state, deadline, created_at, primary_count) \
                 VALUES ($1, $2, 'holder', 'a', 'running', $3, $3, 1)",
            )
            .bind(format!("run-{catalog}"))
            .bind(catalog)
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        let insert_work = "INSERT INTO maintenance_work (id, run_id, catalog_id, path, revision, lanes, lease_token, lease_expires_at, attempted_at) \
                           VALUES ($1, $2, $3, 'a.md', 1, ARRAY['changed'], $4, $5, $5)";
        sqlx::query(insert_work)
            .bind("work-one")
            .bind(format!("run-{first}"))
            .bind(first)
            .bind("token-one")
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        let duplicate_active_claim_insert = sqlx::query(insert_work)
            .bind("work-two")
            .bind(format!("run-{first}"))
            .bind(first)
            .bind("token-two")
            .bind(now)
            .execute(&db.pool)
            .await;
        assert!(duplicate_active_claim_insert.is_err());
        let cross_catalog_work_insert = sqlx::query(insert_work)
            .bind("bad-work")
            .bind(format!("run-{second}"))
            .bind(first)
            .bind("token-three")
            .bind(now)
            .execute(&db.pool)
            .await;
        assert!(cross_catalog_work_insert.is_err());
        sqlx::query(insert_work)
            .bind("work-other")
            .bind(format!("run-{second}"))
            .bind(second)
            .bind("token-other")
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO maintenance_findings (id, run_id, work_id, catalog_id, category, assertion, follow_up, uncertainty, relationship, external, created_at) \
             VALUES ('finding-one', $1, 'work-one', $2, 'suggested_link', 'a', 'follow', 'unknown', 'direct_statement', '[]', $3)",
        )
        .bind(format!("run-{first}"))
        .bind(first)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
        let cross_catalog_evidence_insert = sqlx::query(
            "INSERT INTO maintenance_evidence (catalog_id, work_id, finding_id, ordinal, path, revision, excerpt) \
             VALUES ($1, 'work-one', 'finding-one', 0, 'a.md', 1, 'a')",
        )
        .bind(second)
        .execute(&db.pool)
        .await;
        assert!(cross_catalog_evidence_insert.is_err());
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, message, created_at) \
             VALUES ($1, 'only-second.md', 1, 'import', $2, false, 'a', 'm', $3)",
        )
        .bind(second)
        .bind(b"a".to_vec())
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
        let cross_catalog_pin_insert = sqlx::query(
            "INSERT INTO maintenance_evidence (catalog_id, work_id, ordinal, path, revision, excerpt) \
             VALUES ($1, 'work-one', 0, 'only-second.md', 1, 'a')",
        )
        .bind(first)
        .execute(&db.pool)
        .await;
        assert!(cross_catalog_pin_insert.is_err());
        let cross_work_finding_insert = sqlx::query(
            "INSERT INTO maintenance_evidence (catalog_id, work_id, finding_id, ordinal, path, revision, excerpt) \
             VALUES ($1, $2, 'finding-one', 0, 'a.md', 1, 'a')",
        )
        .bind(second)
        .bind("work-other")
        .execute(&db.pool)
        .await;
        assert!(cross_work_finding_insert.is_err());
        let cross_catalog_finding_insert = sqlx::query(
            "INSERT INTO maintenance_findings (id, run_id, work_id, catalog_id, category, assertion, follow_up, uncertainty, relationship, external, created_at) \
             VALUES ('bad-finding', $1, 'work-one', $2, 'suggested_link', 'a', 'follow', 'unknown', 'direct_statement', '[]', $3)",
        )
        .bind(format!("run-{second}"))
        .bind(second)
        .bind(now)
        .execute(&db.pool)
        .await;
        assert!(cross_catalog_finding_insert.is_err());
        let cross_catalog_disposition_insert = sqlx::query(
            "INSERT INTO maintenance_dispositions (catalog_id, finding_id, disposition, rationale, credential_id, actor, created_at) \
             VALUES ($1, 'finding-one', 'dismissed', 'reason', 'holder', 'a', $2)",
        )
        .bind(second)
        .bind(now)
        .execute(&db.pool)
        .await;
        assert!(cross_catalog_disposition_insert.is_err());
        sqlx::query("UPDATE maintenance_work SET outcome = 'completed', ended_at = $1 WHERE id = 'work-one'")
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        let second_attempt_after_terminal_insert = sqlx::query(insert_work)
            .bind("work-two")
            .bind(format!("run-{first}"))
            .bind(first)
            .bind("token-two")
            .bind(now)
            .execute(&db.pool)
            .await;
        assert!(second_attempt_after_terminal_insert.is_ok());
        sqlx::query(
            "INSERT INTO maintenance_evidence (catalog_id, work_id, finding_id, ordinal, path, revision, excerpt) \
             VALUES ($1, 'work-one', 'finding-one', 0, 'a.md', 1, 'a')",
        )
        .bind(first)
        .execute(&db.pool)
        .await
        .unwrap();
        let duplicate_ordinal = sqlx::query(
            "INSERT INTO maintenance_evidence (catalog_id, work_id, finding_id, ordinal, path, revision, excerpt) \
             VALUES ($1, 'work-one', 'finding-one', 0, 'a.md', 1, 'a')",
        )
        .bind(first)
        .execute(&db.pool)
        .await;
        assert!(duplicate_ordinal.is_err());
        sqlx::query("INSERT INTO maintenance_state (catalog_id, updated_at) VALUES ($1, $2)")
            .bind(first)
            .bind(now)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO maintenance_requests (catalog_id, credential_id, operation, idempotency_key, request, response) \
             VALUES ($1, 'holder', 'submit', 'request-one', $2, $3)",
        )
        .bind(first)
        .bind(serde_json::json!({"work_id": "work-one"}))
        .bind(serde_json::json!({"outcome": "completed"}))
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO maintenance_dispositions (catalog_id, finding_id, disposition, rationale, credential_id, actor, created_at) \
             VALUES ($1, 'finding-one', 'dismissed', 'reason', 'holder', 'a', $2)",
        )
        .bind(first)
        .bind(now)
        .execute(&db.pool)
        .await
        .unwrap();
        let populated: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM maintenance_runs WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_work WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_findings WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_evidence WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_dispositions WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_requests WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_state WHERE catalog_id = $1)",
        )
        .bind(first)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(populated, (1, 2, 1, 1, 1, 1, 1));
        let revision_delete = sqlx::query(
            "DELETE FROM revisions WHERE catalog_id = $1 AND path = 'a.md' AND revision = 1",
        )
        .bind(first)
        .execute(&db.pool)
        .await;
        assert!(
            revision_delete.is_err(),
            "referenced revision must be retained"
        );
        let retained: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM revisions WHERE catalog_id = $1 AND path = 'a.md'), \
             (SELECT count(*) FROM maintenance_work WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_findings WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_evidence WHERE catalog_id = $1)",
        )
        .bind(first)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(retained, (1, 2, 1, 1));
        let retained_history: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM maintenance_dispositions WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_requests WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_state WHERE catalog_id = $1)",
        )
        .bind(first)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(retained_history, (1, 1, 1));
        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(first)
            .execute(&db.pool)
            .await
            .unwrap();
        let remaining: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM maintenance_runs WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_work WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_findings WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_evidence WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_dispositions WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_requests WHERE catalog_id = $1), \
             (SELECT count(*) FROM maintenance_state WHERE catalog_id = $1)",
        )
        .bind(first)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(remaining, (0, 0, 0, 0, 0, 0, 0));
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

        // This fixture stops at 0005; current key readers require later columns.
        let (id, name, sha256): (String, String, Vec<u8>) =
            sqlx::query_as("SELECT id, name, sha256 FROM api_keys")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!((id.as_str(), name.as_str()), ("legacy", "legacy"));
        assert!(crate::keys::matches(&sha256, secret));
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

    /// The leak behind issue #187, made deterministic: a zero startup timeout
    /// expires after Docker created the container but before testcontainers
    /// returned a handle, so testcontainers alone leaves it behind.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_timed_out_start_leaks_without_cleanup_and_remove_labelled_reaps_it() {
        use testcontainers_modules::testcontainers::runners::AsyncRunner;
        use testcontainers_modules::testcontainers::ImageExt;

        let label = pg_support::unique_start_label();
        let started = pg_support::postgres_17()
            .with_label(pg_support::START_LABEL, &label)
            .with_startup_timeout(std::time::Duration::ZERO)
            .start()
            .await;

        assert!(started.is_err(), "a zero startup timeout must fail");
        assert_eq!(
            pg_support::labelled_containers(&label).await.unwrap().len(),
            1
        );
        assert_eq!(pg_support::remove_labelled(&label).await.unwrap(), 1);
        assert!(pg_support::labelled_containers(&label)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_timed_out_start_labelled_leaves_no_container() {
        use testcontainers_modules::testcontainers::core::error::WaitContainerError;
        use testcontainers_modules::testcontainers::{ImageExt, TestcontainersError};

        let label = pg_support::unique_start_label();
        let request = pg_support::postgres_17().with_startup_timeout(std::time::Duration::ZERO);

        let started = pg_support::start_labelled(request, &label).await;

        assert!(matches!(
            started,
            Err(TestcontainersError::WaitContainer(
                WaitContainerError::StartupTimeout
            ))
        ));
        assert!(pg_support::labelled_containers(&label)
            .await
            .unwrap()
            .is_empty());
    }
}
