//! A throwaway Postgres database for each test, dropped with its [`TestDb`].
//! The library's unit tests include this same file (`#[path]` in
//! `src/db.rs`), so there is one copy.
//!
//! **The server.** With [`SERVER_URL_VAR`] set, as `scripts/test-pg.sh run`
//! (`just test`) sets it, every test in every process shares that one server
//! and gets a database of its own on it. Unset, each [`TestDb`] starts its own
//! container, removed when the [`TestDb`] is dropped, so a plain `cargo test`
//! needs nothing running. The shared server is started outside the test
//! processes because Rust never drops statics: a container held in one would
//! never be removed, and under `cargo nextest` (a process per test) a
//! per-process server is a server per test.
//!
//! **Migrations run once per server.** [`template_name`] hashes the
//! migrations (okf's and apalis-postgres', which `okf_mcp::db::migrate` runs
//! after okf's), and [`ensure_template`] builds a database of that name under a
//! session advisory lock: concurrent processes race safely, and a server kept
//! running across schema changes gets a new template rather than serving a
//! stale one. A migrated test database ([`TestDb::start_migrated`]) is a
//! `CREATE DATABASE ... TEMPLATE` copy of it, much cheaper than migrating.
//!
//! **Cleanup.** A test's database is dropped `WITH (FORCE)` when its
//! [`TestDb`] is dropped, even when the test panicked. A killed process drops
//! nothing, so each process first drops the `test_*` databases older than
//! [`STALE_AFTER`] ([`drop_stale_databases`]); the name carries its creation
//! time ([`new_database_name`]).
//!
//! **Containers.** A start that fails after Docker created the container
//! (the start times out under load, or Docker refuses it with "endpoint ...
//! already exists") returns no [`ContainerAsync`], so nothing would remove
//! it. Every container therefore carries a label unique to its start, and
//! [`start_labelled`] removes whatever has that label when the start fails.
//! Tests that need a customized container (`tests/tls.rs`) start it through
//! [`start_with_retry`] too.

// Each crate that includes this file uses a different part of it (the CLI
// tests need `url`, the unit tests only `pool`).
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnection, PgPool, PgPoolOptions};
use sqlx::Connection;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::bollard::errors::Error as DockerError;
use testcontainers_modules::testcontainers::bollard::query_parameters::{
    ListContainersOptions, RemoveContainerOptions,
};
use testcontainers_modules::testcontainers::bollard::Docker;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, ImageExt, TestcontainersError,
};
use tokio::sync::OnceCell;

/// The label key whose value identifies one start of a test container.
pub const START_LABEL: &str = "okf-mcp.test-start";

/// How long one start may take before it counts as failed (testcontainers'
/// own default).
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Attempts per container start: one retry covers a transient Docker
/// refusal or a start that timed out under load.
const START_ATTEMPTS: u32 = 2;

/// The environment variable naming a running server every test shares: a
/// `postgres://user:password@host:port` URL with no database name.
pub const SERVER_URL_VAR: &str = "TEST_PG_URL";

/// The age past which a `test_*` database belongs to a killed run, not to a
/// test still running, and is dropped.
pub const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// The first part of every test database's name.
pub const DATABASE_PREFIX: &str = "test_";

/// The session advisory lock key that serializes template builds on a
/// server (any constant every process agrees on: "okf-tpl" in ASCII).
const TEMPLATE_LOCK_KEY: i64 = 0x006f_6b66_2d74_706c;

/// The pool size of each test's own connection to its database: 8, so a
/// mail test's worker (a LISTEN connection, plus fetches, acks and
/// heartbeats) and the test itself both fit (Decision 19).
const POOL_SIZE: u32 = 8;

/// The schema migrations, embedded here as in `okf_mcp::db::MIGRATOR` (this
/// file cannot name that: it is `crate::db` in the library's unit tests and
/// `okf_mcp::db` in the integration tests).
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// apalis-postgres' migrations (schema `apalis`, history in
/// `apalis._sqlx_migrations`), which `okf_mcp::db::migrate` runs after
/// [`MIGRATOR`].
fn apalis_migrator() -> Migrator {
    // `::<()>`: the job type a storage carries does not matter for its schema.
    apalis_postgres::PostgresStorage::<()>::migrations()
}

/// The shared server's template, prepared once per process. A
/// `tokio::sync::OnceCell` runs its async initializer once, however many
/// tests ask at the same time (like a lock-guarded lazy global in Python).
static SHARED_TEMPLATE: OnceCell<String> = OnceCell::const_new();

/// A database of a test's own and a pool connected to it, with up to 8
/// connections.
pub struct TestDb {
    /// Connected to the test's database.
    pub pool: PgPool,
    /// The connection URL, for code that connects by itself (the CLI).
    pub url: String,
    // Fields drop in declaration order: the pool closes before the database
    // is dropped (and, without a shared server, its container removed).
    _database: Database,
}

impl TestDb {
    /// Creates an empty database (no migrations), for tests of migrating.
    ///
    /// # Panics
    /// If no server can be reached or started (Docker unreachable, or a
    /// container fails to start twice), or the database cannot be created:
    /// a test environment problem, reported at once.
    pub async fn start() -> Self {
        Self::create(false).await
    }

    /// Creates a migrated database, a copy of the server's template.
    ///
    /// # Panics
    /// As [`TestDb::start`], or if the template cannot be built.
    pub async fn start_migrated() -> Self {
        Self::create(true).await
    }

    /// Creates the test's database, migrated when `migrated`: on the shared
    /// server, a new database (a copy of the template); on a container of
    /// the test's own, its `postgres` database, migrated in place (a
    /// template would be built for one copy).
    async fn create(migrated: bool) -> Self {
        let (server_url, container) = server().await;
        let name = match &container {
            Some(_) => "postgres".to_string(),
            None => {
                // The shared server is prepared (template built, stale
                // databases dropped) once per process, whether or not this
                // test migrates.
                let template = shared_template(&server_url).await;
                let name = new_database_name(SystemTime::now());
                create_database(&server_url, &name, migrated.then_some(template.as_str()))
                    .await
                    .expect("create the test database");
                name
            }
        };
        let url = format!("{server_url}/{name}");
        let pool = PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .connect(&url)
            .await
            .expect("connect to the test database");
        if migrated && container.is_some() {
            MIGRATOR.run(&pool).await.expect("migrations apply");
            apalis_migrator()
                .run(&pool)
                .await
                .expect("apalis migrations apply");
        }
        Self {
            pool,
            url,
            _database: Database {
                server_url,
                name,
                container,
            },
        }
    }
}

/// A test's database; dropping it drops the database (on the shared server)
/// or removes the container that holds it.
struct Database {
    server_url: String,
    name: String,
    /// The container of this test's own, when no server is shared.
    container: Option<ContainerAsync<Postgres>>,
}

// `Drop` runs when the value goes out of scope (like `__del__`, but
// deterministic), including while a failed test unwinds.
impl Drop for Database {
    fn drop(&mut self) {
        if self.container.is_some() {
            // The container's own `Drop` removes the whole server.
            return;
        }
        let (server_url, name) = (self.server_url.clone(), self.name.clone());
        // `drop` cannot `.await`, and blocking on the test's own runtime from
        // inside it would deadlock a single-threaded one, so the DROP runs on
        // a thread with a runtime of its own, joined before `drop` returns.
        let dropped = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            runtime
                .block_on(drop_database(&server_url, &name))
                .map_err(|error| error.to_string())
        })
        .join();
        // A failure here must not panic (a panic during unwinding aborts the
        // process); a database left behind is dropped by a later run.
        match dropped {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!("drop test database {}: {error}", self.name),
            Err(_) => tracing::warn!("drop test database {}: the thread panicked", self.name),
        }
    }
}

/// The URL of the shared server, if [`SERVER_URL_VAR`] names one.
///
/// # Returns
/// The URL without a trailing `/`, or `None` when the variable is unset or
/// empty.
pub fn shared_server_url() -> Option<String> {
    std::env::var(SERVER_URL_VAR)
        .ok()
        .map(|url| url.trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

/// A server for one test: the shared one, or a container of its own.
///
/// # Returns
/// The server URL (no database name) and, without a shared server, the
/// container, which removes itself when dropped.
///
/// # Panics
/// As [`start_with_retry`].
pub async fn server() -> (String, Option<ContainerAsync<Postgres>>) {
    if let Some(url) = shared_server_url() {
        return (url, None);
    }
    let container = start_with_retry(postgres_17).await;
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");
    let url = format!("postgres://postgres:postgres@{host}:{port}");
    (url, Some(container))
}

/// The shared server's template name, after this process's first call has
/// built it if needed and dropped stale databases.
async fn shared_template(server_url: &str) -> String {
    let prepared = SHARED_TEMPLATE.get_or_init(|| async {
        let name = template_name();
        ensure_template(server_url, &name)
            .await
            .expect("build the migrated template database");
        drop_stale_databases(server_url, SystemTime::now())
            .await
            .expect("drop stale test databases");
        name
    });
    prepared.await.clone()
}

/// The migrated template's name: `okf_tpl_` and a hash of every migration's
/// version and checksum, so a schema change gets a template of its own.
pub fn template_name() -> String {
    let mut hasher = Sha256::new();
    for migration in MIGRATOR.iter().chain(apalis_migrator().iter()) {
        hasher.update(migration.version.to_le_bytes());
        hasher.update(&migration.checksum);
    }
    let digest = hasher.finalize();
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("okf_tpl_{hex}")
}

/// How many okf migrations a migrated database has applied (in
/// `public._sqlx_migrations`; apalis keeps its own history).
pub fn migration_count() -> usize {
    MIGRATOR.iter().count()
}

/// Makes `name` a complete migrated template on `server_url`, building it
/// unless another process already has.
///
/// The check and the build run under a session advisory lock, so of several
/// processes racing, one builds and the rest wait, then find it complete. A
/// template is marked complete (`IS_TEMPLATE`) only once migrated, so one
/// whose builder was killed is dropped and built again.
///
/// # Arguments
/// * `server_url` - the server, with no database name.
/// * `name` - the template's name ([`template_name`], or a test's own).
///
/// # Returns
/// Whether this call built it.
///
/// # Errors
/// If the server is unreachable or a statement or migration fails. The lock
/// is released either way: it ends with the session.
pub async fn ensure_template(server_url: &str, name: &str) -> Result<bool, sqlx::Error> {
    let mut admin = connect_admin(server_url).await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut admin)
        .await?;
    let complete: Option<bool> =
        sqlx::query_scalar("SELECT datistemplate FROM pg_database WHERE datname = $1")
            .bind(name)
            .fetch_optional(&mut admin)
            .await?;
    let built = match complete {
        Some(true) => false,
        Some(false) => {
            drop_database_on(&mut admin, name).await?;
            build_template(&mut admin, server_url, name).await?;
            true
        }
        None => {
            build_template(&mut admin, server_url, name).await?;
            true
        }
    };
    // Closing the session releases the advisory lock.
    admin.close().await?;
    Ok(built)
}

/// Creates `name`, migrates it (okf's migrations, then apalis'), then marks
/// it a template.
async fn build_template(
    admin: &mut PgConnection,
    server_url: &str,
    name: &str,
) -> Result<(), sqlx::Error> {
    execute_ddl(admin, format!("CREATE DATABASE {}", quoted(name))).await?;
    let mut template = PgConnection::connect(&format!("{server_url}/{name}")).await?;
    MIGRATOR.run(&mut template).await?;
    apalis_migrator().run(&mut template).await?;
    // A copy needs the template to have no connections.
    template.close().await?;
    execute_ddl(
        admin,
        format!("ALTER DATABASE {} IS_TEMPLATE true", quoted(name)),
    )
    .await
}

/// A new test database name: [`DATABASE_PREFIX`], the creation time in Unix
/// seconds, the process id and a counter (unique across processes).
///
/// # Arguments
/// * `now` - the creation time.
pub fn new_database_name(now: SystemTime) -> String {
    let seconds = unix_seconds(now);
    format!(
        "{DATABASE_PREFIX}{seconds}_{}",
        unique_start_label().replace('-', "_")
    )
}

/// When a test database was created, from its name.
///
/// # Returns
/// Unix seconds, or `None` if `name` is not a [`new_database_name`].
pub fn database_created_at(name: &str) -> Option<u64> {
    name.strip_prefix(DATABASE_PREFIX)?
        .split('_')
        .next()?
        .parse()
        .ok()
}

/// Drops every test database created more than [`STALE_AFTER`] before `now`.
///
/// A drop that fails is logged and skipped, not returned: another process
/// sweeping at the same moment may be dropping the same database.
///
/// # Returns
/// The names this call dropped.
///
/// # Errors
/// If the server is unreachable or the databases cannot be listed.
pub async fn drop_stale_databases(
    server_url: &str,
    now: SystemTime,
) -> Result<Vec<String>, sqlx::Error> {
    let cutoff = unix_seconds(now).saturating_sub(STALE_AFTER.as_secs());
    let mut admin = connect_admin(server_url).await?;
    let names: Vec<String> = sqlx::query_scalar("SELECT datname FROM pg_database")
        .fetch_all(&mut admin)
        .await?;
    let stale: Vec<String> = names
        .into_iter()
        .filter(|name| database_created_at(name).is_some_and(|created| created < cutoff))
        .collect();
    let mut dropped = Vec::with_capacity(stale.len());
    for name in stale {
        match drop_database_on(&mut admin, &name).await {
            Ok(()) => dropped.push(name),
            Err(error) => tracing::warn!("drop stale test database {name}: {error}"),
        }
    }
    admin.close().await?;
    Ok(dropped)
}

/// Creates `name` on `server_url`, a copy of `template` when given (else
/// an empty database).
///
/// # Errors
/// If the server is unreachable or the create fails.
pub async fn create_database(
    server_url: &str,
    name: &str,
    template: Option<&str>,
) -> Result<(), sqlx::Error> {
    let statement = match template {
        Some(template) => format!(
            "CREATE DATABASE {} TEMPLATE {}",
            quoted(name),
            quoted(template)
        ),
        None => format!("CREATE DATABASE {}", quoted(name)),
    };
    let mut admin = connect_admin(server_url).await?;
    execute_ddl(&mut admin, statement).await?;
    admin.close().await
}

/// Drops `name` on `server_url`, ending its sessions (`WITH (FORCE)`); a
/// database already gone is fine.
///
/// # Errors
/// If the server is unreachable or the drop fails.
pub async fn drop_database(server_url: &str, name: &str) -> Result<(), sqlx::Error> {
    let mut admin = connect_admin(server_url).await?;
    drop_database_on(&mut admin, name).await?;
    admin.close().await
}

/// As [`drop_database`], over an open connection to the server.
async fn drop_database_on(admin: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    let statement = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", quoted(name));
    execute_ddl(admin, statement).await
}

/// Runs a statement about a whole database (`CREATE`, `ALTER` or `DROP
/// DATABASE`) over `admin`.
///
/// These statements cannot take a bind parameter for the name, so callers
/// format it in through [`quoted`], which accepts only the names this file
/// generates and tests' literals; that is the audit `AssertSqlSafe` asks for.
///
/// # Errors
/// If the statement fails.
pub async fn execute_ddl(admin: &mut PgConnection, statement: String) -> Result<(), sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(statement))
        .execute(admin)
        .await?;
    Ok(())
}

/// `name` as a quoted SQL identifier.
///
/// # Panics
/// If `name` holds anything but lowercase ASCII letters, digits and `_`:
/// every database name here is generated, so another is a bug in a test.
pub fn quoted(name: &str) -> String {
    let generated = name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    assert!(generated, "not a generated database name: {name:?}");
    format!("\"{name}\"")
}

/// A connection to the server's `postgres` database, from which databases
/// are created and dropped.
///
/// # Errors
/// If the server is unreachable.
pub async fn connect_admin(server_url: &str) -> Result<PgConnection, sqlx::Error> {
    PgConnection::connect(&format!("{server_url}/postgres")).await
}

/// Seconds since the Unix epoch (0 for a time before it).
fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A request for Postgres 17 (Alpine), the image every test database uses;
/// callers add their own settings (files, command) before starting it.
pub fn postgres_17() -> ContainerRequest<Postgres> {
    Postgres::default().with_tag("17-alpine")
}

/// Starts the container `request` builds, through [`start_labelled`] with
/// [`STARTUP_TIMEOUT`], retrying a failed start once with a fresh label.
///
/// # Arguments
/// * `request` - builds the container request; called once per attempt,
///   because a request is consumed by its start and cannot be cloned.
///
/// # Panics
/// If every attempt fails (the last error is reported).
pub async fn start_with_retry(
    request: impl Fn() -> ContainerRequest<Postgres>,
) -> ContainerAsync<Postgres> {
    let mut attempt = 1;
    loop {
        let labelled = request().with_startup_timeout(STARTUP_TIMEOUT);
        match start_labelled(labelled, &unique_start_label()).await {
            Ok(container) => return container,
            Err(error) if attempt < START_ATTEMPTS => {
                tracing::warn!("postgres container start failed, retrying: {error}");
                attempt += 1;
            }
            Err(error) => panic!("start a postgres container (is docker reachable?): {error}"),
        }
    }
}

/// A [`START_LABEL`] value no other start uses: the process id keeps
/// concurrent test binaries apart, the counter keeps one binary's tests apart.
pub fn unique_start_label() -> String {
    // A process-wide counter; `fetch_add` hands each caller a distinct value
    // even from parallel threads (like `itertools.count()` behind a lock).
    static NEXT_START: AtomicU64 = AtomicU64::new(0);
    let start = NEXT_START.fetch_add(1, Ordering::Relaxed);
    format!("{}-{start}", std::process::id())
}

/// Starts `request` labelled `START_LABEL=label`; on failure, removes any
/// container the attempt left behind before returning the error.
///
/// # Arguments
/// * `request` - the configured container (image, files, command, startup
///   timeout).
/// * `label` - this start's [`START_LABEL`] value ([`unique_start_label`]).
///
/// # Errors
/// The start's error, or, if the cleanup also failed, that error wrapped as
/// [`TestcontainersError::Other`] (naming both).
pub async fn start_labelled(
    request: ContainerRequest<Postgres>,
    label: &str,
) -> Result<ContainerAsync<Postgres>, TestcontainersError> {
    let start_error = match request.with_label(START_LABEL, label).start().await {
        Ok(container) => return Ok(container),
        Err(error) => error,
    };
    match remove_labelled(label).await {
        Ok(_) => Err(start_error),
        Err(cleanup_error) => Err(TestcontainersError::other(format!(
            "{start_error}; removing its container also failed: {cleanup_error}"
        ))),
    }
}

/// The ids of every container (running or not) labelled `START_LABEL=label`.
///
/// # Errors
/// If Docker is unreachable or refuses the listing.
pub async fn labelled_containers(label: &str) -> Result<Vec<String>, DockerError> {
    let docker = Docker::connect_with_defaults()?;
    let filters = HashMap::from([("label".to_string(), vec![format!("{START_LABEL}={label}")])]);
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await?;
    Ok(containers
        .into_iter()
        .filter_map(|summary| summary.id)
        .collect())
}

/// Force-removes every container labelled `START_LABEL=label`, with its
/// anonymous volumes.
///
/// # Returns
/// How many containers this call removed.
///
/// # Errors
/// If Docker is unreachable, or refuses a removal for a reason other than
/// the container being gone already (404) or already being removed (409),
/// as when testcontainers' own `Drop` got there first.
pub async fn remove_labelled(label: &str) -> Result<usize, DockerError> {
    let docker = Docker::connect_with_defaults()?;
    let mut removed = 0;
    for id in labelled_containers(label).await? {
        let options = RemoveContainerOptions {
            force: true,
            v: true,
            ..Default::default()
        };
        match docker.remove_container(&id, Some(options)).await {
            Ok(()) => removed += 1,
            Err(DockerError::DockerResponseServerError {
                status_code: 404 | 409,
                ..
            }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}
