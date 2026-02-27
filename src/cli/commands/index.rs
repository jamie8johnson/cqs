//! Index command for cqs
//!
//! Indexes codebase files for semantic search.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use cqs::{parse_notes, Embedder, HnswIndex, ModelInfo, Parser as CqParser, Store};

use crate::cli::{
    acquire_index_lock, check_interrupted, enumerate_files, find_project_root, reset_interrupted,
    run_index_pipeline, signal, Cli,
};

/// Index codebase files for semantic search
///
/// Parses source files, generates embeddings, and stores them in the index database.
/// Uses incremental indexing by default (only re-embeds changed files).
pub(crate) fn cmd_index(cli: &Cli, force: bool, dry_run: bool, no_ignore: bool) -> Result<()> {
    reset_interrupted();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    // Ensure .cqs directory exists
    if !cqs_dir.exists() {
        std::fs::create_dir_all(&cqs_dir)
            .with_context(|| format!("Failed to create {}", cqs_dir.display()))?;
    }

    // Acquire lock (unless dry run)
    let _lock = if !dry_run {
        Some(acquire_index_lock(&cqs_dir)?)
    } else {
        None
    };

    signal::setup_signal_handler();

    let _span = tracing::info_span!("cmd_index", force = force, dry_run = dry_run).entered();

    if !cli.quiet {
        println!("Scanning files...");
    }

    let parser = CqParser::new()?;
    let files = enumerate_files(&root, &parser, no_ignore)?;

    if !cli.quiet {
        println!("Found {} files", files.len());
    }

    if dry_run {
        for file in &files {
            println!("  {}", file.display());
        }
        println!();
        println!("(dry run - no changes made)");
        return Ok(());
    }

    // Initialize or open store
    let store = if index_path.exists() && !force {
        Store::open(&index_path)?
    } else {
        // Remove old index if forcing
        if index_path.exists() {
            std::fs::remove_file(&index_path)
                .with_context(|| format!("Failed to remove {}", index_path.display()))?;
        }
        let store = Store::open(&index_path)?;
        store.init(&ModelInfo::default())?;
        store
    };

    if !cli.quiet {
        println!("Indexing {} files (pipelined)...", files.len());
    }

    // Run the 3-stage pipeline: parse → embed → write
    let stats = run_index_pipeline(&root, files.clone(), &index_path, force, cli.quiet)?;
    let total_embedded = stats.total_embedded;
    let total_cached = stats.total_cached;
    let gpu_failures = stats.gpu_failures;

    // Prune missing files
    let existing_files: HashSet<_> = files.into_iter().collect();
    let pruned = store
        .prune_missing(&existing_files)
        .context("Failed to prune deleted files from index")?;

    if !cli.quiet {
        println!();
        println!("Index complete:");
        let newly_embedded = total_embedded - total_cached;
        if total_cached > 0 {
            println!(
                "  Chunks: {} ({} cached, {} embedded)",
                total_embedded, total_cached, newly_embedded
            );
        } else {
            println!("  Embedded: {}", total_embedded);
        }
        if gpu_failures > 0 {
            println!("  GPU failures: {} (fell back to CPU)", gpu_failures);
        }
        if pruned > 0 {
            println!("  Pruned: {} (deleted files)", pruned);
        }
        if stats.parse_errors > 0 {
            println!(
                "  Parse errors: {} (see logs for details)",
                stats.parse_errors
            );
        }
    }

    // Extract call graph + type edges (includes large functions >100 lines)
    if !check_interrupted() {
        if !cli.quiet {
            println!("Extracting relationships...");
        }

        let (total_calls, total_type_edges) =
            extract_relationships(&parser, &root, &existing_files, &store)?;

        if !cli.quiet {
            println!("  Call graph: {} calls", total_calls);
            println!("  Type edges: {} edges", total_type_edges);
        }
    }

    // Index notes if notes.toml exists
    if !check_interrupted() {
        if !cli.quiet {
            println!("Indexing notes...");
        }

        let (note_count, was_skipped) = index_notes_from_file(&root, &store, force)?;

        if !cli.quiet {
            if was_skipped && note_count == 0 {
                println!("Notes up to date.");
            } else if note_count > 0 {
                let ns = store
                    .note_stats()
                    .context("Failed to read note statistics")?;
                println!(
                    "  Notes: {} total ({} warnings, {} patterns)",
                    ns.total, ns.warnings, ns.patterns
                );
            }
        }
    }

    // Build HNSW index for fast chunk search (notes use brute-force from SQLite)
    if !check_interrupted() {
        if !cli.quiet {
            println!("Building HNSW index...");
        }

        if let Some(total) = build_hnsw_index(&store, &cqs_dir)? {
            if !cli.quiet {
                println!("  HNSW index: {} vectors", total);
            }
        }
    }

    Ok(())
}

/// Extract call graph and type edges from source files.
///
/// Uses `parse_file_relationships()` for a single parse pass that returns
/// both call sites and type references. Returns (total_calls, total_type_edges).
fn extract_relationships(
    parser: &CqParser,
    root: &Path,
    files: &HashSet<PathBuf>,
    store: &Store,
) -> Result<(usize, usize)> {
    let _span = tracing::info_span!("extract_relationships", file_count = files.len()).entered();
    let mut total_calls = 0;
    let mut total_type_edges = 0;
    for file in files {
        let abs_path = root.join(file);
        match parser.parse_file_relationships(&abs_path) {
            Ok((function_calls, chunk_type_refs)) => {
                for fc in &function_calls {
                    total_calls += fc.calls.len();
                }
                store
                    .upsert_function_calls(file, &function_calls)
                    .context("Failed to store function calls")?;

                if !chunk_type_refs.is_empty() {
                    for ctr in &chunk_type_refs {
                        total_type_edges += ctr.type_refs.len();
                    }
                    if let Err(e) = store.upsert_type_edges_for_file(file, &chunk_type_refs) {
                        tracing::warn!(
                            file = %abs_path.display(),
                            error = %e,
                            "Failed to store type edges"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to extract relationships from {}: {}",
                    abs_path.display(),
                    e
                );
            }
        }
    }
    tracing::info!(
        total_calls,
        total_type_edges,
        "Relationship extraction complete"
    );
    Ok((total_calls, total_type_edges))
}

/// Index notes from notes.toml if it exists and needs reindexing
///
/// Returns (indexed_count, was_skipped) where was_skipped is true if notes were up to date.
fn index_notes_from_file(root: &Path, store: &Store, force: bool) -> Result<(usize, bool)> {
    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok((0, true));
    }

    // Check if notes need reindexing (Some(mtime) = needs reindex, None = up to date)
    let needs_reindex = force
        || store
            .notes_need_reindex(&notes_path)
            .unwrap_or(Some(0))
            .is_some();

    if !needs_reindex {
        return Ok((0, true));
    }

    match parse_notes(&notes_path) {
        Ok(notes) => {
            if notes.is_empty() {
                return Ok((0, false));
            }

            let embedder = Embedder::new()?;
            let count = cqs::index_notes(&notes, &notes_path, &embedder, store)?;
            Ok((count, false))
        }
        Err(e) => {
            tracing::warn!("Failed to parse notes: {}", e);
            Ok((0, false))
        }
    }
}

/// Build HNSW index from store embeddings
///
/// Creates an HNSW index containing chunk embeddings only.
///
/// Notes are excluded from HNSW — they use brute-force search from SQLite
/// so that notes are immediately searchable without rebuild.
pub(crate) fn build_hnsw_index(store: &Store, cqs_dir: &Path) -> Result<Option<usize>> {
    let chunk_count = store.chunk_count().context("Failed to read chunk count")? as usize;
    let _span = tracing::info_span!("build_hnsw_index", chunk_count).entered();

    if chunk_count == 0 {
        return Ok(None);
    }

    const HNSW_BATCH_SIZE: usize = 10_000;

    let chunk_batches = store.embedding_batches(HNSW_BATCH_SIZE);

    let hnsw = HnswIndex::build_batched(chunk_batches, chunk_count)?;
    hnsw.save(cqs_dir, "index")?;

    Ok(Some(hnsw.len()))
}
