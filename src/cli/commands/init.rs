//! Init command for cqs
//!
//! Creates .cq/ directory and downloads the embedding model.

use anyhow::{Context, Result};

use cqs::Embedder;

use crate::cli::{find_project_root, Cli};

/// Initialize cq in a project directory
///
/// Creates `.cq/` directory, downloads the embedding model, and warms up the embedder.
pub(crate) fn cmd_init(cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");

    if !cli.quiet {
        println!("Initializing cq...");
    }

    // Create .cq directory
    std::fs::create_dir_all(&cq_dir).context("Failed to create .cq directory")?;

    // Create .gitignore
    let gitignore = cq_dir.join(".gitignore");
    std::fs::write(
        &gitignore,
        "index.db\nindex.db-wal\nindex.db-shm\nindex.lock\n",
    )
    .context("Failed to create .gitignore")?;

    // Download model
    if !cli.quiet {
        println!("Downloading model (~547MB)...");
    }

    let embedder = Embedder::new().context("Failed to initialize embedder")?;

    if !cli.quiet {
        println!("Detecting hardware... {}", embedder.provider());
    }

    // Warm up
    embedder.warm()?;

    if !cli.quiet {
        println!("Created .cq/");
        println!();
        println!("Run 'cq index' to index your codebase.");
    }

    Ok(())
}
