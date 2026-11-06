//! The `okf-mcp` command line: administration directly against the
//! database (design "Process and routes"), and `serve`, the default, which
//! runs the MCP server.
//!
//! Every subcommand that touches the database runs the embedded migrations
//! first (idempotent), so a fresh database needs no separate step.

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// `Context` adds a message to an error (anyhow's `.context(..)`), like
// `raise X from e` with a better message.
use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::archive::{self, Files};
use crate::clock::Clock;
use crate::config::Config;
use crate::db;
use crate::http;
use crate::store::catalogs;
use crate::store::export;
use crate::store::import::{self, ImportOptions};
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
    /// Serve MCP over HTTP.
    Serve(ServeArgs),
    /// Apply the schema migrations and exit.
    Migrate,
    /// Create, list or delete catalogs.
    // A nested subcommand: `okf-mcp catalog create ...`.
    #[command(subcommand)]
    Catalog(CatalogCommand),
    /// Create, list, revoke, grant or ungrant API keys.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Import a directory or a .tar.gz into an existing catalog.
    Import {
        /// The catalog.
        catalog: String,
        /// A directory, a .tar.gz file, or `-` for a .tar.gz on stdin.
        source: PathBuf,
        /// Replace a non-empty catalog's contents (write the differences).
        #[arg(long)]
        replace: bool,
        /// Recorded as the revisions' actor.
        #[arg(long, default_value = "okf-mcp/import")]
        actor: String,
    },
    /// Export a catalog's live documents as a .tar.gz.
    Export {
        /// The catalog.
        catalog: String,
        /// The file to write, or `-` for stdout.
        output: PathBuf,
    },
}

/// `okf-mcp serve`'s options.
// `Default`: no flags, which is what `okf-mcp` alone means.
#[derive(Debug, Default, Args)]
pub struct ServeArgs {
    /// The address to listen on, overriding the config's `listen`.
    #[arg(long, env = "OKF_MCP_LISTEN")]
    pub listen: Option<std::net::SocketAddr>,
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
        /// The key's id, or an unambiguous active name.
        id: String,
        /// Select this actor when `id` is a name.
        #[arg(long)]
        actor: Option<String>,
    },
    /// Grant a key a catalog.
    Grant {
        /// The key's id, or an unambiguous active name.
        id: String,
        /// Select this actor when `id` is a name.
        #[arg(long)]
        actor: Option<String>,
        /// The catalog.
        catalog: String,
    },
    /// Remove a key's grant of a catalog (and clear it as the key's default).
    Ungrant {
        /// The key's id, or an unambiguous active name.
        id: String,
        /// Select this actor when `id` is a name.
        #[arg(long)]
        actor: Option<String>,
        /// The catalog.
        catalog: String,
    },
}

/// `okf-mcp key create`'s options.
// `derive(Args)` makes this struct a group of options that a subcommand
// variant (`Create(KeyCreateArgs)`) can hold.
#[derive(Debug, Args)]
pub struct KeyCreateArgs {
    /// Grouping name (`[A-Za-z0-9._-]+`).
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
/// * `clock` - the source of every timestamp written (`Arc<dyn Clock>`: any
///   clock, real or fixed, behind a shared handle the server can keep).
///
/// # Errors
/// Any failure, with context, for `main` to print.
pub async fn run(cli: Cli, clock: Arc<dyn Clock>) -> anyhow::Result<()> {
    // Read first so a bad file fails every command, not only `import`.
    let config = Config::load(cli.config.as_deref())?;
    // `unwrap_or_else` builds the default only when no subcommand was given.
    match cli
        .command
        .unwrap_or_else(|| Command::Serve(ServeArgs::default()))
    {
        Command::Serve(args) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            // `mut`: the flag may override the file.
            let mut config = config;
            if let Some(listen) = args.listen {
                config.listen = listen;
            }
            let listen = config.listen;
            let app = App::load(pool, config, clock)
                .await
                .context("load the catalogs")?;
            let listener = TcpListener::bind(listen)
                .await
                .with_context(|| format!("listen on {listen}"))?;
            tracing::info!(%listen, "serving");
            let shutdown = CancellationToken::new();
            // A background task; it cancels `shutdown` on the first signal.
            tokio::spawn(cancel_on_signal(shutdown.clone()));
            http::serve(app, listener, shutdown)
                .await
                .context("serve")?;
        }
        Command::Migrate => {
            connect(cli.database_url.as_deref()).await?;
            println!("migrations applied");
        }
        Command::Catalog(command) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            run_catalog(&pool, command, clock.as_ref()).await?;
        }
        Command::Key(command) => {
            let pool = connect(cli.database_url.as_deref()).await?;
            run_key(&pool, command, clock.as_ref()).await?;
        }
        Command::Import {
            catalog,
            source,
            replace,
            actor,
        } => {
            // Connect first: a missing or unreachable database fails before a
            // large tarball (or stdin) is read.
            let pool = connect(cli.database_url.as_deref()).await?;
            let limit = config.max_import_bytes.as_u64();
            // Reading files is blocking I/O: run it on a blocking thread, as
            // `import` does for its snapshot, so the runtime's threads stay free.
            let files = tokio::task::spawn_blocking(move || read_source(&source, limit))
                .await
                .context("the import reader stopped")??;
            let options = ImportOptions {
                replace,
                actor: &actor,
                now: clock.now(),
            };
            let report = import::import(&pool, &catalog, files, &options).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Export { catalog, output } => {
            let pool = connect(cli.database_url.as_deref()).await?;
            // Build the archive in memory first: no `StdoutLock` is held across
            // `.await` (which would make this future impossible to
            // `tokio::spawn`), and an unknown catalog leaves no file behind.
            let tarball = export::export(&pool, &catalog, Vec::new()).await?;
            // Writing is blocking I/O: run it on a blocking thread.
            tokio::task::spawn_blocking(move || write_output(&output, &tarball))
                .await
                .context("the export writer stopped")??;
        }
    }
    Ok(())
}

/// Cancels `shutdown` on SIGINT (Ctrl-C) or SIGTERM (what Kubernetes sends).
///
/// # Arguments
/// * `shutdown` - the server's token.
async fn cancel_on_signal(shutdown: CancellationToken) {
    // `ctrl_c()` resolves at once with an error if the handler cannot be
    // registered; waiting forever then keeps that from reading as a signal.
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for SIGINT");
            // Wait forever: SIGTERM still works.
            std::future::pending::<()>().await;
        }
    };
    // `tokio::signal::unix` exists only on Unix, the only platform this
    // server runs on.
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                // Wait forever: Ctrl-C still works.
                std::future::pending::<()>().await;
            }
        }
    };
    // Whichever signal comes first; the other future is dropped.
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
    shutdown.cancel();
}

/// Reads an import's files: a directory, a `.tar.gz` file, or (`-`) a
/// `.tar.gz` on stdin.
///
/// # Arguments
/// * `source` - the path given on the command line.
/// * `limit` - `max_import_bytes`, the cap on the uncompressed total.
///
/// # Errors
/// The archive's refusal, or the file failing to open.
fn read_source(source: &Path, limit: u64) -> anyhow::Result<Files> {
    let files = if source.as_os_str() == "-" {
        archive::read_tarball(std::io::stdin().lock(), limit)?
    } else if source.is_dir() {
        archive::read_directory(source, limit)?
    } else {
        let file = File::open(source).with_context(|| format!("open {}", source.display()))?;
        archive::read_tarball(BufReader::new(file), limit)?
    };
    Ok(files)
}

/// Writes an export's archive to a file, or (`-`) to stdout.
///
/// # Arguments
/// * `output` - the path given on the command line.
/// * `tarball` - the finished `.tar.gz`.
///
/// # Errors
/// The file failing to be created or written, or stdout failing.
fn write_output(output: &Path, tarball: &[u8]) -> anyhow::Result<()> {
    if output.as_os_str() == "-" {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(tarball).context("write to stdout")?;
        // Locked stdout is line-buffered: flush the tail before returning.
        stdout.flush().context("write to stdout")?;
    } else {
        write_file_atomically(output, tarball)
            .with_context(|| format!("write {}", output.display()))?;
    }
    Ok(())
}

/// Writes `contents` to `path` through a temp file in the same directory,
/// renamed into place once complete, so a failed write (a full disk, say)
/// never leaves a truncated archive. An existing file is replaced, as
/// `tar -cf` would.
///
/// # Arguments
/// * `path` - the file to write.
/// * `contents` - its bytes.
///
/// # Errors
/// The temp file failing to be created, written, synced or renamed.
fn write_file_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    // `parent()` of a bare file name is `Some("")`: the current directory.
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".okf-export-").suffix(".tmp");
    // `tempfile` alone creates 0o600; ask for 0o666 minus the umask, as
    // `File::create` does.
    #[cfg(unix)]
    builder.permissions(crate::okf::workspace::new_file_permissions());
    // The temp file is deleted when `temp` is dropped on any early return
    // (`Drop`, like a `with` block's `__exit__`), so a failure leaks nothing.
    let mut temp = builder.tempfile_in(parent)?;
    temp.write_all(contents)?;
    // Durable before the rename, so a crash cannot leave an empty file.
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
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
            let created = keys::create_with_id(pool, &new, clock.now()).await?;
            println!("{}", created.key);
            eprintln!(
                "key id: {}\nstore this key now: it is not shown again",
                created.id
            );
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
                    "{}\t{}\t{}\t{}\t{state}\t{catalogs}\tdefault {}",
                    key.name,
                    key.actor,
                    key.id,
                    key.access.as_str(),
                    key.default_catalog.as_deref().unwrap_or("-")
                );
            }
        }
        KeyCommand::Revoke { id, actor } => {
            let resolved = keys::revoke_target(pool, &id, actor.as_deref(), clock.now()).await?;
            println!("revoked key {resolved}");
        }
        KeyCommand::Grant { id, actor, catalog } => {
            let resolved = keys::grant_target(pool, &id, actor.as_deref(), &catalog).await?;
            println!("granted {catalog} to key {resolved}");
        }
        KeyCommand::Ungrant { id, actor, catalog } => {
            let resolved = keys::ungrant_target(pool, &id, actor.as_deref(), &catalog).await?;
            println!("removed {catalog} from key {resolved}");
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

    /// A fixed clock at the current instant, as `run` takes it.
    fn fixed_now() -> Arc<dyn Clock> {
        Arc::new(FixedClock(chrono::Utc::now()))
    }

    fn cli(database_url: Option<String>, command: Command) -> Cli {
        Cli {
            database_url,
            config: None,
            command: Some(command),
        }
    }

    #[test]
    fn serve_is_the_default_and_takes_listen() {
        let default = Cli::try_parse_from(["okf-mcp"]).unwrap();
        let flagged =
            Cli::try_parse_from(["okf-mcp", "serve", "--listen", "127.0.0.1:9000"]).unwrap();
        let migrate = Cli::try_parse_from(["okf-mcp", "migrate"]).unwrap();

        assert!(default.command.is_none());
        assert!(matches!(
            flagged.command,
            Some(Command::Serve(ServeArgs { listen: Some(addr) })) if addr.port() == 9000
        ));
        assert!(matches!(migrate.command, Some(Command::Migrate)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn migrate_applies_the_schema() {
        let db = crate::db::pg_support::TestDb::start().await;
        let clock = fixed_now();

        run(
            cli(Some(db.url.clone()), Command::Migrate),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        // A second run finds nothing to apply: every later subcommand
        // migrates on connect, so this must be a no-op.
        run(
            cli(Some(db.url.clone()), Command::Migrate),
            Arc::clone(&clock),
        )
        .await
        .unwrap();

        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        // One row per embedded migration (`iter` walks the files `migrate!`
        // found), so adding a migration does not break this test.
        let embedded = i64::try_from(crate::db::MIGRATOR.iter().count()).unwrap();
        assert_eq!(applied, embedded);
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
            fixed_now(),
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
        let clock = fixed_now();
        let url = Some(db.url.clone());
        let create = KeyCommand::Create(KeyCreateArgs {
            id: "k".to_string(),
            access: Access::Read,
            catalogs: Vec::new(),
            all: true,
            default: None,
            actor: None,
        });

        run(cli(url.clone(), Command::Key(create)), Arc::clone(&clock))
            .await
            .unwrap();
        run(
            cli(url.clone(), Command::Key(KeyCommand::List)),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let revoke = KeyCommand::Revoke {
            id: "k".to_string(),
            actor: None,
        };
        run(cli(url, Command::Key(revoke)), Arc::clone(&clock))
            .await
            .unwrap();

        let rows: Vec<(String, bool, bool)> =
            sqlx::query_as("SELECT actor, all_catalogs, revoked_at IS NOT NULL FROM api_keys")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        // The actor defaults to the id.
        assert_eq!(rows, [("k".to_string(), true, true)]);
    }

    #[test]
    fn import_parses() {
        let parsed = Cli::try_parse_from(["okf-mcp", "import", "pg", "-", "--replace"]).unwrap();

        assert!(matches!(
            parsed.command,
            Some(Command::Import { ref catalog, ref source, replace: true, ref actor })
                if catalog == "pg" && source.as_os_str() == "-" && actor == "okf-mcp/import"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_reads_a_directory() {
        let db = crate::db::pg_support::TestDb::start().await;
        let clock = fixed_now();
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
        };
        run(
            cli(Some(db.url.clone()), Command::Catalog(create)),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.md"), "# Index\n").unwrap();
        let import = Command::Import {
            catalog: "pg".to_string(),
            source: dir.path().to_path_buf(),
            replace: false,
            actor: "test".to_string(),
        };

        run(cli(Some(db.url.clone()), import), Arc::clone(&clock))
            .await
            .unwrap();

        let rows: Vec<(String, String)> = sqlx::query_as("SELECT path, updated_by FROM documents")
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, [("index.md".to_string(), "test".to_string())]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_reads_a_tarball_file_and_explains_a_missing_one() {
        let db = crate::db::pg_support::TestDb::start().await;
        let clock = fixed_now();
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
        };
        run(
            cli(Some(db.url.clone()), Command::Catalog(create)),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("pg.tar.gz");
        let script = archive::ExportFile {
            path: "tools/run.sh",
            content: b"#!/bin/sh\n",
            executable: true,
            modified: clock.now(),
        };
        archive::write_tarball(File::create(&tarball).unwrap(), &[script]).unwrap();
        let import = |source: PathBuf| Command::Import {
            catalog: "pg".to_string(),
            source,
            replace: false,
            actor: "test".to_string(),
        };

        run(
            cli(Some(db.url.clone()), import(tarball)),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let missing = dir.path().join("missing.tar.gz");
        let error = run(
            cli(Some(db.url.clone()), import(missing.clone())),
            Arc::clone(&clock),
        )
        .await
        .unwrap_err();

        let rows: Vec<(String, bool)> = sqlx::query_as("SELECT path, executable FROM documents")
            .fetch_all(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, [("tools/run.sh".to_string(), true)]);
        assert!(error
            .to_string()
            .contains(&format!("open {}", missing.display())));
    }

    #[tokio::test]
    async fn import_checks_the_database_before_reading_the_source() {
        // A source that does not exist: reading it first would report "open".
        let import = Command::Import {
            catalog: "pg".to_string(),
            source: PathBuf::from("/nonexistent-okf-import.tar.gz"),
            replace: false,
            actor: "test".to_string(),
        };

        let error = run(cli(None, import), fixed_now()).await.unwrap_err();

        assert!(error.to_string().contains("DATABASE_URL"));
    }

    #[test]
    fn export_parses() {
        let parsed = Cli::try_parse_from(["okf-mcp", "export", "pg", "-"]).unwrap();

        assert!(matches!(
            parsed.command,
            Some(Command::Export { ref catalog, ref output })
                if catalog == "pg" && output.as_os_str() == "-"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn export_writes_a_tarball_file_and_none_for_an_unknown_catalog() {
        let db = crate::db::pg_support::TestDb::start().await;
        let clock = fixed_now();
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
        };
        run(
            cli(Some(db.url.clone()), Command::Catalog(create)),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.md"), "# Index\n").unwrap();
        let import = Command::Import {
            catalog: "pg".to_string(),
            source: dir.path().to_path_buf(),
            replace: false,
            actor: "test".to_string(),
        };
        run(cli(Some(db.url.clone()), import), Arc::clone(&clock))
            .await
            .unwrap();
        let export = |catalog: &str, output: PathBuf| Command::Export {
            catalog: catalog.to_string(),
            output,
        };
        let written = dir.path().join("pg.tar.gz");
        let unwritten = dir.path().join("nope.tar.gz");

        run(
            cli(Some(db.url.clone()), export("pg", written.clone())),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let unknown = run(
            cli(Some(db.url.clone()), export("nope", unwritten.clone())),
            Arc::clone(&clock),
        )
        .await;

        let files = archive::read_tarball(File::open(&written).unwrap(), u64::MAX).unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), ["index.md"]);
        assert_eq!(files["index.md"].content, b"# Index\n");
        assert!(unknown.is_err());
        assert!(!unwritten.exists());
        // The temp file was renamed into place: nothing else is left behind.
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["index.md", "pg.tar.gz"]);
    }

    #[tokio::test]
    async fn a_missing_database_url_is_explained() {
        let error = run(cli(None, Command::Migrate), fixed_now())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("DATABASE_URL"));
    }

    #[tokio::test]
    async fn serve_without_a_database_is_explained() {
        let serve = Command::Serve(ServeArgs::default());

        let error = run(cli(None, serve), fixed_now()).await.unwrap_err();

        assert!(error.to_string().contains("DATABASE_URL"));
    }
}
