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
use std::time::Duration;

// `Context` adds a message to an error (anyhow's `.context(..)`), like
// `raise X from e` with a better message.
use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::archive::{self, Files};
use crate::clock::{format_timestamp, Clock};
use crate::config::Config;
use crate::db;
use crate::http;
use crate::store::catalogs;
use crate::store::export;
use crate::store::import::{self, ImportOptions, ImportScope};
use crate::store::keys::{self, Access, Grants, KeyKind, NewKey};
use crate::store::oauth::codes::{self, NewSignInCode, SignInCodeRecord};

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
    /// Create, update, list or delete catalogs.
    // A nested subcommand: `okf-mcp catalog create ...`.
    #[command(subcommand)]
    Catalog(CatalogCommand),
    /// Create, list, revoke, grant, ungrant or configure reporting on API keys.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Mint, list and revoke OAuth sign-in codes; list and revoke OAuth grants.
    // clap names a subcommand after its variant in kebab case: `Oauth` is
    // `oauth` (an `OAuth` variant would be `o-auth`).
    #[command(subcommand)]
    Oauth(OauthCommand),
    /// Import a directory or a .tar.gz into an existing catalog.
    Import {
        /// The catalog.
        catalog: String,
        /// A directory, a .tar.gz file, or `-` for a .tar.gz on stdin.
        source: PathBuf,
        /// Replace a non-empty catalog's contents (write the differences).
        #[arg(long)]
        replace: bool,
        /// Replace only repository machinery (AGENTS.md, tools/, install/,
        /// ...): the source's other files are ignored and the catalog's
        /// concepts are left as they are.
        #[arg(long, requires = "replace")]
        only_machinery: bool,
        /// Refuse unless the catalog is still at this generation, the one
        /// `okf-mcp export` reported for the source.
        #[arg(long, value_name = "N")]
        expect_generation: Option<i64>,
        /// Recorded as the revisions' actor.
        #[arg(long, default_value = "okf-mcp/import")]
        actor: String,
    },
    /// Export a catalog's live documents as a .tar.gz, and report its
    /// generation on stderr.
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
        /// The team that owns it (a user's name names that user's personal
        /// team).
        #[arg(long)]
        owner: String,
    },
    /// Change an existing catalog's description. A running server shows the
    /// new text within one `poll_interval`.
    Update {
        /// Its name.
        name: String,
        /// The new free text (may be empty).
        #[arg(long)]
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
    /// Set a key's independent maintenance-report capability.
    MaintenanceReport {
        /// The key's immutable id, or an unambiguous active name.
        id: String,
        /// Enable or disable maintenance reporting.
        #[arg(long, required = true, action = clap::ArgAction::Set)]
        enabled: bool,
        /// Select this actor when `id` is a name.
        #[arg(long)]
        actor: Option<String>,
    },
    /// Revoke a key (an OAuth grant is revoked with `oauth grant revoke`).
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
    pub name: String,
    /// Access level.
    #[arg(long, value_enum)]
    pub access: Access,
    /// Permit maintenance reporting (does not grant write access).
    #[arg(long)]
    pub maintenance_report: bool,
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
    /// The actor its writes are recorded as (default: the name).
    #[arg(long)]
    pub actor: Option<String>,
    /// The user it belongs to (must exist).
    #[arg(long)]
    pub user: String,
}

/// `okf-mcp oauth ...`.
#[derive(Debug, Subcommand)]
pub enum OauthCommand {
    /// Sign-in codes: one-time codes a person enters on the consent page.
    #[command(subcommand)]
    Code(CodeCommand),
    /// OAuth grants: the keys sign-in codes became.
    #[command(subcommand)]
    Grant(GrantCommand),
}

/// `okf-mcp oauth code ...`.
#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    /// Mint a code and print it once.
    Create(CodeCreateArgs),
    /// List codes in every state (never the codes themselves).
    List,
    /// Revoke a code nobody has redeemed.
    Revoke {
        /// The code's id (from `oauth code list`).
        id: String,
    },
}

/// `okf-mcp oauth grant ...`.
#[derive(Debug, Subcommand)]
pub enum GrantCommand {
    /// List OAuth grants, revoked ones included.
    List,
    /// Revoke an OAuth grant: its tokens stop working within `poll_interval`.
    Revoke {
        /// The grant's id (from `oauth grant list`).
        id: String,
    },
}

/// `okf-mcp oauth code create`'s options.
#[derive(Debug, Args)]
pub struct CodeCreateArgs {
    /// A label (`[A-Za-z0-9._-]+`); the grant's name starts with it.
    pub name: String,
    /// The actor the grant's writes are recorded as.
    #[arg(long)]
    pub actor: String,
    /// Access level: `read` or `write` (`admin` is refused).
    #[arg(long, value_enum)]
    pub access: Access,
    /// Granted catalogs, comma-separated.
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
    /// The grant's default catalog (must be granted).
    #[arg(long)]
    pub default: Option<String>,
    /// How long the code stays valid unredeemed (`24h`, `3d`; at most
    /// `oauth.code_max_expiry`).
    // `value_parser = fn` runs the function on the text; its `Err(String)`
    // becomes clap's usage error.
    #[arg(long, default_value = "24h", value_parser = parse_expiry)]
    pub expires: Duration,
    /// The user the code, and the grant it becomes, belongs to (must exist).
    #[arg(long)]
    pub user: String,
}

/// Parses `--expires` (`humantime` syntax: `90m`, `24h`, `3d`).
///
/// # Errors
/// `humantime`'s message, for clap to print.
fn parse_expiry(text: &str) -> Result<Duration, String> {
    humantime_serde::re::humantime::parse_duration(text).map_err(|error| error.to_string())
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
            let pool = connect(cli.database_url.as_deref(), &config).await?;
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
            connect(cli.database_url.as_deref(), &config).await?;
            println!("migrations applied");
        }
        Command::Catalog(command) => {
            let pool = connect(cli.database_url.as_deref(), &config).await?;
            run_catalog(&pool, command, clock.as_ref()).await?;
        }
        Command::Key(command) => {
            let pool = connect(cli.database_url.as_deref(), &config).await?;
            run_key(&pool, command, clock.as_ref()).await?;
        }
        Command::Oauth(command) => {
            let pool = connect(cli.database_url.as_deref(), &config).await?;
            run_oauth(&pool, command, &config, clock.as_ref()).await?;
        }
        Command::Import {
            catalog,
            source,
            replace,
            only_machinery,
            expect_generation,
            actor,
        } => {
            // Connect first: a missing or unreachable database fails before a
            // large tarball (or stdin) is read.
            let pool = connect(cli.database_url.as_deref(), &config).await?;
            let limit = config.max_import_bytes.as_u64();
            // Reading files is blocking I/O: run it on a blocking thread, as
            // `import` does for its snapshot, so the runtime's threads stay free.
            let files = tokio::task::spawn_blocking(move || read_source(&source, limit))
                .await
                .context("the import reader stopped")??;
            let options = ImportOptions {
                replace,
                scope: if only_machinery {
                    ImportScope::Machinery
                } else {
                    ImportScope::Everything
                },
                expect_generation,
                actor: &actor,
                now: clock.now(),
            };
            let report = import::import(&pool, &catalog, files, &options).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Export { catalog, output } => {
            let pool = connect(cli.database_url.as_deref(), &config).await?;
            // Build the archive in memory first: no `StdoutLock` is held across
            // `.await` (which would make this future impossible to
            // `tokio::spawn`), and an unknown catalog leaves no file behind.
            let (tarball, generation) = export::export(&pool, &catalog, Vec::new()).await?;
            // Writing is blocking I/O: run it on a blocking thread.
            tokio::task::spawn_blocking(move || write_output(&output, &tarball))
                .await
                .context("the export writer stopped")??;
            // stderr, since the archive itself may be on stdout.
            eprintln!("exported catalog {catalog:?} at generation {generation}");
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
        CatalogCommand::Create {
            name,
            description,
            owner,
        } => {
            let catalog = catalogs::create(pool, &name, &description, &owner, clock.now()).await?;
            println!("created catalog {}", catalog.name);
        }
        CatalogCommand::Update { name, description } => {
            let previous = catalogs::update_description(pool, &name, &description).await?;
            // `{:?}` quotes the text, so an empty description shows as `""`.
            println!("updated catalog {name} description: {previous:?} -> {description:?}");
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
                name: &args.name,
                access: args.access,
                maintenance_report: args.maintenance_report,
                grants,
                default_catalog: args.default.as_deref(),
                actor: args.actor.as_deref().unwrap_or(&args.name),
                user: &args.user,
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
                println!("{}", format_key_record(&key));
            }
        }
        KeyCommand::MaintenanceReport { id, enabled, actor } => {
            let resolved =
                keys::set_maintenance_report_target(pool, &id, actor.as_deref(), enabled).await?;
            println!("key {resolved} maintenance_report {enabled}");
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

fn format_key_record(key: &keys::KeyRecord) -> String {
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
    format!(
        "{}\t{}\t{}\t{}\t{state}\t{catalogs}\tdefault {}\tmaintenance_report {}",
        key.name,
        key.actor,
        key.id,
        key.access.as_str(),
        key.default_catalog.as_deref().unwrap_or("-"),
        key.maintenance_report
    )
}

/// Runs one `okf-mcp oauth ...` subcommand. `code create` prints the code
/// alone on stdout and its id and expiry on stderr, as `key create` does.
///
/// # Arguments
/// * `pool` - the migrated database.
/// * `command` - the subcommand.
/// * `config` - for `oauth.code_max_expiry`.
/// * `clock` - the source of every timestamp.
///
/// # Errors
/// The store's refusal or database error; an id that is no OAuth grant.
async fn run_oauth(
    pool: &PgPool,
    command: OauthCommand,
    config: &Config,
    clock: &dyn Clock,
) -> anyhow::Result<()> {
    let now = clock.now();
    match command {
        OauthCommand::Code(CodeCommand::Create(args)) => {
            let expires_at = codes::expiry(now, args.expires, config.oauth.code_max_expiry)?;
            let grants = if args.all {
                Grants::All
            } else {
                Grants::Catalogs(&args.catalogs)
            };
            let new = NewSignInCode {
                name: &args.name,
                access: args.access,
                grants,
                default_catalog: args.default.as_deref(),
                actor: &args.actor,
                expires_at,
                user: &args.user,
            };
            let created = codes::create(pool, &new, now).await?;
            println!("{}", created.code);
            eprintln!(
                "code id: {}\nexpires: {}\nhand this code over now: it is not shown again",
                created.id,
                format_timestamp(expires_at)
            );
        }
        OauthCommand::Code(CodeCommand::List) => {
            for code in codes::list(pool).await? {
                println!("{}", format_code_record(&code, now));
            }
        }
        OauthCommand::Code(CodeCommand::Revoke { id }) => {
            codes::revoke(pool, &id, now).await?;
            println!("revoked sign-in code {id}");
        }
        OauthCommand::Grant(GrantCommand::List) => {
            for grant in keys::list(pool).await? {
                if grant.kind == KeyKind::OAuth {
                    println!(
                        "{}\tclient {}",
                        format_key_record(&grant),
                        grant.client_name.as_deref().unwrap_or("-")
                    );
                }
            }
        }
        OauthCommand::Grant(GrantCommand::Revoke { id }) => {
            // The one exact-id path to a grant on the command line: `key
            // revoke` resolves API keys only (`store::keys`).
            let is_grant = keys::list(pool)
                .await?
                .iter()
                .any(|key| key.id == id && key.kind == KeyKind::OAuth);
            if !is_grant {
                anyhow::bail!("no OAuth grant {id:?} (a plain key is revoked with `key revoke`)");
            }
            keys::revoke_by_id(pool, &id, now).await?;
            println!("revoked OAuth grant {id}");
        }
    }
    Ok(())
}

/// One `oauth code list` line, tab-separated: name, actor, id, access,
/// state, catalogs (`*` for all), `default X`, `expires T`, `grant G`.
///
/// # Arguments
/// * `code` - the code.
/// * `now` - the time its state is computed at.
///
/// # Returns
/// The line, without a newline.
fn format_code_record(code: &SignInCodeRecord, now: chrono::DateTime<chrono::Utc>) -> String {
    let catalogs = if code.all_catalogs {
        "*".to_string()
    } else {
        code.catalogs.join(",")
    };
    format!(
        "{}\t{}\t{}\t{}\t{}\t{catalogs}\tdefault {}\texpires {}\tgrant {}",
        code.name,
        code.actor,
        code.id,
        code.access.as_str(),
        code.state(now).as_str(),
        code.default_catalog.as_deref().unwrap_or("-"),
        format_timestamp(code.expires_at),
        code.grant_id.as_deref().unwrap_or("-")
    )
}

/// Connects to the database and applies the migrations.
async fn connect(url: Option<&str>, config: &Config) -> anyhow::Result<PgPool> {
    let url = url.context("no database: set DATABASE_URL or pass --database-url")?;
    let pool = db::connect(url, config.database_max_connections)
        .await
        .context("connect to the database")?;
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
        let parsed = Cli::try_parse_from([
            "okf-mcp",
            "catalog",
            "create",
            "pg",
            "--description",
            "D",
            "--owner",
            "family",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Catalog(CatalogCommand::Create { ref name, ref description, ref owner }))
                if name == "pg" && description == "D" && owner == "family"
        ));
        // Every new catalog has an owner, so `--owner` is required.
        assert!(Cli::try_parse_from(["okf-mcp", "catalog", "create", "pg"]).is_err());
        let parsed =
            Cli::try_parse_from(["okf-mcp", "catalog", "update", "pg", "--description", ""])
                .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Catalog(CatalogCommand::Update { ref name, ref description }))
                if name == "pg" && description.is_empty()
        ));
        // `--description` is what `update` changes, so it is required.
        assert!(Cli::try_parse_from(["okf-mcp", "catalog", "update", "pg"]).is_err());
        let parsed =
            Cli::try_parse_from(["okf-mcp", "catalog", "delete", "pg", "--force"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Catalog(CatalogCommand::Delete { force: true, .. }))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_create_runs_against_the_database() {
        let db = crate::db::test_db().await;
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
            owner: crate::db::TEST_TEAM.to_string(),
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
            "--user",
            "alice",
        ])
        .unwrap();
        let Some(Command::Key(KeyCommand::Create(args))) = parsed.command else {
            panic!("expected key create");
        };
        assert_eq!(args.name, "k");
        assert_eq!(args.user, "alice");
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
            "--user",
            "alice",
        ]);
        let neither = Cli::try_parse_from([
            "okf-mcp", "key", "create", "k", "--access", "read", "--user", "alice",
        ]);
        // Every new key names its user, so `--user` is required.
        let no_user =
            Cli::try_parse_from(["okf-mcp", "key", "create", "k", "--access", "read", "--all"]);
        assert_eq!(
            both.unwrap_err().kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
        assert_eq!(
            neither.unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            no_user.unwrap_err().kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_subcommands_run_against_the_database() {
        let db = crate::db::test_db().await;
        let clock = fixed_now();
        let url = Some(db.url.clone());
        let create = KeyCommand::Create(KeyCreateArgs {
            name: "k".to_string(),
            access: Access::Read,
            maintenance_report: false,
            catalogs: Vec::new(),
            all: true,
            default: None,
            actor: None,
            user: crate::db::TEST_USER.to_string(),
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

    #[tokio::test(flavor = "multi_thread")]
    async fn maintenance_capability_cli_resolves_actor_and_reissued_names() {
        let db = crate::db::test_db().await;
        let clock = fixed_now();
        let url = Some(db.url.clone());
        for actor in ["terminal", "web"] {
            let parsed = Cli::try_parse_from([
                "okf-mcp",
                "key",
                "create",
                "shared",
                "--access",
                "read",
                "--all",
                "--actor",
                actor,
                "--maintenance-report",
                "--user",
                crate::db::TEST_USER,
            ])
            .unwrap();
            let Some(Command::Key(command)) = parsed.command else {
                panic!("expected key create")
            };
            run(cli(url.clone(), Command::Key(command)), Arc::clone(&clock))
                .await
                .unwrap();
        }
        let records = keys::list(&db.pool).await.unwrap();
        assert!(records.iter().all(|record| record.maintenance_report));
        let old_id = records
            .iter()
            .find(|record| record.actor == "terminal")
            .unwrap()
            .id
            .clone();
        assert_ne!(
            old_id,
            records
                .iter()
                .find(|record| record.actor == "web")
                .unwrap()
                .id
        );
        let without_flag = Cli::try_parse_from([
            "okf-mcp",
            "key",
            "create",
            "ordinary",
            "--access",
            "admin",
            "--all",
            "--user",
            crate::db::TEST_USER,
        ])
        .unwrap();
        let Some(Command::Key(ordinary)) = without_flag.command else {
            panic!("expected key create")
        };
        run(cli(url.clone(), Command::Key(ordinary)), Arc::clone(&clock))
            .await
            .unwrap();
        assert!(
            !keys::list(&db.pool)
                .await
                .unwrap()
                .iter()
                .find(|record| record.name == "ordinary")
                .unwrap()
                .maintenance_report
        );
        assert!(Cli::try_parse_from([
            "okf-mcp",
            "key",
            "maintenance-report",
            "shared",
            "--enabled",
            "maybe"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["okf-mcp", "key", "maintenance-report", "shared"]).is_err());
        let toggle = |target: &str, actor: Option<&str>, enabled: &str| {
            let mut args = vec![
                "okf-mcp",
                "key",
                "maintenance-report",
                target,
                "--enabled",
                enabled,
            ];
            if let Some(actor) = actor {
                args.extend(["--actor", actor]);
            }
            let Some(Command::Key(command)) = Cli::try_parse_from(args).unwrap().command else {
                panic!("expected key maintenance-report")
            };
            command
        };
        assert!(run(
            cli(url.clone(), Command::Key(toggle("shared", None, "false"))),
            Arc::clone(&clock)
        )
        .await
        .is_err());
        run(
            cli(
                url.clone(),
                Command::Key(toggle("shared", Some("terminal"), "false")),
            ),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let records = keys::list(&db.pool).await.unwrap();
        assert!(
            !records
                .iter()
                .find(|record| record.id == old_id)
                .unwrap()
                .maintenance_report
        );
        assert!(
            records
                .iter()
                .find(|record| record.actor == "web")
                .unwrap()
                .maintenance_report
        );
        assert!(
            format_key_record(records.iter().find(|record| record.id == old_id).unwrap())
                .ends_with("\tmaintenance_report false")
        );
        run(
            cli(
                url.clone(),
                Command::Key(KeyCommand::Revoke {
                    id: old_id.clone(),
                    actor: None,
                }),
            ),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let parsed = Cli::try_parse_from([
            "okf-mcp",
            "key",
            "create",
            "shared",
            "--access",
            "read",
            "--all",
            "--actor",
            "terminal",
            "--user",
            crate::db::TEST_USER,
        ])
        .unwrap();
        let Some(Command::Key(command)) = parsed.command else {
            panic!("expected key create")
        };
        run(cli(url.clone(), Command::Key(command)), Arc::clone(&clock))
            .await
            .unwrap();
        let records = keys::list(&db.pool).await.unwrap();
        let replacement = records
            .iter()
            .find(|record| record.actor == "terminal" && record.revoked_at.is_none())
            .unwrap();
        let revoked_credential_id = old_id;
        let replacement_credential_id = &replacement.id;
        assert_ne!(revoked_credential_id, *replacement_credential_id);
        assert!(!replacement.maintenance_report);
        assert!(run(
            cli(
                url.clone(),
                Command::Key(toggle(&revoked_credential_id, None, "true"))
            ),
            Arc::clone(&clock)
        )
        .await
        .is_err());
        run(
            cli(
                url.clone(),
                Command::Key(toggle("shared", Some("terminal"), "true")),
            ),
            Arc::clone(&clock),
        )
        .await
        .unwrap();
        let records = keys::list(&db.pool).await.unwrap();
        assert!(
            records
                .iter()
                .find(|record| record.id == *replacement_credential_id)
                .unwrap()
                .maintenance_report
        );
        assert!(
            !records
                .iter()
                .find(|record| record.id == revoked_credential_id)
                .unwrap()
                .maintenance_report
        );
        assert!(
            records
                .iter()
                .find(|record| record.actor == "web")
                .unwrap()
                .maintenance_report
        );
        assert!(run(
            cli(
                url,
                Command::Key(toggle(replacement_credential_id, Some("terminal"), "false"))
            ),
            clock
        )
        .await
        .is_err());
    }

    #[test]
    fn key_list_columns_have_the_documented_order() {
        let record = keys::KeyRecord {
            id: "credential-id".to_string(),
            name: "claude".to_string(),
            sha256: None,
            access: Access::Write,
            maintenance_report: false,
            all_catalogs: false,
            catalogs: vec!["pg".to_string(), "work".to_string()],
            default_catalog: Some("pg".to_string()),
            actor: "claude-code-tui".to_string(),
            created_at: chrono::Utc::now(),
            revoked_at: None,
            kind: keys::KeyKind::Key,
            client_id: None,
            client_name: None,
            user: None,
        };

        assert_eq!(
            format_key_record(&record),
            "claude\tclaude-code-tui\tcredential-id\twrite\tactive\tpg,work\tdefault pg\tmaintenance_report false"
        );
    }

    #[test]
    fn oauth_code_create_parses_like_key_create() {
        let parse = |args: &[&str]| {
            // `chain` puts the program name in front, as the shell would.
            Cli::try_parse_from(["okf-mcp", "oauth", "code", "create"].iter().chain(args))
        };
        let parsed = parse(&[
            "laptop",
            "--actor",
            "example-actor",
            "--access",
            "write",
            "--catalogs",
            "main,other",
            "--default",
            "main",
            "--expires",
            "3d",
            "--user",
            "alice",
        ])
        .unwrap();
        let defaulted = parse(&[
            "c", "--actor", "a", "--access", "read", "--all", "--user", "a",
        ])
        .unwrap();

        let Some(Command::Oauth(OauthCommand::Code(CodeCommand::Create(args)))) = parsed.command
        else {
            panic!("expected oauth code create");
        };
        assert_eq!(args.name, "laptop");
        assert_eq!(args.actor, "example-actor");
        assert_eq!(args.user, "alice");
        assert_eq!(args.access, Access::Write);
        assert_eq!(args.catalogs, ["main", "other"]);
        assert!(!args.all);
        assert_eq!(args.default.as_deref(), Some("main"));
        assert_eq!(args.expires, Duration::from_secs(3 * 24 * 60 * 60));
        let Some(Command::Oauth(OauthCommand::Code(CodeCommand::Create(args)))) = defaulted.command
        else {
            panic!("expected oauth code create");
        };
        assert!(args.all);
        // `default_value = "24h"` and the store's constant agree.
        assert_eq!(args.expires, codes::DEFAULT_EXPIRY);
        // `--actor` and `--user` are required, `--expires` must parse, and
        // `--catalogs` and `--all` exclude each other.
        assert_eq!(
            parse(&["c", "--access", "read", "--all", "--user", "a"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse(&["c", "--actor", "a", "--access", "read", "--all"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse(&[
                "c",
                "--actor",
                "a",
                "--access",
                "read",
                "--all",
                "--expires",
                "soon",
                "--user",
                "a"
            ])
            .unwrap_err()
            .kind(),
            clap::error::ErrorKind::ValueValidation
        );
        assert_eq!(
            parse(&[
                "c",
                "--actor",
                "a",
                "--access",
                "read",
                "--all",
                "--catalogs",
                "main",
                "--user",
                "a"
            ])
            .unwrap_err()
            .kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
        assert!(matches!(
            Cli::try_parse_from(["okf-mcp", "oauth", "grant", "revoke", "g"]).unwrap().command,
            Some(Command::Oauth(OauthCommand::Grant(GrantCommand::Revoke { ref id }))) if id == "g"
        ));
        assert!(matches!(
            Cli::try_parse_from(["okf-mcp", "oauth", "code", "revoke", "c"]).unwrap().command,
            Some(Command::Oauth(OauthCommand::Code(CodeCommand::Revoke { ref id }))) if id == "c"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oauth_subcommands_run_against_the_database() {
        use crate::store::oauth::clients::ClientKind;
        use crate::store::oauth::codes::insert_test_client;

        let db = crate::db::test_db().await;
        let clock = fixed_now();
        let url = Some(db.url.clone());
        // Runs `okf-mcp <args>` against the test database.
        let okf = |args: &[&str]| {
            let command = Cli::try_parse_from(std::iter::once(&"okf-mcp").chain(args))
                .expect("a valid command line")
                .command
                .expect("a subcommand");
            run(cli(url.clone(), command), Arc::clone(&clock))
        };
        okf(&["catalog", "create", "main", "--owner", crate::db::TEST_TEAM])
            .await
            .unwrap();

        okf(&[
            "oauth",
            "code",
            "create",
            "laptop",
            "--actor",
            "example-actor",
            "--access",
            "read",
            "--catalogs",
            "main",
            "--user",
            crate::db::TEST_USER,
        ])
        .await
        .unwrap();
        let too_long = okf(&[
            "oauth",
            "code",
            "create",
            "late",
            "--actor",
            "example-actor",
            "--access",
            "read",
            "--all",
            "--expires",
            "8d",
            "--user",
            crate::db::TEST_USER,
        ])
        .await;
        let admin = okf(&[
            "oauth",
            "code",
            "create",
            "boss",
            "--actor",
            "example-actor",
            "--access",
            "admin",
            "--all",
            "--user",
            crate::db::TEST_USER,
        ])
        .await;
        okf(&["oauth", "code", "list"]).await.unwrap();

        let minted = codes::list(&db.pool).await.unwrap();
        assert_eq!(minted.len(), 1, "8 days and admin are refused");
        assert!(too_long.is_err() && admin.is_err());
        assert_eq!(minted[0].name, "laptop");
        assert_eq!(minted[0].catalogs, ["main"]);
        // Whole seconds: Postgres keeps microseconds, the clock nanoseconds.
        assert_eq!(
            minted[0].expires_at.timestamp(),
            (clock.now() + chrono::TimeDelta::hours(24)).timestamp()
        );
        okf(&["oauth", "code", "revoke", &minted[0].id])
            .await
            .unwrap();
        assert!(codes::list(&db.pool).await.unwrap()[0].revoked_at.is_some());
        assert!(okf(&["oauth", "code", "revoke", &minted[0].id])
            .await
            .is_err());

        // A grant, made the way the consent page makes one.
        let main = ["main".to_string()];
        let code = codes::create(
            &db.pool,
            &NewSignInCode {
                name: "phone",
                access: Access::Write,
                grants: Grants::Catalogs(&main),
                default_catalog: None,
                actor: "example-actor",
                expires_at: clock.now() + chrono::TimeDelta::hours(1),
                user: crate::db::TEST_USER,
            },
            clock.now(),
        )
        .await
        .unwrap();
        let client = insert_test_client(
            &db.pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let grant = codes::redeem(&db.pool, &code.id, &client, clock.now())
            .await
            .unwrap();
        let grant_name = keys::list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|key| key.id == grant)
            .expect("the grant")
            .name;
        // A plain key with the grant's very name and actor (allowed: the
        // live name/actor rule covers keys only).
        let plain = keys::create_with_id(
            &db.pool,
            &NewKey {
                name: &grant_name,
                access: Access::Read,
                maintenance_report: false,
                grants: Grants::All,
                default_catalog: None,
                actor: "example-actor",
                user: crate::db::TEST_USER,
            },
            clock.now(),
        )
        .await
        .unwrap();

        okf(&["oauth", "grant", "list"]).await.unwrap();
        // `key ...` by name reaches the key alone: not ambiguous, grant untouched.
        okf(&[
            "key",
            "maintenance-report",
            &grant_name,
            "--enabled",
            "true",
        ])
        .await
        .unwrap();
        // `key revoke` does not take a grant's id, and `oauth grant revoke`
        // does not take a plain key's.
        assert!(okf(&["key", "revoke", &grant]).await.is_err());
        assert!(okf(&["oauth", "grant", "revoke", &plain.id]).await.is_err());
        okf(&["oauth", "grant", "revoke", &grant]).await.unwrap();

        let rows: Vec<(String, bool, bool)> = sqlx::query_as(
            "SELECT id, revoked_at IS NOT NULL, maintenance_report FROM api_keys ORDER BY id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(rows.contains(&(grant, true, false)));
        assert!(rows.contains(&(plain.id, false, true)));
    }

    #[test]
    fn code_list_columns_have_the_documented_order() {
        let at = |text: &str| {
            chrono::DateTime::parse_from_rfc3339(text)
                .expect("a valid timestamp")
                .to_utc()
        };
        let record = SignInCodeRecord {
            id: "code-id".to_string(),
            name: "laptop".to_string(),
            access: Access::Read,
            all_catalogs: false,
            catalogs: vec!["main".to_string(), "other".to_string()],
            default_catalog: None,
            actor: "example-actor".to_string(),
            created_at: at("2026-11-11T11:00:00Z"),
            expires_at: at("2026-11-12T12:00:00Z"),
            redeemed_at: None,
            grant_id: None,
            revoked_at: None,
            user: None,
        };
        let redeemed = SignInCodeRecord {
            all_catalogs: true,
            catalogs: Vec::new(),
            default_catalog: Some("main".to_string()),
            redeemed_at: Some(at("2026-11-11T13:00:00Z")),
            grant_id: Some("grant-id".to_string()),
            ..record.clone()
        };

        assert_eq!(
            format_code_record(&record, at("2026-11-11T12:00:00Z")),
            "laptop\texample-actor\tcode-id\tread\tlive\tmain,other\tdefault -\t\
             expires 2026-11-12T12:00:00Z\tgrant -"
        );
        assert_eq!(
            format_code_record(&redeemed, at("2026-11-11T14:00:00Z")),
            "laptop\texample-actor\tcode-id\tread\tredeemed\t*\tdefault main\t\
             expires 2026-11-12T12:00:00Z\tgrant grant-id"
        );
    }

    #[test]
    fn import_parses() {
        let parsed = Cli::try_parse_from(["okf-mcp", "import", "pg", "-", "--replace"]).unwrap();

        assert!(matches!(
            parsed.command,
            Some(Command::Import { ref catalog, ref source, replace: true, ref actor, .. })
                if catalog == "pg" && source.as_os_str() == "-" && actor == "okf-mcp/import"
        ));
    }

    #[test]
    fn import_parses_the_machinery_scope_and_the_generation_guard() {
        let parsed = Cli::try_parse_from([
            "okf-mcp",
            "import",
            "pg",
            "-",
            "--replace",
            "--only-machinery",
            "--expect-generation",
            "7",
        ])
        .unwrap();
        let without_replace =
            Cli::try_parse_from(["okf-mcp", "import", "pg", "-", "--only-machinery"]);

        assert!(matches!(
            parsed.command,
            Some(Command::Import {
                only_machinery: true,
                expect_generation: Some(7),
                ..
            })
        ));
        assert!(without_replace.is_err(), "--only-machinery needs --replace");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn import_reads_a_directory() {
        let db = crate::db::test_db().await;
        let clock = fixed_now();
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
            owner: crate::db::TEST_TEAM.to_string(),
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
            only_machinery: false,
            expect_generation: None,
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
        let db = crate::db::test_db().await;
        let clock = fixed_now();
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
            owner: crate::db::TEST_TEAM.to_string(),
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
            only_machinery: false,
            expect_generation: None,
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
            only_machinery: false,
            expect_generation: None,
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
        let db = crate::db::test_db().await;
        let clock = fixed_now();
        let create = CatalogCommand::Create {
            name: "pg".to_string(),
            description: String::new(),
            owner: crate::db::TEST_TEAM.to_string(),
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
            only_machinery: false,
            expect_generation: None,
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
