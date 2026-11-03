//! The `okf-mcp` binary: parses the command line and runs it
//! ([`okf_mcp::cli::run`]).

use std::sync::Arc;

use clap::Parser;
use okf_mcp::cli::Cli;
use okf_mcp::clock::SystemClock;
use tracing_subscriber::EnvFilter;

/// The log filter when `RUST_LOG` is unset: warnings, plus this crate's and
/// the request traces' `info` lines.
const DEFAULT_LOG_FILTER: &str = "warn,okf_mcp=info,tower_http=info";

/// Installs a `tracing` subscriber on stderr (`RUST_LOG`, default
/// [`DEFAULT_LOG_FILTER`]: the server logs `serving`, catalog loads and
/// shutdown, and `tower_http` one line per request, at `info`), then runs the
/// command line with the system clock.
// `#[tokio::main]` wraps `main` in a tokio runtime, so it can `.await`.
// Returning `anyhow::Result` prints the error chain and exits non-zero.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `try_from_default_env` fails when `RUST_LOG` is unset or invalid; fall
    // back to the default filter rather than aborting.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    // `Arc::new` puts the clock on the heap behind a shared handle; the server
    // keeps one for as long as it runs.
    okf_mcp::cli::run(Cli::parse(), Arc::new(SystemClock)).await
}
