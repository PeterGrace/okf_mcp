//! Tests of the test-database helper itself (`tests/support/pg.rs`): the
//! migrated template is built once however many processes race for it, a
//! half-built one is rebuilt, a killed run's databases are dropped by a later
//! one, and a dropped `TestDb` drops its database.
//!
//! Each test works on the shared server when `TEST_PG_URL` names one (as
//! `just test` does), else on a container of its own.

mod support;

use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use sqlx::Connection;
use support::pg::{self, TestDb};

/// The environment variable that turns [`template_race_child`] from a no-op
/// into one racer: it names the template to race for.
const RACE_TEMPLATE_VAR: &str = "OKF_TEST_RACE_TEMPLATE";

/// How many processes race for the template.
const RACERS: usize = 4;

/// Whether `name` exists on `server_url`.
async fn database_exists(server_url: &str, name: &str) -> bool {
    let mut admin = pg::connect_admin(server_url).await.unwrap();
    let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM pg_database WHERE datname = $1")
        .bind(name)
        .fetch_optional(&mut admin)
        .await
        .unwrap();
    admin.close().await.unwrap();
    found.is_some()
}

/// Drops the template `name` (a template must be unmarked first).
async fn drop_template(server_url: &str, name: &str) {
    let mut admin = pg::connect_admin(server_url).await.unwrap();
    let statement = format!("ALTER DATABASE {} IS_TEMPLATE false", pg::quoted(name));
    pg::execute_ddl(&mut admin, statement).await.unwrap();
    admin.close().await.unwrap();
    pg::drop_database(server_url, name).await.unwrap();
}

/// A template name of this test's own, so the race really builds one.
fn own_template_name(purpose: &str) -> String {
    let unique = pg::new_database_name(SystemTime::now());
    format!(
        "okf_tpl_{purpose}_{}",
        unique.trim_start_matches(pg::DATABASE_PREFIX)
    )
}

/// The migrations a copy of `template` has applied.
async fn migrations_in_a_copy_of(server_url: &str, template: &str) -> i64 {
    let copy = pg::new_database_name(SystemTime::now());
    pg::create_database(server_url, &copy, Some(template))
        .await
        .unwrap();
    let mut connection = sqlx::PgConnection::connect(&format!("{server_url}/{copy}"))
        .await
        .unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    pg::drop_database(server_url, &copy).await.unwrap();
    applied
}

/// One racer, run as a separate process by [`racing_processes_build_the_template_once`]:
/// ensures the template, prints whether it built it, and checks a copy is
/// fully migrated. A no-op unless [`RACE_TEMPLATE_VAR`] is set.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a child process of racing_processes_build_the_template_once"]
async fn template_race_child() {
    let Ok(template) = std::env::var(RACE_TEMPLATE_VAR) else {
        return;
    };
    let server_url = pg::shared_server_url().expect("the parent passes the server");

    let built = pg::ensure_template(&server_url, &template).await.unwrap();

    println!("built={built}");
    let applied = migrations_in_a_copy_of(&server_url, &template).await;
    assert_eq!(applied, pg::migration_count() as i64);
}

#[tokio::test(flavor = "multi_thread")]
async fn racing_processes_build_the_template_once() {
    // Arrange: one server, a template nobody has built, and this test
    // binary's path, to start the racers from.
    let (server_url, _container) = pg::server().await;
    let template = own_template_name("race");
    let binary = std::env::current_exe().unwrap();

    // Act: start every racer before waiting on any, so they overlap.
    let racers: Vec<_> = (0..RACERS)
        .map(|_| {
            Command::new(&binary)
                .args(["--exact", "template_race_child", "--ignored", "--nocapture"])
                .env(pg::SERVER_URL_VAR, &server_url)
                .env(RACE_TEMPLATE_VAR, &template)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<_> = racers
        .into_iter()
        .map(|racer| racer.wait_with_output().unwrap())
        .collect();

    // Assert: every racer passed, and exactly one built the template.
    for output in &outputs {
        assert!(
            output.status.success(),
            "a racer failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let builders = outputs
        .iter()
        .filter(|output| String::from_utf8_lossy(&output.stdout).contains("built=true"))
        .count();
    let waiters = outputs
        .iter()
        .filter(|output| String::from_utf8_lossy(&output.stdout).contains("built=false"))
        .count();
    assert_eq!((builders, waiters), (1, RACERS - 1));
    drop_template(&server_url, &template).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_built_template_is_dropped_and_rebuilt() {
    // Arrange: a database of the template's name that was never migrated
    // or marked a template, as a killed builder leaves it.
    let (server_url, _container) = pg::server().await;
    let template = own_template_name("half");
    pg::create_database(&server_url, &template, None)
        .await
        .unwrap();

    // Act
    let built = pg::ensure_template(&server_url, &template).await.unwrap();
    let built_again = pg::ensure_template(&server_url, &template).await.unwrap();

    // Assert: rebuilt complete once, then left alone.
    assert!(built);
    assert!(!built_again);
    let applied = migrations_in_a_copy_of(&server_url, &template).await;
    assert_eq!(applied, pg::migration_count() as i64);
    drop_template(&server_url, &template).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_runs_databases_are_dropped_and_fresh_ones_kept() {
    // Arrange: a database created two hours ago (a killed run's) and one
    // created now (a test still running).
    let (server_url, _container) = pg::server().await;
    let now = SystemTime::now();
    let stale = pg::new_database_name(now - Duration::from_secs(2 * 60 * 60));
    let fresh = pg::new_database_name(now);
    for name in [&stale, &fresh] {
        pg::create_database(&server_url, name, None).await.unwrap();
    }

    // Act
    let dropped = pg::drop_stale_databases(&server_url, now).await.unwrap();

    // Assert: the stale one is gone (this call, or another test's
    // once-per-process sweep running at the same time, dropped it) and the
    // fresh one is kept.
    assert!(!dropped.contains(&fresh));
    assert!(!database_exists(&server_url, &stale).await);
    assert!(database_exists(&server_url, &fresh).await);
    pg::drop_database(&server_url, &fresh).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_test_db_drops_its_database_on_the_shared_server() {
    // Without a shared server the container goes, and the database with it.
    let Some(server_url) = pg::shared_server_url() else {
        return;
    };
    // Arrange: a migrated database with a connection open, as a test that
    // panicked mid-query leaves it.
    let db = TestDb::start_migrated().await;
    let name = db.url.rsplit('/').next().unwrap().to_string();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(applied, pg::migration_count() as i64);

    // Act
    drop(db);

    // Assert
    assert!(!database_exists(&server_url, &name).await);
}

#[test]
fn a_database_name_carries_its_creation_time() {
    let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let first = pg::new_database_name(created);
    let second = pg::new_database_name(created);

    assert_ne!(first, second, "two names in one second must differ");
    assert_eq!(pg::database_created_at(&first), Some(1_700_000_000));
    assert!(first.len() <= 63, "postgres truncates names past 63 bytes");
}

#[test]
fn names_that_are_not_test_databases_have_no_creation_time() {
    for name in ["postgres", "template1", "okf_tpl_0123", "test_", "test_x_1"] {
        assert_eq!(pg::database_created_at(name), None, "{name}");
    }
}

#[test]
fn the_template_name_is_stable_and_valid() {
    let name = pg::template_name();

    assert_eq!(name, pg::template_name());
    assert!(name.starts_with("okf_tpl_"));
    assert_eq!(name.len(), "okf_tpl_".len() + 16);
}
