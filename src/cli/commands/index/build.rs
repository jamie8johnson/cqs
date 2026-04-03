//! Index command for cqs
//!
//! Indexes codebase files for semantic search.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use std::sync::Arc;

use cqs::{parse_notes, Embedder, HnswIndex, ModelInfo, Parser as CqParser, Store};

use crate::cli::{
    acquire_index_lock, args::IndexArgs, check_interrupted, enumerate_files, find_project_root,
    reset_interrupted, run_index_pipeline, signal, Cli,
};

/// Index codebase files for semantic search
///
/// Parses source files, generates embeddings, and stores them in the index database.
/// Uses incremental indexing by default (only re-embeds changed files).
pub(crate) fn cmd_index(cli: &Cli, args: &IndexArgs) -> Result<()> {
    let force = args.force;
    let dry_run = args.dry_run;
    let no_ignore = args.no_ignore;

    #[cfg(feature = "llm-summaries")]
    let llm_summaries = args.llm_summaries;
    #[cfg(not(feature = "llm-summaries"))]
    let llm_summaries = false;
    #[cfg(feature = "llm-summaries")]
    let improve_docs = args.improve_docs;
    #[cfg(not(feature = "llm-summaries"))]
    let improve_docs = false;
    #[cfg(feature = "llm-summaries")]
    let improve_all = args.improve_all;
    #[cfg(not(feature = "llm-summaries"))]
    let improve_all = false;
    #[cfg(feature = "llm-summaries")]
    let max_docs = args.max_docs;
    #[cfg(not(feature = "llm-summaries"))]
    let max_docs: Option<usize> = None;
    #[cfg(feature = "llm-summaries")]
    let hyde_queries = args.hyde_queries;
    #[cfg(not(feature = "llm-summaries"))]
    let hyde_queries = false;
    #[cfg(feature = "llm-summaries")]
    let max_hyde = args.max_hyde;
    #[cfg(not(feature = "llm-summaries"))]
    let max_hyde: Option<usize> = None;

    reset_interrupted();

    // Validate: --improve-docs requires --llm-summaries
    #[cfg(feature = "llm-summaries")]
    if improve_docs && !llm_summaries {
        anyhow::bail!("--improve-docs requires --llm-summaries");
    }
    #[cfg(feature = "llm-summaries")]
    if improve_all && !improve_docs {
        anyhow::bail!("--improve-all requires --improve-docs");
    }

    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    // Ensure .cqs directory exists with restrictive permissions
    if !cqs_dir.exists() {
        std::fs::create_dir_all(&cqs_dir)
            .with_context(|| format!("Failed to create {}", cqs_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&cqs_dir, std::fs::Permissions::from_mode(0o700))
            {
                tracing::debug!(path = %cqs_dir.display(), error = %e, "Failed to set file permissions");
            }
        }
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

    // Initialize or open store.
    // When --force, back up the old DB instead of deleting it.
    // If interrupted during rebuild, the backup remains recoverable.
    let backup_path = cqs_dir.join("index.db.bak");
    let store = if index_path.exists() && !force {
        Store::open(&index_path)
            .with_context(|| format!("Failed to open store at {}", index_path.display()))?
    } else {
        if index_path.exists() {
            std::fs::rename(&index_path, &backup_path)
                .with_context(|| format!("Failed to back up {}", index_path.display()))?;
            // DS-13: Also remove WAL/SHM files left by SQLite — stale journal
            // files from the old DB would corrupt the fresh database.
            for suffix in &["-wal", "-shm"] {
                let journal = cqs_dir.join(format!("index.db{suffix}"));
                if journal.exists() {
                    if let Err(e) = std::fs::remove_file(&journal) {
                        tracing::warn!(path = %journal.display(), error = %e,
                            "Failed to remove stale SQLite journal file");
                    }
                }
            }
        }
        let mut store = Store::open(&index_path)
            .with_context(|| format!("Failed to create store at {}", index_path.display()))?;
        let mc = cli.model_config();
        store.init(&ModelInfo::new(&mc.repo, mc.dim))?;
        // Update dim to match the model — open() defaulted to EMBEDDING_DIM
        // because metadata didn't exist yet before init().
        store.set_dim(mc.dim);
        store
    };
    let store = Arc::new(store);

    if !cli.quiet {
        println!("Indexing {} files (pipelined)...", files.len());
    }

    // Mark HNSW as dirty before writing chunks — if we crash between SQLite
    // commit and HNSW save, the dirty flag tells the next load to fall back
    // to brute-force search until a full rebuild. (RT-DATA-6)
    // DS-41: The dirty flag is a crash-safety invariant — if we can't set it,
    // abort rather than risk a stale index on crash.
    store
        .set_hnsw_dirty(true)
        .context("Failed to mark HNSW dirty before indexing")?;

    // Run the 3-stage pipeline: parse → embed → write
    // Pipeline shares the same Store via Arc (no duplicate DB connections)
    let stats = run_index_pipeline(
        &root,
        files.clone(),
        Arc::clone(&store),
        force,
        cli.quiet,
        cli.model_config().clone(),
    )?;
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

    if !cli.quiet && stats.total_calls > 0 {
        println!("  Call graph: {} calls", stats.total_calls);
    }
    if !cli.quiet && stats.total_type_edges > 0 {
        println!("  Type edges: {} edges", stats.total_type_edges);
    }

    // LLM summary pass (SQ-6): generate one-sentence summaries via Claude API
    // Runs BEFORE enrichment so summaries are incorporated into enrichment NL.
    #[cfg(feature = "llm-summaries")]
    if !check_interrupted() && llm_summaries {
        if !cli.quiet {
            println!("Generating LLM summaries...");
        }
        let config = cqs::config::Config::load(&root);
        let count = cqs::llm::llm_summary_pass(&store, cli.quiet, &config, Some(&cqs_dir))
            .context("LLM summary pass failed")?;
        if !cli.quiet && count > 0 {
            println!("  LLM summaries: {} new", count);
        }
    }

    // Doc comment generation pass: generate and write back doc comments
    #[cfg(feature = "llm-summaries")]
    if !check_interrupted() && improve_docs {
        if !cli.quiet {
            println!("Generating doc comments...");
        }
        let config = cqs::config::Config::load(&root);
        let doc_results = cqs::llm::doc_comment_pass(
            &store,
            &config,
            max_docs.unwrap_or(0),
            improve_all,
            Some(&cqs_dir),
        )
        .context("Doc comment generation failed")?;

        if !doc_results.is_empty() {
            // Group by file and write back
            use std::collections::HashMap;
            let mut by_file: HashMap<std::path::PathBuf, Vec<_>> = HashMap::new();
            for r in doc_results {
                by_file.entry(r.file.clone()).or_default().push(r);
            }
            let doc_parser = CqParser::new()?;
            let mut total = 0;
            for (path, edits) in &by_file {
                match cqs::doc_writer::rewriter::rewrite_file(path, edits, &doc_parser) {
                    Ok(n) => total += n,
                    Err(e) => tracing::warn!(
                        file = %path.display(),
                        error = %e,
                        "Doc write-back failed"
                    ),
                }
            }
            if !cli.quiet {
                println!(
                    "  Doc comments: {} functions across {} files",
                    total,
                    by_file.len()
                );
            }
        } else if !cli.quiet {
            println!("  Doc comments: 0 candidates");
        }
    }

    // HyDE query prediction pass: generate hypothetical queries for functions
    #[cfg(feature = "llm-summaries")]
    if !check_interrupted() && hyde_queries {
        if !cli.quiet {
            println!("Generating hyde query predictions...");
        }
        let config = cqs::config::Config::load(&root);
        let count = cqs::llm::hyde_query_pass(
            &store,
            cli.quiet,
            &config,
            max_hyde.unwrap_or(0),
            Some(&cqs_dir),
        )
        .context("Hyde query prediction pass failed")?;
        if !cli.quiet && count > 0 {
            println!("  Hyde predictions: {} new", count);
        }
    }

    // Call-graph enrichment pass (SQ-4): re-embed chunks with caller/callee context
    if !check_interrupted() && stats.total_calls > 0 {
        use crate::cli::enrichment_pass;

        if !cli.quiet {
            println!("Enriching embeddings with call graph context...");
        }
        let embedder = Embedder::new(cli.model_config().clone())
            .context("Failed to create embedder for enrichment pass")?;
        match enrichment_pass(&store, &embedder, cli.quiet) {
            Ok(count) => {
                if !cli.quiet && count > 0 {
                    println!("  Enriched: {} chunks", count);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Enrichment pass failed, continuing without");
                if !cli.quiet {
                    eprintln!("  Warning: enrichment pass failed: {:?}", e);
                }
            }
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
            // HNSW saved successfully — clear dirty flag (RT-DATA-6)
            if let Err(e) = store.set_hnsw_dirty(false) {
                tracing::warn!(error = %e, "Failed to clear HNSW dirty flag after HNSW save");
            }
            if !cli.quiet {
                println!("  HNSW index: {} vectors", total);
            }
        }
    }

    // Clean up backup from --force (rebuild succeeded)
    if backup_path.exists() {
        let _ = std::fs::remove_file(&backup_path);
    }

    Ok(())
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
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to check notes reindex status, forcing reindex");
                Some(0)
            })
            .is_some();

    if !needs_reindex {
        return Ok((0, true));
    }

    match parse_notes(&notes_path) {
        Ok(notes) => {
            if notes.is_empty() {
                return Ok((0, false));
            }

            let count = cqs::index_notes(&notes, &notes_path, store)?;
            Ok((count, false))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse notes");
            eprintln!("Warning: notes.toml parse error — notes not indexed: {}", e);
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
    Ok(build_hnsw_index_owned(store, cqs_dir)?.map(|h| h.len()))
}

/// Build HNSW index and return the Owned index for continued incremental use.
///
/// Builds from all chunk embeddings in the store, saves to disk, and returns
/// the `HnswIndex` (Owned variant). Used by watch mode to keep a mutable index
/// in memory for `insert_batch` calls on subsequent file changes.
pub(crate) fn build_hnsw_index_owned(store: &Store, cqs_dir: &Path) -> Result<Option<HnswIndex>> {
    let chunk_count = store.chunk_count().context("Failed to read chunk count")? as usize;
    let _span = tracing::info_span!("build_hnsw_index_owned", chunk_count).entered();

    if chunk_count == 0 {
        return Ok(None);
    }

    const HNSW_BATCH_SIZE: usize = 10_000;

    let chunk_batches = store.embedding_batches(HNSW_BATCH_SIZE);

    let hnsw = HnswIndex::build_batched_with_dim(chunk_batches, chunk_count, store.dim())?;
    hnsw.save(cqs_dir, "index")?;

    Ok(Some(hnsw))
}
