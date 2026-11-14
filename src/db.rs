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

/// Applies every okf migration not yet applied, then apalis' own (both are
/// no-ops when up to date).
///
/// apalis-postgres keeps its tables in schema `apalis` and its history in
/// `apalis._sqlx_migrations`, apart from okf's `public._sqlx_migrations`, so
/// neither migrator ever sees the other's rows. Its migrations create the
/// `pgcrypto` and `hstore` extensions, which (like `citext`) are trusted:
/// the database owner may create them.
///
/// # Errors
/// [`OkfError::Migrate`] if a migration fails, or the database has a
/// migration this binary does not know (it is older than the schema);
/// [`OkfError::Database`] if apalis' setup cannot reach the database.
pub async fn migrate(pool: &PgPool) -> Result<(), OkfError> {
    MIGRATOR.run(pool).await?;
    // `::<()>`: `setup` is defined on `PostgresStorage<()>`; the job type
    // a storage carries does not matter for creating the schema.
    apalis_postgres::PostgresStorage::<()>::setup(pool)
        .await
        .map_err(apalis_error)?;
    Ok(())
}

/// An apalis setup failure as an [`OkfError`].
///
/// # Arguments
/// * `error` - what `PostgresStorage::setup` returned.
///
/// # Returns
/// [`OkfError::Migrate`] for a migration failure (including apalis' own
/// "upgrade required" refusal), [`OkfError::Database`] (or `Busy`) for a
/// driver error.
fn apalis_error(error: apalis_postgres::Error) -> OkfError {
    match error {
        // apalis wraps its migrator's error in a driver error; unwrap it.
        // `*source` moves the `MigrateError` out of its `Box`.
        apalis_postgres::Error::Database(sqlx::Error::Migrate(source)) => {
            OkfError::Migrate { source: *source }
        }
        apalis_postgres::Error::Database(source) => OkfError::from(source),
        // `setup` returns `UpgradeRequired` for a pre-1.0 apalis history; the
        // other variants are about running jobs, not setup. Each keeps its
        // text. Listed rather than `_`, so a new apalis variant is a compile
        // error here until someone decides what it means. `other @ (...)`
        // binds the whole matched value to `other`.
        other @ (apalis_postgres::Error::UpgradeRequired { .. }
        | apalis_postgres::Error::JsonError(_)
        | apalis_postgres::Error::ReenqueueMismatch { .. }
        | apalis_postgres::Error::TaskIdError(_)
        | apalis_postgres::Error::StatusError(_)
        | apalis_postgres::Error::WorkerOutOfSync
        | apalis_postgres::Error::WorkerAlreadyExists(_)) => OkfError::Migrate {
            source: sqlx::migrate::MigrateError::Source(Box::new(std::io::Error::other(
                other.to_string(),
            ))),
        },
    }
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

    use apalis_postgres::PostgresStorage;

    /// How many migrations `apalis._sqlx_migrations` records.
    async fn apalis_history(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM apalis._sqlx_migrations")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// How many migrations okf's own `_sqlx_migrations` records.
    async fn okf_history(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migrations_create_the_schema_and_rerun_as_a_no_op() {
        // A bare database, not `test_db()`: its fixture user would bump the
        // admin generation this test expects migrations to leave at zero.
        let db = pg_support::TestDb::start().await;
        migrate(&db.pool).await.unwrap();
        let apalis_once = apalis_history(&db.pool).await;

        migrate(&db.pool).await.unwrap();

        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations' \
             ORDER BY table_name COLLATE \"C\"",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            [
                "api_keys",
                "audit_access_counts",
                "audit_credentials",
                "audit_entries",
                "audit_people",
                "audit_teams",
                "catalogs",
                "changeset_entries",
                "changesets",
                "documents",
                "email_tokens",
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
                "server_settings",
                "server_state",
                "sign_in_code_catalogs",
                "sign_in_codes",
                "sign_in_events",
                "team_members",
                "teams",
                "user_catalogs",
                "user_identities",
                "user_passkeys",
                "user_sessions",
                "users",
            ]
        );
        // apalis keeps everything in its own schema, with its own history.
        let apalis_tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'apalis' ORDER BY table_name COLLATE \"C\"",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        for table in ["_sqlx_migrations", "jobs", "workers"] {
            assert!(
                apalis_tables.iter().any(|name| name == table),
                "{apalis_tables:?}"
            );
        }
        let embedded = i64::try_from(PostgresStorage::<()>::migrations().iter().count()).unwrap();
        assert_eq!(apalis_once, embedded, "every apalis migration applied once");
        assert_eq!(
            apalis_history(&db.pool).await,
            apalis_once,
            "the rerun is a no-op"
        );
        assert_eq!(
            okf_history(&db.pool).await,
            i64::try_from(MIGRATOR.iter().count()).unwrap(),
            "okf's history holds only okf's migrations"
        );
        let admin_generation: i64 = sqlx::query_scalar("SELECT admin_generation FROM server_state")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(admin_generation, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migration_0010_keeps_every_row_and_both_migrators_rerun_as_no_ops() {
        // Arrange: a database the real migrator took to 0009 (so its history
        // is what a v0.14 server left), with a user in her personal team,
        // a catalog that team owns, a key with one grant and an OAuth grant.
        let db = pg_support::TestDb::start().await;
        MIGRATOR.run_to(9, &db.pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO users (name, created_at) VALUES ('alice', now());
             INSERT INTO teams (name, personal_of, created_at)
                 SELECT 'alice', id, now() FROM users WHERE name = 'alice';
             INSERT INTO team_members (team_id, user_id, role, created_at)
                 SELECT t.id, u.id, 'owner', now() FROM teams t, users u
                 WHERE t.name = 'alice' AND u.name = 'alice';
             INSERT INTO catalogs (name, created_at, owner_team_id)
                 SELECT 'pg', now(), id FROM teams WHERE name = 'alice';
             INSERT INTO api_keys (name, sha256, access, all_catalogs, actor, created_at, user_id)
                 SELECT 'laptop', decode(repeat('ab', 32), 'hex'), 'write', false, 'alice',
                        now(), id
                 FROM users WHERE name = 'alice';
             INSERT INTO key_catalogs (key_id, catalog_id)
                 SELECT k.id, c.id FROM api_keys k, catalogs c
                 WHERE k.name = 'laptop' AND c.name = 'pg';
             INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at)
                 VALUES ('c1', 'dcr', '{https://client.example.com/cb}', '{}', now());
             INSERT INTO api_keys (name, kind, client_id, access, all_catalogs, actor, created_at)
                 VALUES ('laptop.grant', 'oauth', 'c1', 'read', true, 'alice', now());",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        assert_eq!(okf_history(&db.pool).await, 9);

        // Act: what a v0.15 server does on start, twice.
        migrate(&db.pool).await.unwrap();
        migrate(&db.pool).await.unwrap();

        // Assert: nothing existing changed meaning.
        // `email::text`: decode citext as plain text.
        let user: (
            Option<String>,
            bool,
            Option<String>,
            i32,
            Option<chrono::DateTime<chrono::Utc>>,
        ) = sqlx::query_as(
            "SELECT email::text, is_admin, password_hash, failed_sign_ins, locked_until \
                 FROM users WHERE name = 'alice'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(user, (None, false, None, 0, None));
        let keys: Vec<(String, bool, Option<chrono::DateTime<chrono::Utc>>)> =
            sqlx::query_as("SELECT name, self_issued, expires_at FROM api_keys ORDER BY name")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            keys,
            [
                ("laptop".to_string(), false, None),
                ("laptop.grant".to_string(), false, None)
            ]
        );
        let level: Option<String> = sqlx::query_scalar("SELECT access FROM key_catalogs")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(level, None, "an existing grant keeps the key's own level");
        let member_access: String =
            sqlx::query_scalar("SELECT member_access FROM catalogs WHERE name = 'pg'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(member_access, "write");
        let new_rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM user_identities) + (SELECT count(*) FROM user_passkeys) \
                  + (SELECT count(*) FROM user_sessions) + (SELECT count(*) FROM email_tokens) \
                  + (SELECT count(*) FROM sign_in_events) + (SELECT count(*) FROM user_catalogs) \
                  + (SELECT count(*) FROM server_settings)",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(new_rows, 0, "the migration creates no rows");
        assert_eq!(
            okf_history(&db.pool).await,
            i64::try_from(MIGRATOR.iter().count()).unwrap()
        );
        assert_eq!(
            apalis_history(&db.pool).await,
            i64::try_from(PostgresStorage::<()>::migrations().iter().count()).unwrap()
        );
    }

    /// Inserts an API key row with these columns, named `name` (its hash is
    /// derived from the name, so each call is distinct).
    async fn insert_key(
        pool: &PgPool,
        name: &str,
        flags: (&str, bool, bool, bool),
        user_id: Option<&str>,
    ) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
        // A tuple keeps this at five parameters: (access, maintenance_report,
        // all_catalogs, self_issued).
        let (access, report, all, self_issued) = flags;
        sqlx::query(
            "INSERT INTO api_keys \
             (name, sha256, access, maintenance_report, all_catalogs, actor, created_at, \
              user_id, self_issued) \
             VALUES ($1, sha256($1::bytea), $2, $3, $4, $1, now(), $5, $6)",
        )
        .bind(name)
        .bind(access)
        .bind(report)
        .bind(all)
        .bind(user_id)
        .bind(self_issued)
        .execute(pool)
        .await
    }

    /// The constraint a refused statement broke.
    fn broken<T>(result: Result<T, sqlx::Error>) -> Option<String> {
        result.err().and_then(|error| {
            error
                .as_database_error()
                .and_then(|e| e.constraint().map(str::to_string))
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migration_0010_constraints_hold() {
        let db = test_db().await;
        sqlx::raw_sql("INSERT INTO catalogs (name, created_at) VALUES ('pg', now());")
            .execute(&db.pool)
            .await
            .unwrap();
        let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(TEST_USER)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let user = Some(user_id.as_str());

        // A self-issued key: read or write, never a reporter, never every
        // catalog, always a user's.
        insert_key(&db.pool, "mine", ("write", false, false, true), user)
            .await
            .unwrap();
        for (name, flags) in [
            ("admin", ("admin", false, false, true)),
            ("reporter", ("read", true, false, true)),
            ("everything", ("read", false, true, true)),
        ] {
            assert_eq!(
                broken(insert_key(&db.pool, name, flags, user).await).as_deref(),
                Some("api_keys_self_issued_caps"),
                "{name}"
            );
        }
        assert_eq!(
            broken(insert_key(&db.pool, "orphan", ("read", false, false, true), None).await)
                .as_deref(),
            Some("api_keys_self_issued_user")
        );
        // Admin-issued keys are as before.
        insert_key(&db.pool, "root", ("admin", true, true, false), None)
            .await
            .unwrap();

        // Per-catalog levels are read or write; NULL is the key's own.
        let grant = |access: Option<&'static str>| {
            let pool = db.pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO key_catalogs (key_id, catalog_id, access) \
                     SELECT k.id, c.id, $1 FROM api_keys k, catalogs c \
                     WHERE k.name = 'root' AND c.name = 'pg' \
                     ON CONFLICT (key_id, catalog_id) DO UPDATE SET access = EXCLUDED.access",
                )
                .bind(access)
                .execute(&pool)
                .await
            }
        };
        grant(None).await.unwrap();
        grant(Some("read")).await.unwrap();
        assert_eq!(
            broken(grant(Some("admin")).await).as_deref(),
            Some("key_catalogs_access_level")
        );

        // Entitlements, member access, tokens and events take only their values.
        let checks = [
            (
                "INSERT INTO user_catalogs (user_id, catalog_id, access, created_at) \
                 SELECT $1, id, 'admin', now() FROM catalogs WHERE name = 'pg'",
                "user_catalogs_access_check",
            ),
            (
                "UPDATE catalogs SET member_access = 'owner' WHERE name = 'pg' AND $1 <> ''",
                "catalogs_member_access_check",
            ),
            (
                "INSERT INTO email_tokens (hash, user_id, purpose, created_at, expires_at) \
                 VALUES (sha256('t'::bytea), $1, 'login', now(), now())",
                "email_tokens_purpose_check",
            ),
            (
                "INSERT INTO sign_in_events (user_id, method, outcome, at) \
                 VALUES ($1, 'password', 'maybe', now())",
                "sign_in_events_outcome_check",
            ),
        ];
        for (statement, constraint) in checks {
            let result = sqlx::query(statement)
                .bind(&user_id)
                .execute(&db.pool)
                .await;
            assert_eq!(broken(result).as_deref(), Some(constraint), "{statement}");
        }

        // Email addresses are unique ignoring case (citext).
        sqlx::query("UPDATE users SET email = 'Alice@Example.com' WHERE id = $1")
            .bind(&user_id)
            .execute(&db.pool)
            .await
            .unwrap();
        let clash = sqlx::query(
            "INSERT INTO users (name, email, created_at) VALUES ('bob', 'alice@example.com', now())",
        )
        .execute(&db.pool)
        .await;
        assert_eq!(broken(clash).as_deref(), Some("users_email_key"));
    }

    #[test]
    fn an_apalis_setup_error_is_a_migrate_or_database_error() {
        let upgrade = apalis_error(apalis_postgres::Error::UpgradeRequired {
            current: "0.7.4".into(),
            required: "1.0.0".into(),
            link: None,
        });
        let database = apalis_error(apalis_postgres::Error::Database(sqlx::Error::PoolClosed));
        let migrate = apalis_error(apalis_postgres::Error::Database(sqlx::Error::Migrate(
            Box::new(sqlx::migrate::MigrateError::VersionMismatch(1)),
        )));

        assert!(matches!(upgrade, OkfError::Migrate { .. }), "{upgrade:?}");
        assert!(
            matches!(database, OkfError::Database { .. }),
            "{database:?}"
        );
        assert!(
            matches!(
                migrate,
                OkfError::Migrate {
                    source: sqlx::migrate::MigrateError::VersionMismatch(1)
                }
            ),
            "{migrate:?}"
        );
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
