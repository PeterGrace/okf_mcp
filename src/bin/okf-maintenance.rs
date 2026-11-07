//! Report-only operator CLI. Dependency tracing is deliberately disabled.

use std::{future::Future, io::Write, path::PathBuf, process::ExitCode, sync::Arc};

use clap::{error::ErrorKind, Parser, Subcommand};
use okf_mcp::{
    clock::SystemClock,
    investigator::{
        claude::{self, ClaudeAdapter, Inference},
        client::{self, CatalogClient, McpClient},
        config::{RunnerConfig, Secrets},
        contract,
        error::RunnerError,
        evaluate,
        journal::JournalStore,
        supervisor,
        types::RunSummary,
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "okf-maintenance",
    version,
    about = "Report-only OKF maintenance runner"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read-only config, credential presence, CLI version/help and MCP schema checks.
    Preflight {
        #[arg(long)]
        config: PathBuf,
    },
    /// Investigate or recover one bounded batch; requires both configured consents.
    RunOnce {
        #[arg(long)]
        config: PathBuf,
    },
    /// Evaluate a version-1 JSON corpus offline, without credentials or inference.
    Evaluate {
        #[arg(long)]
        cases: PathBuf,
    },
}

// Private injection boundary: tests replace network/inference, not the controller,
// configuration, readiness subprocess, journal, cancellation or output mapping.
trait Backend {
    type Client: CatalogClient;
    type Model: Inference + 'static;
    async fn connect(
        &mut self,
        config: &RunnerConfig,
        secrets: &Secrets,
    ) -> Result<Self::Client, RunnerError>;
    fn model(
        &mut self,
        config: &RunnerConfig,
        secrets: &Secrets,
    ) -> Result<Self::Model, RunnerError>;
}

struct Production;
impl Backend for Production {
    type Client = McpClient;
    type Model = ClaudeAdapter;
    async fn connect(
        &mut self,
        config: &RunnerConfig,
        secrets: &Secrets,
    ) -> Result<McpClient, RunnerError> {
        client::connect(config, secrets).await
    }
    fn model(
        &mut self,
        config: &RunnerConfig,
        secrets: &Secrets,
    ) -> Result<ClaudeAdapter, RunnerError> {
        ClaudeAdapter::new(config, secrets)
    }
}

fn write_json(value: &impl serde::Serialize, output: &mut impl Write) -> Result<(), RunnerError> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| RunnerError::Contract)?;
    bytes.push(b'\n');
    output.write_all(&bytes).map_err(|_| RunnerError::Io)
}

async fn dispatch(cli: Cli) -> Result<i32, RunnerError> {
    dispatch_with(cli, &mut Production, &mut std::io::stdout()).await
}

async fn dispatch_with(
    cli: Cli,
    backend: &mut impl Backend,
    output: &mut impl Write,
) -> Result<i32, RunnerError> {
    let (path, preflight) = match cli.command {
        Command::Evaluate { cases } => {
            write_json(&evaluate::run(&cases)?, output)?;
            return Ok(0);
        }
        Command::Preflight { config } => (config, true),
        Command::RunOnce { config } => (config, false),
    };
    let started = tokio::time::Instant::now();
    let config = RunnerConfig::load(&path).map_err(|_| RunnerError::Config)?;
    config.validate_consent()?;
    let secrets = Secrets::from_env()?;
    let deadline = started + std::time::Duration::from_secs(config.max_run_seconds);
    let cancel = CancellationToken::new();
    // Both modes must listen before starting any version/help subprocess.
    let signals = Signals::new()?;
    if preflight {
        let readiness = async {
            let probe = claude::preflight_cli_with_cancel(&config, cancel.clone());
            tokio::pin!(probe);
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => {
                    cancel.cancel();
                    let _ = probe.await;
                    return Err(RunnerError::Budget);
                }
                result = &mut probe => result?,
            }
            let mut client =
                network_until(backend.connect(&config, &secrets), &cancel, deadline).await?;
            contract::validate_tools(
                &network_until(client.list_tools(), &cancel, deadline).await?,
            )?;
            if cancel.is_cancelled() {
                return Err(RunnerError::Cancelled);
            }
            Ok(())
        };
        until_signal(readiness, &cancel, signals).await?;
        write_json(
            &RunSummary {
                run_id: None,
                reported: 0,
                inconclusive: 0,
                failed: 0,
                skipped: 0,
                invocations: 0,
                accounting_known: true,
                reason: "preflight_passed".into(),
            },
            output,
        )?;
        // Readiness is not a completed run, paid-envelope check or grant check.
        return Ok(0);
    }
    let store = JournalStore::open(&config.state_dir, &config.binding(secrets.token())?)?;
    let run = async {
        let mut client =
            network_until(backend.connect(&config, &secrets), &cancel, deadline).await?;
        let model = Arc::new(backend.model(&config, &secrets)?);
        supervisor::run_once_with_deadline(
            &config,
            &mut client,
            model,
            &store,
            &SystemClock,
            cancel.clone(),
            deadline,
        )
        .await
    };
    let summary = until_signal(run, &cancel, signals).await?;
    let status = summary.exit_code();
    write_json(&summary, output)?;
    Ok(i32::from(status))
}

// Only network-only setup belongs here: dropping an owned process future would
// bypass its cancellation-and-reap completion boundary.
async fn network_until<T>(
    future: impl Future<Output = Result<T, RunnerError>>,
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<T, RunnerError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(RunnerError::Cancelled),
        _ = tokio::time::sleep_until(deadline) => Err(RunnerError::Budget),
        result = future => result,
    }
}

struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}
impl Signals {
    fn new() -> Result<Self, RunnerError> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt()).map_err(|_| RunnerError::Io)?,
                terminate: signal(SignalKind::terminate()).map_err(|_| RunnerError::Io)?,
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }
    async fn receive(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn until_signal<T>(
    run: impl Future<Output = T>,
    cancel: &CancellationToken,
    mut signals: Signals,
) -> T {
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => result,
        _ = signals.receive() => {
            cancel.cancel();
            // The supervisor owns inference cleanup and durable close/recovery.
            // Dropping/aborting it here would lose that completion boundary.
            run.await
        }
    }
}

fn quiet_logging() -> Result<(), RunnerError> {
    // rmcp can trace complete protocol messages, including evidence and tokens.
    // Never use EnvFilter/RUST_LOG or offer a dependency-verbose override here.
    tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::default())
        .map_err(|_| RunnerError::Config)
}

fn entry() -> Result<i32, RunnerError> {
    quiet_logging()?;
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.print().map_err(|_| RunnerError::Io)?;
            return Ok(0);
        }
        Err(_) => return Err(RunnerError::Config),
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| RunnerError::Io)?
        .block_on(dispatch(cli))
}

fn main() -> ExitCode {
    match entry() {
        Ok(status) => ExitCode::from(status as u8),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "okf-maintenance/tests.rs"]
mod tests;
