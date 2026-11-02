//! A throwaway Postgres for tests: one container per test, removed when the
//! [`TestDb`] is dropped. The library's unit tests include this same file
//! (`#[path]` in `src/db.rs`), so there is one copy.
//!
//! Each test starts its own container (about 1.4 s; tests run in parallel). A
//! container shared through a `static` would never be removed, because Rust
//! does not drop statics.

// Each crate that includes this file uses a different part of it (the CLI
// tests need `url`, the unit tests only `pool`).
#![allow(dead_code)]

use sqlx::postgres::{PgPool, PgPoolOptions};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

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
    /// Starts the container and connects.
    ///
    /// # Panics
    /// If Docker is unreachable or the container does not start: a test
    /// environment problem, reported at once.
    pub async fn start() -> Self {
        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start a postgres container (is docker reachable?)");
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
