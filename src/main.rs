//! The `okf_mcp` binary. For now a stub that only sets up logging; the MCP
//! server arrives in milestone M3.

use tracing_subscriber::EnvFilter;

/// Installs a `tracing` subscriber that logs to stderr, filtered by
/// `RUST_LOG` (default `warn`).
fn main() {
    // `try_from_default_env` fails when `RUST_LOG` is unset or invalid; fall
    // back to warnings only rather than aborting.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    tracing::warn!("okf_mcp has no server yet; the MCP server arrives in M3");
}
