//! The `okf-mcp` command line: administration directly against the
//! database (design "Process and routes"). `serve`, the default, arrives in M4.
//!
//! Every subcommand that touches the database runs the embedded migrations
//! first (idempotent), so a fresh database needs no separate step.

use std::path::PathBuf;

// `Context` adds a message to an error (anyhow's `.context(..)`), like
// `raise X from e` with a better message.
use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use sqlx::PgPool;

use crate::clock::Clock;
use crate::config::Config;
use crate::db;
use crate::store::catalogs;
use crate::store::keys::{self, Access, Grants, NewKey};

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
    /// Create, list or delete catalogs.
    // A nested subcommand: `okf-mcp catalog create ...`.
    #[command(subcommand)]
    Catalog(CatalogCommand),
    /// Create, list, revoke, grant or ungrant API keys.
    #[command(subcommand)]
    Key(KeyCommand),
}

/// `okf-mcp catalog ...`.
#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    /// Create an empty catalog.
    Create {
        /// Its name (`[a-z0-9-]+`).
        name: String,
        /// Free text.
        #[arg(long, default_value = "")]
        description: String,
    },
    /// List catalogs.
    List,
    /// Delete a catalog with its documents, history, pending changes and grants.
    Delete {
        /// Its name.
        name: String,
        /// Delete even if a session has unpublished changes.
        #[arg(long)]
        force: bool,
    },
}

/// `okf-mcp key ...`.
#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    /// Create a key and print it once.
    Create(KeyCreateArgs),
    /// List keys (never the keys themselves).
    List,
    /// Revoke a key.
    Revoke {
        /// The key's id.
        id: String,
    },
    /// Grant a key a catalog.
    Grant {
        /// The key's id.
        id: String,
        /// The catalog.
        catalog: String,
    },
    /// Remove a key's grant of a catalog (and clear it as the key's default).
    Ungrant {
        /// The key's id.
        id: String,
        /// The catalog.
        catalog: String,
    },
}

/// `okf-mcp key create`'s options.
// `derive(Args)` makes this struct a group of options that a subcommand
// variant (`Create(KeyCreateArgs)`) can hold.
#[derive(Debug, Args)]
pub struct KeyCreateArgs {
    /// Unique id (`[A-Za-z0-9._-]+`).
    pub id: String,
    /// Access level.
    #[arg(long, value_enum)]
    pub access: Access,
    /// Granted catalogs, comma-separated.
    // Exactly one of `--catalogs` and `--all`: clap refuses both or neither.
    #[arg(
        long,
        value_delimiter = ',',
        required_unless_present = "all",
        conflicts_with = "all"
    )]
    pub catalogs: Vec<String>,
    /// Grant every catalog, present and future.
    #[arg(long)]
    pub all: bool,
    /// The key's default catalog (must be granted).
    #[arg(long)]
    pub default: Option<String>,
    /// The actor its writes are recorded as (default: the id).
    #[arg(long)]
    pub actor: Option<String>,
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
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            anyhow::bail!("the MCP server arrives in milestone 4; see `okf-mcp --help`")
        }
        Command::Migrate => {
            connect(cli.database_url.as_deref()).await?;
            println!("migrations applied");
        }
        Command::Catalog(command) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            run_catalog(&pool, command, clock).await?;
        }
        Command::Key(command) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            run_key(&pool, command, clock).await?;
        }
    }
    Ok(())
}

/// Runs `okf-mcp catalog ...`.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which catalog subcommand.
/// * `clock` - the source of `created_at`.
///
/// # Errors
/// The store's refusal or database error.
async fn run_catalog(
    pool: &PgPool,
    command: CatalogCommand,
    clock: &dyn Clock,
) -> anyhow::Result<()> {
    match command {
        CatalogCommand::Create { name, description } => {
            let catalog = catalogs::create(pool, &name, &description, clock.now()).await?;
            println!("created catalog {}", catalog.name);
        }
        CatalogCommand::List => {
            for catalog in catalogs::list(pool).await? {
                println!(
                    "{}\tgeneration {}\t{}",
                    catalog.name, catalog.generation, catalog.description
                );
            }
        }
        CatalogCommand::Delete { name, force } => {
            catalogs::delete(pool, &name, force).await?;
            println!("deleted catalog {name}");
        }
    }
    Ok(())
}

/// Runs `okf-mcp key ...`. `create` prints the key alone on stdout (so
/// `$(okf-mcp key create ...)` captures it) and a reminder on stderr; nothing
/// else ever prints or logs a key.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - which key subcommand.
/// * `clock` - the source of `created_at` and `revoked_at`.
///
/// # Errors
/// The store's refusal or database error.
async fn run_key(pool: &PgPool, command: KeyCommand, clock: &dyn Clock) -> anyhow::Result<()> {
    match command {
        KeyCommand::Create(args) => {
            let grants = if args.all {
                Grants::All
            } else {
                Grants::Catalogs(&args.catalogs)
            };
            let new = NewKey {
                id: &args.id,
                access: args.access,
                grants,
                default_catalog: args.default.as_deref(),
                actor: args.actor.as_deref().unwrap_or(&args.id),
            };
            let key = keys::create(pool, &new, clock.now()).await?;
            println!("{key}");
            eprintln!("store this key now: it is not shown again");
        }
        KeyCommand::List => {
            for key in keys::list(pool).await? {
                let catalogs = if key.all_catalogs {
                    "*".to_string()
                } else {
                    key.catalogs.join(",")
                };
                let state = if key.revoked_at.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                println!(
                    "{}\t{}\t{state}\t{catalogs}\tdefault {}\t{}",
                    key.id,
                    key.access.as_str(),
                    key.default_catalog.as_deref().unwrap_or("-"),
                    key.actor
                );
            }
        }
        KeyCommand::Revoke { id } => {
            keys::revoke(pool, &id, clock.now()).await?;
            println!("revoked key {id}");
        }
        KeyCommand::Grant { id, catalog } => {
            keys::grant(pool, &id, &catalog).await?;
            println!("granted {catalog} to key {id}");
        }
        KeyCommand::Ungrant { id, catalog } => {
            keys::ungrant(pool, &id, &catalog).await?;
            println!("removed {catalog} from key {id}");
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

    #[test]
    fn catalog_subcommands_parse() {
        let parsed =
            Cli::try_parse_from(["okf-mcp", "catalog", "create", "pg", "--description", "D"])
                .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Catalog(CatalogCommand::Create { ref name, ref description }))
                if name == "pg" && description == "D"
        ));
        let parsed =
            Cli::try_parse_from(["okf-mcp", "catalog", "delete", "pg", "--force"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Catalog(CatalogCommand::Delete { force: true, .. }))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_create_runs_against_the_database() {
        let db = crate::db::pg_support::TestDb::start().await;
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
        };

        run(
            cli(Some(db.url.clone()), Command::Catalog(create)),
            &FixedClock(chrono::Utc::now()),
        )
        .await
        .unwrap();

        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM catalogs")
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(names, ["pg"]);
    }

    #[test]
    fn key_create_needs_catalogs_or_all_but_not_both() {
        let parsed = Cli::try_parse_from([
            "okf-mcp",
            "key",
            "create",
            "k",
            "--access",
            "write",
            "--catalogs",
            "pg,work",
        ])
        .unwrap();
        let Some(Command::Key(KeyCommand::Create(args))) = parsed.command else {
            panic!("expected key create");
        };
        assert_eq!(
            (args.access, args.catalogs, args.all),
            (
                Access::Write,
                vec!["pg".to_string(), "work".to_string()],
                false
            )
        );

        let both = Cli::try_parse_from([
            "okf-mcp",
            "key",
            "create",
            "k",
            "--access",
            "read",
            "--catalogs",
            "pg",
            "--all",
        ]);
        let neither = Cli::try_parse_from(["okf-mcp", "key", "create", "k", "--access", "read"]);
        assert_eq!(
            both.unwrap_err().kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
        assert_eq!(
            neither.unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_subcommands_run_against_the_database() {
        let db = crate::db::pg_support::TestDb::start().await;
        let clock = FixedClock(chrono::Utc::now());
        let url = Some(db.url.clone());
        let create = KeyCommand::Create(KeyCreateArgs {
            id: "k".to_string(),
            access: Access::Read,
            catalogs: Vec::new(),
            all: true,
            default: None,
            actor: None,
        });

        run(cli(url.clone(), Command::Key(create)), &clock)
            .await
            .unwrap();
        run(cli(url.clone(), Command::Key(KeyCommand::List)), &clock)
            .await
            .unwrap();
        let revoke = KeyCommand::Revoke {
            id: "k".to_string(),
        };
        run(cli(url, Command::Key(revoke)), &clock).await.unwrap();

        let rows: Vec<(String, bool, bool)> =
            sqlx::query_as("SELECT actor, all_catalogs, revoked_at IS NOT NULL FROM api_keys")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        // The actor defaults to the id.
        assert_eq!(rows, [("k".to_string(), true, true)]);
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
