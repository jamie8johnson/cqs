//! Reference index commands for cqs
//!
//! Manages reference indexes for multi-index search.
//! References are read-only indexes of external codebases.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use cqs::config::{add_reference_to_config, remove_reference_from_config, ReferenceConfig};
use cqs::reference;
use cqs::{ModelInfo, Parser as CqParser, Store};

use crate::cli::commands::index::build_hnsw_index;
use crate::cli::{enumerate_files, find_project_root, run_index_pipeline, Cli};

/// Reference subcommands
#[derive(clap::Subcommand)]
pub(crate) enum RefCommand {
    /// Add a reference index from an external codebase
    Add {
        /// Reference name (used in results and commands)
        name: String,
        /// Path to the source codebase to index
        source: PathBuf,
        /// Score weight multiplier (0.0-1.0, default 0.8)
        #[arg(long, default_value = "0.8")]
        weight: f32,
    },
    /// List configured references
    List,
    /// Remove a reference index
    Remove {
        /// Name of the reference to remove
        name: String,
    },
    /// Update a reference index from its source
    Update {
        /// Name of the reference to update
        name: String,
    },
}

/// Handle ref subcommands
pub(crate) fn cmd_ref(cli: &Cli, subcmd: &RefCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_ref").entered();
    match subcmd {
        RefCommand::Add {
            name,
            source,
            weight,
        } => cmd_ref_add(cli, name, source, *weight),
        RefCommand::List => cmd_ref_list(cli),
        RefCommand::Remove { name } => cmd_ref_remove(name),
        RefCommand::Update { name } => cmd_ref_update(cli, name),
    }
}

fn cmd_ref_add(cli: &Cli, name: &str, source: &std::path::Path, weight: f32) -> Result<()> {
    let root = find_project_root();
    let config = cqs::config::Config::load(&root);

    // Validate weight
    if !(0.0..=1.0).contains(&weight) {
        bail!("Weight must be between 0.0 and 1.0 (got {})", weight);
    }

    // Check for duplicate
    if config.references.iter().any(|r| r.name == name) {
        bail!(
            "Reference '{}' already exists. Use 'cqs ref update {}' to re-index.",
            name,
            name
        );
    }

    // Validate source
    let source = dunce::canonicalize(source)
        .map_err(|e| anyhow::anyhow!("Source path '{}' not found: {}", source.display(), e))?;

    // Create reference directory with restrictive permissions
    let ref_dir = reference::ref_path(name)
        .ok_or_else(|| anyhow::anyhow!("Could not determine reference storage directory"))?;
    std::fs::create_dir_all(&ref_dir)
        .with_context(|| format!("Failed to create {}", ref_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&ref_dir, std::fs::Permissions::from_mode(0o700));
    }
    let db_path = ref_dir.join("index.db");

    // Initialize store schema
    {
        let store = Store::open(&db_path)?;
        store.init(&ModelInfo::default())?;
        // Drop store — pipeline opens its own
    }

    // Enumerate files
    let parser = CqParser::new()?;
    let files = enumerate_files(&source, &parser, false)?;

    if files.is_empty() {
        bail!("No supported source files found in '{}'", source.display());
    }

    if !cli.quiet {
        println!(
            "Indexing {} files from '{}'...",
            files.len(),
            source.display()
        );
    }

    // Run indexing pipeline
    let stats = run_index_pipeline(&source, files, &db_path, false, cli.quiet)?;

    if !cli.quiet {
        println!("  Embedded: {} chunks", stats.total_embedded);
    }

    // Build HNSW index
    let store = Store::open(&db_path)?;
    if let Some(count) = build_hnsw_index(&store, &ref_dir)? {
        if !cli.quiet {
            println!("  HNSW: {} vectors", count);
        }
    }

    // Add to config
    let ref_config = ReferenceConfig {
        name: name.to_string(),
        path: ref_dir,
        source: Some(source),
        weight,
    };
    let config_path = root.join(".cqs.toml");
    add_reference_to_config(&config_path, &ref_config)?;

    if !cli.quiet {
        println!("Reference '{}' added.", name);
    }
    Ok(())
}

fn cmd_ref_list(cli: &Cli) -> Result<()> {
    let root = find_project_root();
    let config = cqs::config::Config::load(&root);

    if config.references.is_empty() {
        println!("No references configured.");
        return Ok(());
    }

    if cli.json {
        let refs: Vec<_> = config
            .references
            .iter()
            .map(|r| {
                let chunks = Store::open(&r.path.join("index.db"))
                    .ok()
                    .and_then(|s| s.chunk_count().ok())
                    .unwrap_or(0);
                serde_json::json!({
                    "name": r.name,
                    "path": r.path.to_string_lossy(),
                    "source": r.source.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "weight": r.weight,
                    "chunks": chunks,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&refs)?);
        return Ok(());
    }

    println!("{:<15} {:<8} {:<10} SOURCE", "NAME", "WEIGHT", "CHUNKS");
    println!("{}", "─".repeat(60));

    for r in &config.references {
        let chunks = Store::open(&r.path.join("index.db"))
            .ok()
            .and_then(|s| s.chunk_count().ok())
            .unwrap_or(0);
        let source_str = r
            .source
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(none)".to_string());
        println!(
            "{:<15} {:<8.2} {:<10} {}",
            r.name, r.weight, chunks, source_str
        );
    }

    Ok(())
}

fn cmd_ref_remove(name: &str) -> Result<()> {
    let root = find_project_root();
    let config_path = root.join(".cqs.toml");
    let removed = remove_reference_from_config(&config_path, name)?;

    if !removed {
        bail!("Reference '{}' not found in config.", name);
    }

    // Delete reference directory — only via canonical ref_path() to prevent
    // config-supplied paths from deleting arbitrary directories
    if let Some(refs_root) = reference::refs_dir() {
        let ref_dir = refs_root.join(name);
        if ref_dir.exists() {
            // Verify the path is actually inside the refs directory
            if let (Ok(canonical_dir), Ok(canonical_root)) = (
                dunce::canonicalize(&ref_dir),
                dunce::canonicalize(&refs_root),
            ) {
                if canonical_dir.starts_with(&canonical_root) {
                    std::fs::remove_dir_all(&canonical_dir)
                        .with_context(|| format!("Failed to remove {}", ref_dir.display()))?;
                } else {
                    tracing::warn!(
                        path = %canonical_dir.display(),
                        "Refusing to delete reference directory outside refs root"
                    );
                }
            }
        }
    }

    println!("Reference '{}' removed.", name);
    Ok(())
}

fn cmd_ref_update(cli: &Cli, name: &str) -> Result<()> {
    let root = find_project_root();
    let config = cqs::config::Config::load(&root);

    let ref_config = config
        .references
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow::anyhow!("Reference '{}' not found in config.", name))?;

    let source = ref_config
        .source
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Reference '{}' has no source path configured.", name))?;

    if !source.exists() {
        bail!(
            "Source path '{}' does not exist. Update the config or remove and re-add the reference.",
            source.display()
        );
    }

    let db_path = ref_config.path.join("index.db");
    let ref_dir = &ref_config.path;

    // Get current chunk count before modifying anything
    let existing_chunks = if db_path.exists() {
        Store::open(&db_path)
            .ok()
            .and_then(|s| s.chunk_count().ok())
            .unwrap_or(0)
    } else {
        0
    };

    // Enumerate files
    let parser = CqParser::new()?;
    let files = enumerate_files(source, &parser, false)?;

    // Guard: if the binary finds 0 files but the index has chunks, abort.
    // This happens when the binary doesn't support languages in the index.
    if files.is_empty() && existing_chunks > 0 {
        bail!(
            "No supported files found in '{}', but the index has {} chunks.\n\
             This usually means the binary doesn't support the language(s) in the index.\n\
             Rebuild with a binary that supports the required languages, or use \
             'cqs ref remove {name}' and re-add.",
            source.display(),
            existing_chunks,
        );
    }

    if !cli.quiet {
        println!("Updating reference '{}' ({} files)...", name, files.len());
    }

    // Run incremental indexing pipeline
    let stats = run_index_pipeline(source, files.clone(), &db_path, false, cli.quiet)?;

    if !cli.quiet {
        let newly = stats.total_embedded - stats.total_cached;
        println!(
            "  Chunks: {} ({} cached, {} embedded)",
            stats.total_embedded, stats.total_cached, newly
        );
    }

    // Prune chunks for deleted files
    let store = Store::open(&db_path)?;
    let existing_files: HashSet<_> = files.into_iter().collect();
    let pruned = store.prune_missing(&existing_files)?;

    // Guard: if pruning would remove >50% of existing chunks, warn loudly
    if pruned > 0 && existing_chunks > 0 {
        let remaining = existing_chunks.saturating_sub(pruned as u64);
        if remaining == 0 {
            tracing::warn!(
                pruned,
                name,
                "All chunks were pruned. The index is now empty. \
                 If this was unintentional, re-index with 'cqs ref update'.",
            );
        } else if (pruned as u64) > existing_chunks / 2 {
            tracing::warn!(
                pruned,
                existing_chunks,
                pct = (pruned as f64 / existing_chunks as f64) * 100.0,
                "Pruned over 50% of chunks. Verify source path is correct.",
            );
        }
    }

    if !cli.quiet && pruned > 0 {
        println!("  Pruned: {} (deleted files)", pruned);
    }

    // Rebuild HNSW
    if let Some(count) = build_hnsw_index(&store, ref_dir)? {
        if !cli.quiet {
            println!("  HNSW: {} vectors", count);
        }
    }

    if !cli.quiet {
        println!("Reference '{}' updated.", name);
    }
    Ok(())
}
