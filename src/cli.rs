//! The `okf-mcp` command line: administration directly against the
//! database (design "Process and routes"). `serve`, the default, arrives in M4.
//!
//! Every subcommand that touches the database runs the embedded migrations
//! first (idempotent), so a fresh database needs no separate step.

use std::path::PathBuf;

// `Context` adds a message to an error (anyhow's `.context(..)`), like
// `raise X from e` with a better message.
use anyhow::Context;
use clap::{Parser, Subcommand};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::config::Config;
use crate::db;

/// `okf-mcp`: an MCP server for OKF knowledge catalogs.
// `derive(Parser)` generates the argument parser from the struct, as
// `argparse` would from `add_argument` calls; the `#[arg]` attributes are
// the options.
#[derive(Debug, Parser)]
#[command(name = "okf-mcp", version, about)]
pub struct Cli {
    /// The Postgres URL.
    // `global = true` accepts the flag after a subcommand too;
    // `hide_env_values` keeps the URL (and its password) out of `--help`.
    #[arg(long, env = "DATABASE_URL", global = true, hide_env_values = true)]
    pub database_url: Option<String>,
    /// The TOML configuration file.
    #[arg(long, env = "OKF_MCP_CONFIG", global = true)]
    pub config: Option<PathBuf>,
    /// What to do (`serve` when omitted).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve MCP over HTTP (arrives in milestone 4).
    Serve,
    /// Apply the schema migrations and exit.
    Migrate,
}

/// Runs one command line.
///
/// # Arguments
/// * `cli` - the parsed command line.
/// * `clock` - the source of every timestamp written (`&dyn Clock`: any
///   clock, real or fixed).
///
/// # Errors
/// Any failure, with context, for `main` to print.
pub async fn run(cli: Cli, clock: &dyn Clock) -> anyhow::Result<()> {
    // Read now so a bad file fails every command; `import` (Task 8) uses it.
    let _config = Config::load(cli.config.as_deref())?;
    let _ = clock; // The `catalog`, `key` and `import` subcommands (Tasks 5-8) use it.
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            anyhow::bail!("the MCP server arrives in milestone 4; see `okf-mcp --help`")
        }
        Command::Migrate => {
            connect(cli.database_url.as_deref()).await?;
            println!("migrations applied");
        }
    }
    Ok(())
}

/// Connects to the database and applies the migrations.
async fn connect(url: Option<&str>) -> anyhow::Result<PgPool> {
    let url = url.context("no database: set DATABASE_URL or pass --database-url")?;
    let pool = db::connect(url).await.context("connect to the database")?;
    db::migrate(&pool).await?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    fn cli(database_url: Option<String>, command: Command) -> Cli {
        Cli {
            database_url,
            config: None,
            command: Some(command),
        }
    }

    #[test]
    fn no_subcommand_means_serve() {
        let parsed = Cli::try_parse_from(["okf-mcp"]).unwrap();
        assert!(parsed.command.is_none());
        let parsed = Cli::try_parse_from(["okf-mcp", "migrate"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::Migrate)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migrate_applies_the_schema() {
        let db = crate::db::pg_support::TestDb::start().await;

        let clock = FixedClock(chrono::Utc::now());

        run(cli(Some(db.url.clone()), Command::Migrate), &clock)
            .await
            .unwrap();
        // A second run finds nothing to apply: every later subcommand
        // migrates on connect, so this must be a no-op.
        run(cli(Some(db.url.clone()), Command::Migrate), &clock)
            .await
            .unwrap();

        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(applied, 1);
    }

    #[tokio::test]
    async fn a_missing_database_url_is_explained() {
        let error = run(cli(None, Command::Migrate), &FixedClock(chrono::Utc::now()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("DATABASE_URL"));
    }

    #[tokio::test]
    async fn serve_is_not_there_yet() {
        let error = run(cli(None, Command::Serve), &FixedClock(chrono::Utc::now()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("milestone 4"));
    }
}
