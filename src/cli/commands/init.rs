//! Init command for cqs
//!
//! Creates .cqs/ directory and downloads the embedding model.

use anyhow::{Context, Result};

use cqs::Embedder;

use crate::cli::{find_project_root, Cli};

/// Initialize cqs in a project directory
///
/// Creates `.cqs/` directory, downloads the embedding model, and warms up the embedder.
pub(crate) fn cmd_init(cli: &Cli) -> Result<()> {
    let _span = tracing::info_span!("cmd_init").entered();
    let root = find_project_root();
    let cqs_dir = root.join(cqs::INDEX_DIR);

    if !cli.quiet {
        println!("Initializing cqs...");
    }

    // Create .cqs directory
    std::fs::create_dir_all(&cqs_dir).context("Failed to create .cqs directory")?;

    // Set restrictive permissions on .cqs directory (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&cqs_dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::debug!(error = %e, "Failed to set .cqs directory permissions");
        }
    }

    // Create .gitignore
    let gitignore = cqs_dir.join(".gitignore");
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
        println!("Created .cqs/");
        println!();
        println!("Run 'cqs index' to index your codebase.");
    }

    Ok(())
}
