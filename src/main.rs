use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;

fn main() -> Result<()> {
    // Parse CLI first to check verbose flag
    let cli = cli::Cli::parse();

    // Log to stderr to keep stdout clean for structured output
    // --verbose flag sets debug level, otherwise use RUST_LOG or default to warn
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,ort=error"))
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    cli::run_with(cli)
}
