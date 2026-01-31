use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cq")]
#[command(about = "Semantic code search with local embeddings")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Search query
    #[arg(trailing_var_arg = true)]
    query: Vec<String>,

    /// Max results
    #[arg(short = 'n', long, default_value = "5")]
    limit: usize,

    /// Min similarity threshold
    #[arg(short = 't', long, default_value = "0.3")]
    threshold: f32,

    /// Filter by language
    #[arg(short = 'l', long)]
    lang: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Suppress progress output
    #[arg(short, long)]
    quiet: bool,

    /// Show debug info
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Download model and create .cq/
    Init,
    /// Check model, index, hardware
    Doctor,
    /// Index current project
    Index,
    /// Show index statistics
    Stats,
    /// Show/edit configuration
    Config,
    /// Download latest model
    UpdateModel,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Init) => {
            println!("cq init - not implemented");
        }
        Some(Commands::Doctor) => {
            println!("cq doctor - not implemented");
        }
        Some(Commands::Index) => {
            println!("cq index - not implemented");
        }
        Some(Commands::Stats) => {
            println!("cq stats - not implemented");
        }
        Some(Commands::Config) => {
            println!("cq config - not implemented");
        }
        Some(Commands::UpdateModel) => {
            println!("cq update-model - not implemented");
        }
        None => {
            if cli.query.is_empty() {
                println!("Usage: cq <query> or cq <command>");
                println!("Run 'cq --help' for more information.");
            } else {
                let query = cli.query.join(" ");
                println!("Searching for: {}", query);
                println!("(search not implemented)");
            }
        }
    }

    Ok(())
}
