#![allow(clippy::doc_lazy_continuation)]
use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cli;

/// Initializes logging and runs the CLI application.
/// Parses command-line arguments and configures a tracing subscriber that logs to stderr. The log level is set to debug if the `--verbose` flag is provided, otherwise it uses the `RUST_LOG` environment variable or defaults to warn level (with ort module set to error).
/// # Returns
/// Returns a `Result<()>` indicating success or failure of the CLI execution.
/// # Errors
/// Returns an error if the CLI application execution fails.
fn main() -> Result<()> {
    // Parse CLI first to check verbose flag
    let cli = cli::Cli::parse();

    // Log to stderr to keep stdout clean for structured output.
    // P1.20 / OB-V1.30-1: --verbose flag sets debug level for cqs (everything
    // else stays at info), otherwise honour RUST_LOG, defaulting to
    // "cqs=info,warn,ort=error" so the ~150 span instrumentation sites in the
    // codebase actually render without third-party noise.
    let filter = if cli.verbose {
        EnvFilter::new("cqs=debug,info")
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("cqs=info,warn,ort=error"))
    };

    // FmtSpan::CLOSE emits a synthetic event on span close with elapsed time —
    // turns every `info_span!("foo", ...).entered()` into a "foo" + latency
    // line in the journal automatically. Without it, only events emitted
    // *inside* a span produce log lines; entry/exit pairs disappear.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .init();

    cli::run_with(cli)
}
