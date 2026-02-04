use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod cli;

fn main() -> Result<()> {
    // Log to stderr to keep stdout clean for MCP JSON-RPC
    // Use RUST_LOG env var for filtering, default to warn level
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    cli::run()
}
