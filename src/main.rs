use anyhow::Result;

mod cli;

fn main() -> Result<()> {
    // Log to stderr to keep stdout clean for MCP JSON-RPC
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    cli::run()
}
