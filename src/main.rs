//! The `okf-mcp` binary: parses the command line and runs it
//! ([`okf_mcp::cli::run`]).

use std::sync::Arc;

use clap::Parser;
use okf_mcp::cli::Cli;
use okf_mcp::clock::SystemClock;
use tracing_subscriber::EnvFilter;

/// Installs a `tracing` subscriber on stderr (`RUST_LOG`, default
/// `warn,okf_mcp=info`: the server logs `serving`, catalog loads and shutdown
/// at `info`), then runs the command line with the system clock.
// `#[tokio::main]` wraps `main` in a tokio runtime, so it can `.await`.
// Returning `anyhow::Result` prints the error chain and exits non-zero.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `try_from_default_env` fails when `RUST_LOG` is unset or invalid; fall
    // back to warnings, plus this crate's info lines, rather than aborting.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,okf_mcp=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    // `Arc::new` puts the clock on the heap behind a shared handle; the server
    // keeps one for as long as it runs.
    okf_mcp::cli::run(Cli::parse(), Arc::new(SystemClock)).await
}
