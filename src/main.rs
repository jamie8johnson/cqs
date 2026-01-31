use anyhow::Result;

mod cli;

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    cli::run()
}
