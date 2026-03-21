use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;

/// Initializes logging and runs the CLI application.
///
/// Parses command-line arguments and configures a tracing subscriber that logs to stderr. The log level is set to debug if the `--verbose` flag is provided, otherwise it uses the `RUST_LOG` environment variable or defaults to warn level (with ort module set to error).
///
/// # Returns
///
/// Returns a `Result<()>` indicating success or failure of the CLI execution.
///
/// # Errors
///
/// Returns an error if the CLI application execution fails.
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
