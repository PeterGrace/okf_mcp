//! A throwaway Postgres for tests: one container per test, removed when the
//! [`TestDb`] is dropped. The library's unit tests include this same file
//! (`#[path]` in `src/db.rs`), so there is one copy.
//!
//! Each test starts its own container (about 1.4 s; tests run in parallel). A
//! container shared through a `static` would never be removed, because Rust
//! does not drop statics.
//!
//! A start that fails after Docker created the container (the start times
//! out under load, or Docker refuses it with "endpoint ... already exists")
//! returns no [`ContainerAsync`], so nothing would remove it. Every container
//! therefore carries a label unique to its start, and [`start_labelled`]
//! removes whatever has that label when the start fails. Tests that need a
//! customized container (`tests/tls.rs`) start it through
//! [`start_with_retry`] too.

// Each crate that includes this file uses a different part of it (the CLI
// tests need `url`, the unit tests only `pool`).
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};
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

/// The label key whose value identifies one start of a test container.
pub const START_LABEL: &str = "okf-mcp.test-start";

/// How long one start may take before it counts as failed (testcontainers'
/// own default).
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Attempts per [`TestDb::start`]: one retry covers a transient Docker
/// refusal or a start that timed out under load.
const START_ATTEMPTS: u32 = 2;

/// A running Postgres 17 and a pool connected to it (not migrated).
pub struct TestDb {
    /// Connected to the container's `postgres` database.
    pub pool: PgPool,
    /// The connection URL, for code that connects by itself (the CLI).
    pub url: String,
    // Fields drop in declaration order: the pool closes before the container
    // (whose `Drop` stops and removes it) goes away.
    _container: ContainerAsync<Postgres>,
}

impl TestDb {
    /// Starts the container (retrying once) and connects.
    ///
    /// # Panics
    /// If Docker is unreachable or the container does not start twice: a
    /// test environment problem, reported at once.
    pub async fn start() -> Self {
        let container = start_with_retry(postgres_17).await;
        let host = container.get_host().await.expect("container host");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("container port");
        let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect to the test postgres");
        Self {
            pool,
            url,
            _container: container,
        }
    }
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
