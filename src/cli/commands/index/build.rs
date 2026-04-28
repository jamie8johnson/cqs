//! Index command for cqs
//!
//! Indexes codebase files for semantic search.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use std::sync::Arc;

use cqs::{parse_notes, Embedder, HnswIndex, HnswKind, ModelInfo, Parser as CqParser, Store};

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
    let umap_flag = args.umap;

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
    let apply_docs = args.apply;
    #[cfg(not(feature = "llm-summaries"))]
    let apply_docs = false;
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
    #[cfg(feature = "llm-summaries")]
    if apply_docs && !improve_docs {
        anyhow::bail!("--apply requires --improve-docs");
    }

    let root = find_project_root();
    let project_cqs_dir = cqs::resolve_index_dir(&root);

    // Ensure project `.cqs/` exists before slot resolution / migration so
    // the slot helpers find a real directory to inspect.
    if !project_cqs_dir.exists() {
        std::fs::create_dir_all(&project_cqs_dir)
            .with_context(|| format!("Failed to create {}", project_cqs_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&project_cqs_dir, std::fs::Permissions::from_mode(0o700))
            {
                tracing::debug!(path = %project_cqs_dir.display(), error = %e, "Failed to set file permissions");
            }
        }
    }

    // Idempotent migration: if a legacy `.cqs/index.db` exists and slots/
    // hasn't been seeded yet, move the legacy index into `slots/default/`.
    if let Err(e) = cqs::slot::migrate_legacy_index_to_default_slot(&project_cqs_dir) {
        tracing::warn!(error = %e, "slot migration failed during index; continuing");
    }

    // Resolve slot. `cqs index` accepts the global `--slot` flag, falls back
    // to `CQS_SLOT` / `.cqs/active_slot` / "default" per spec. If the
    // operator runs `cqs index --slot foo` against a non-existent slot dir,
    // create it now (analogous to `cqs slot create foo` followed by index).
    let resolved_slot = cqs::slot::resolve_slot_name(cli.slot.as_deref(), &project_cqs_dir)
        .map_err(anyhow::Error::from)?;
    let cqs_dir = if cqs::slot::slots_root(&project_cqs_dir).exists() {
        cqs::resolve_slot_dir(&project_cqs_dir, &resolved_slot.name)
    } else {
        // Pre-slots layout: `.cqs/index.db` directly. We allowed migration
        // above to fail silently — the only way to reach here with neither
        // slots/ nor a legacy index is a fresh project. Materialize the
        // slot dir so subsequent runs are slot-aware.
        let dir = cqs::resolve_slot_dir(&project_cqs_dir, &resolved_slot.name);
        if !dir.exists() {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create slot dir {}", dir.display()))?;
        }
        dir
    };
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

    // Ensure the slot dir itself exists with restrictive permissions when
    // we materialize it as part of `cqs index --slot <new>` flows. The
    // project_cqs_dir was already chmodded above; the slot dir is a fresh
    // child whose permissions also matter for SEC-D.4 alignment.
    if !cqs_dir.exists() {
        std::fs::create_dir_all(&cqs_dir)
            .with_context(|| format!("Failed to create slot dir {}", cqs_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&cqs_dir, std::fs::Permissions::from_mode(0o700))
            {
                tracing::debug!(path = %cqs_dir.display(), error = %e, "Failed to set slot dir permissions");
            }
        }
    }

    // Span carries slot name + resolution source so failed-index logs
    // surface which slot was being touched without reading code.
    let _slot_span = tracing::info_span!(
        "index_slot",
        slot_name = %resolved_slot.name,
        slot_source = resolved_slot.source.as_str(),
    )
    .entered();

    // Detect a running cqs-watch --serve daemon BEFORE we touch anything.
    // The daemon holds a shared file lock on `index.hnsw.lock` for the
    // lifetime of its in-memory HNSW. A subsequent `cqs index --force` then
    // blocks indefinitely in `locks_lock_inode_wait` waiting for an
    // exclusive write lock the daemon will never release. On WSL/NTFS the
    // "advisory-only" warning fires but the wait still happens. Fail-fast
    // here with clear instructions instead of hanging for 60+ minutes.
    //
    // We use a connect-only probe (not the typed `daemon_ping`) so a daemon
    // running an older `PingResponse` schema still gets detected — schema
    // drift would otherwise let the deserialize error fall through and
    // silently restore the old hang behavior on version mismatches.
    //
    // Daemon socket is hashed from the project-level `.cqs/` (one daemon
    // per project, regardless of slot) so we use `project_cqs_dir` here.
    #[cfg(unix)]
    if !dry_run {
        let sock_path = cqs::daemon_translate::daemon_socket_path(&project_cqs_dir);
        if sock_path.exists() {
            use std::os::unix::net::UnixStream;
            use std::time::Duration;
            match UnixStream::connect(&sock_path) {
                Ok(stream) => {
                    // Connected — daemon is alive enough to hold the HNSW
                    // lock. Drop the stream immediately and bail.
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                    drop(stream);
                    anyhow::bail!(
                        "A cqs-watch --serve daemon is currently running ({}). It holds a shared lock on \
                         the HNSW index, so this reindex would block indefinitely in locks_lock_inode_wait. \
                         Stop the daemon before reindexing:\n\n  \
                         systemctl --user stop cqs-watch && cqs index{} && systemctl --user start cqs-watch\n\n\
                         (If you launched the daemon manually, kill that process instead.)",
                        sock_path.display(),
                        if force { " --force" } else { "" }
                    );
                }
                Err(e) => {
                    // Socket file exists but connect failed → stale socket
                    // (kill -9, OOM, power loss left the file behind). Safe
                    // to proceed; the next daemon start will replace it.
                    tracing::debug!(
                        path = %sock_path.display(),
                        error = %e,
                        "stale daemon socket present; proceeding with reindex"
                    );
                }
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

    // P2.12: capture wall-clock start so the optional JSON envelope can
    // report `took_ms`. Honors both global `--json` and the local one.
    let want_json = cli.json || args.json;
    let json_start = std::time::Instant::now();

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
        // Read LLM summaries from existing DB before destroying it.
        // Summaries are keyed by content_hash (blake3 of source content) so they're
        // valid for any chunk with identical source, even after reindex.
        let saved_summaries = if index_path.exists() {
            match Store::open(&index_path) {
                Ok(old_store) => {
                    let summaries = match old_store.get_all_summaries_full() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to read LLM summaries");
                            Vec::new()
                        }
                    };
                    if !summaries.is_empty() {
                        tracing::info!(
                            count = summaries.len(),
                            "Read LLM summaries from existing DB"
                        );
                    }
                    drop(old_store); // Close before rename
                    summaries
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to read summaries from existing DB");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

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
        let mc = cli.try_model_config()?;
        store.init(&ModelInfo::new(&mc.repo, mc.dim))?;
        store.set_dim(mc.dim);

        // Restore saved summaries into the fresh DB
        if !saved_summaries.is_empty() {
            match store.upsert_summaries_batch(&saved_summaries) {
                Ok(n) => tracing::info!(count = n, "Restored LLM summaries"),
                Err(e) => tracing::warn!(error = %e, "Failed to restore LLM summaries"),
            }
        }
        store
    };
    let store = Arc::new(store);

    if !cli.quiet {
        println!("Indexing {} files (pipelined)...", files.len());
    }

    // Mark both HNSW kinds as dirty before writing chunks — if we crash
    // between SQLite commit and HNSW save, the dirty flag tells the next
    // load to fall back to brute-force search until a full rebuild. Base
    // and enriched are built from the same chunks, so both must be marked.
    // (RT-DATA-6) DS-41: the dirty flag is a crash-safety invariant — if we
    // can't set it, abort rather than risk a stale index on crash.
    store
        .set_hnsw_dirty(HnswKind::Enriched, true)
        .context("Failed to mark enriched HNSW dirty before indexing")?;
    store
        .set_hnsw_dirty(HnswKind::Base, true)
        .context("Failed to mark base HNSW dirty before indexing")?;

    // Run the 3-stage pipeline: parse → embed → write
    // Pipeline shares the same Store via Arc (no duplicate DB connections)
    let stats = run_index_pipeline(
        &root,
        files.clone(),
        Arc::clone(&store),
        force,
        cli.quiet,
        cli.try_model_config()?.clone(),
    )?;
    let total_embedded = stats.total_embedded;
    let total_cached = stats.total_cached;
    let gpu_failures = stats.gpu_failures;

    // Prune missing files
    let existing_files: HashSet<_> = files.into_iter().collect();
    let pruned = store
        .prune_missing(&existing_files, &root)
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
            // Group by file and write back (or stage as review patches)
            use std::collections::HashMap;
            let mut by_file: HashMap<std::path::PathBuf, Vec<_>> = HashMap::new();
            for r in doc_results {
                by_file.entry(r.file.clone()).or_default().push(r);
            }
            let doc_parser = CqParser::new()?;
            if apply_docs {
                if !cli.quiet {
                    println!("  Writing generated doc comments directly to source files (--apply)");
                }
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
            } else {
                let patch_dir = cqs_dir.join("proposed-docs");
                let mut written = 0usize;
                let mut skipped = 0usize;
                for (path, edits) in &by_file {
                    match cqs::doc_writer::rewriter::write_proposed_patch(
                        path,
                        &root,
                        edits,
                        &doc_parser,
                        &patch_dir,
                    ) {
                        Ok(true) => written += 1,
                        Ok(false) => skipped += 1,
                        Err(e) => tracing::warn!(
                            file = %path.display(),
                            error = %e,
                            "Doc patch write failed"
                        ),
                    }
                }
                if !cli.quiet {
                    if written > 0 {
                        println!(
                            "  Doc comments: {} proposed update(s) written to {}",
                            written,
                            patch_dir.display()
                        );
                        println!(
                            "    Review and apply with: git apply {}/**/*.patch",
                            patch_dir.display()
                        );
                        println!("    Or rerun with --apply to write directly (skips review).");
                    } else if skipped > 0 {
                        println!(
                            "  Doc comments: {} candidate file(s) produced no diff",
                            skipped
                        );
                    }
                }
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

    // #1126 / P2.60: belt-and-braces final flush of the summary queue
    // before the index lock is dropped. Each LLM pass also flushes on
    // its own way out, but residue from a signal-interrupted pass would
    // otherwise have to wait for the next `cqs index` run. Idempotent
    // on an empty queue; cheap.
    #[cfg(feature = "llm-summaries")]
    if let Err(e) = store.flush_pending_summaries() {
        tracing::warn!(error = %e, "cmd_index: final flush of summary queue failed; rows retained for next run");
    }

    // Call-graph enrichment pass (SQ-4): re-embed chunks with caller/callee context
    if !check_interrupted() && stats.total_calls > 0 {
        use crate::cli::enrichment_pass;

        if !cli.quiet {
            println!("Enriching embeddings with call graph context...");
        }
        let embedder = Embedder::new(cli.try_model_config()?.clone())
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

    // Index notes if notes.toml exists.
    //
    // #1168: first-encounter prompt — if this is the project's first index
    // (no `.accepted-shared-notes` marker yet) AND a committed notes file
    // exists, confirm with the user before proceeding. Bypassable with
    // `--accept-shared-notes` for CI / scripted use; non-TTY stdin
    // auto-skips the index-notes step (loud warn, never hangs).
    let proceed_with_notes =
        first_encounter_notes_gate(&root, &project_cqs_dir, args.accept_shared_notes, cli.quiet)?;
    if !check_interrupted() && proceed_with_notes {
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

    // SPLADE sparse encoding (if model available).
    //
    // Path resolution is delegated to cqs::splade::resolve_splade_model_dir
    // so the env var (CQS_SPLADE_MODEL) and vocab-mismatch probe stay
    // consistent with the search-time encoder loaders. Critical for index
    // correctness: if the index pass and search pass use different SPLADE
    // models, the sparse vectors are token-incompatible and search-time
    // queries return garbage. Single source of truth.
    if !check_interrupted() {
        if let Some(splade_dir) = cqs::splade::resolve_splade_model_dir() {
            if !cli.quiet {
                println!("Encoding SPLADE sparse vectors...");
            }
            match cqs::splade::SpladeEncoder::new(
                &splade_dir,
                cqs::splade::SpladeEncoder::default_threshold(),
            ) {
                Ok(encoder) => {
                    let _span = tracing::info_span!("splade_index_encode").entered();
                    // CQ-4: Only encode chunks that don't already have sparse
                    // vectors. On --force the DB is fresh so all chunks are
                    // "missing"; on incremental runs this skips the ~95% of
                    // chunks that haven't changed.
                    let chunk_texts = store.chunk_splade_texts_missing()?;
                    let mut sparse_vecs: Vec<(String, cqs::splade::SparseVector)> = Vec::new();
                    let mut encoded = 0usize;
                    let mut failed = 0usize;

                    // PF-5: batch encode instead of per-chunk.
                    //
                    // CQS_SPLADE_BATCH overrides the initial batch size
                    // (default 64). Larger SPLADE models (SPLADE-Code 0.6B
                    // at 5.5x params) overflow GPU memory at 64. The inner
                    // loop is also adaptive: on OOM, halve and retry.
                    //
                    // CQS_SPLADE_RESET_EVERY (default 0 = disabled) triggers
                    // a session.clear() every N batches. This frees the ORT
                    // BFC arena which can accumulate cached allocations even
                    // with constant-shape inputs. Set to 32-64 if encoding
                    // a large corpus through a large model leaks memory.
                    let initial_batch: usize = std::env::var("CQS_SPLADE_BATCH")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .filter(|&n: &usize| n >= 1)
                        .unwrap_or(64);
                    let reset_every: usize = std::env::var("CQS_SPLADE_RESET_EVERY")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);

                    let total_chunks = chunk_texts.len();
                    let progress_step = (total_chunks / 20).max(1);
                    let mut next_progress_threshold = progress_step;

                    tracing::info!(
                        initial_batch,
                        reset_every,
                        total_chunks,
                        "SPLADE encoding starting"
                    );

                    let mut current_batch_size = initial_batch;
                    let mut idx = 0;
                    let mut batches_done = 0usize;
                    while idx < total_chunks {
                        let end = (idx + current_batch_size).min(total_chunks);
                        let batch = &chunk_texts[idx..end];
                        let ids: Vec<&str> = batch.iter().map(|(id, _)| id.as_str()).collect();
                        let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();

                        match encoder.encode_batch(&texts) {
                            Ok(svs) => {
                                for (id, sv) in ids.into_iter().zip(svs) {
                                    if !sv.is_empty() {
                                        sparse_vecs.push((id.to_string(), sv));
                                        encoded += 1;
                                    }
                                }
                                idx = end;
                                batches_done += 1;

                                // Periodic arena reset
                                if reset_every > 0 && batches_done.is_multiple_of(reset_every) {
                                    encoder.clear_session();
                                    tracing::debug!(batches_done, "SPLADE periodic session reset");
                                }

                                // Progress logging at ~5% milestones
                                if encoded >= next_progress_threshold {
                                    let pct = encoded * 100 / total_chunks.max(1);
                                    tracing::info!(
                                        encoded,
                                        total = total_chunks,
                                        pct,
                                        batch_size = current_batch_size,
                                        "SPLADE encoding progress"
                                    );
                                    if !cli.quiet {
                                        eprintln!(
                                            "  SPLADE: {}/{} ({}%) batch={}",
                                            encoded, total_chunks, pct, current_batch_size
                                        );
                                    }
                                    next_progress_threshold += progress_step;
                                }
                            }
                            Err(e) if current_batch_size > 1 => {
                                let new_size = (current_batch_size / 2).max(1);
                                tracing::warn!(
                                    old_batch = current_batch_size,
                                    new_batch = new_size,
                                    error = %e,
                                    "SPLADE batch failed (likely OOM) — halving batch size and retrying"
                                );
                                // Clear the session on OOM to free leaked memory
                                // before retrying at the smaller size.
                                encoder.clear_session();
                                current_batch_size = new_size;
                                // Don't advance idx — retry the same range.
                            }
                            Err(e) => {
                                // batch_size already at 1: this chunk truly
                                // can't be encoded. Skip it and move on.
                                tracing::warn!(
                                    chunk_id = ?ids[0],
                                    error = %e,
                                    "SPLADE encoding failed at batch_size=1, skipping chunk"
                                );
                                failed += 1;
                                idx += 1;
                            }
                        }
                    }
                    if !sparse_vecs.is_empty() {
                        store.upsert_sparse_vectors(&sparse_vecs)?;
                    }
                    if !cli.quiet {
                        println!(
                            "  SPLADE: {} chunks encoded (final batch={})",
                            encoded, current_batch_size
                        );
                        if failed > 0 {
                            println!("  SPLADE: {} chunks failed", failed);
                        }
                    }
                    // Persist the SpladeIndex to disk so query-time SPLADE
                    // doesn't have to rebuild it from SQLite on every CLI
                    // invocation. `sparse_vecs` already holds every chunk
                    // we just encoded, so building the in-memory index here
                    // costs only the HashMap insertion loop — no reload from
                    // SQLite. The first search after reindex then skips the
                    // ~45s load step.
                    //
                    // Failure is warned, not fatal — the query-time rebuild
                    // path still works; users just pay the rebuild cost on
                    // first query until the persist is rerun.
                    if !sparse_vecs.is_empty() {
                        match store.splade_generation() {
                            Ok(generation) => {
                                let splade_path =
                                    cqs_dir.join(cqs::splade::index::SPLADE_INDEX_FILENAME);
                                // CQ-4: Load ALL sparse vectors for the
                                // persist (not just the delta we encoded).
                                // On --force this equals sparse_vecs; on
                                // incremental it merges prior + new.
                                let all_vecs = match store.load_all_sparse_vectors() {
                                    Ok(v) => v,
                                    Err(e) => {
                                        // Don't fall back to delta-only: persisting
                                        // just the newly-encoded subset would silently
                                        // drop all previously-encoded chunks from the
                                        // on-disk index (correctness audit 2026-04-12).
                                        tracing::warn!(error = %e,
                                            "Failed to load sparse vectors for persist — \
                                             skipping. Next query will rebuild from SQLite.");
                                        Vec::new()
                                    }
                                };
                                if !all_vecs.is_empty() {
                                    let idx = cqs::splade::index::SpladeIndex::build(all_vecs);
                                    match idx.save(&splade_path, generation) {
                                        Ok(()) => {
                                            if !cli.quiet {
                                                println!(
                                                    "  SPLADE index: persisted ({} chunks, {} tokens)",
                                                    idx.len(),
                                                    idx.unique_tokens()
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                path = %splade_path.display(),
                                                "SPLADE index persist failed; query-time rebuild \
                                                 will still work"
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Failed to read splade_generation for eager persist — \
                                     skipping. Next SPLADE query will rebuild from SQLite."
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SPLADE encoder unavailable, skipping sparse encoding");
                }
            }
        }
    }

    // Optional UMAP projection — runs once per `cqs index --umap` invocation.
    // Lives between SPLADE and HNSW build because all final embeddings are
    // settled by this point and HNSW only depends on the dense vectors. The
    // projection itself is opt-in; failures are logged but never fatal so the
    // rest of the index build still succeeds.
    if umap_flag && !check_interrupted() {
        if !cli.quiet {
            println!("Running UMAP projection...");
        }
        match super::umap::run_umap_projection(&store, cli.quiet) {
            Ok(updated) => {
                if !cli.quiet && updated > 0 {
                    println!("  UMAP: {updated} chunks projected to 2D");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "UMAP projection failed — cluster view will be unavailable");
                if !cli.quiet {
                    eprintln!("  Warning: UMAP projection failed ({e}); cluster view skipped");
                }
            }
        }
    }

    // Build HNSW index for fast chunk search (notes use brute-force from SQLite)
    if !check_interrupted() {
        if !cli.quiet {
            println!("Building HNSW index...");
        }

        if let Some(total) = build_hnsw_index(&store, &cqs_dir)? {
            // HNSW saved successfully — clear enriched dirty flag (RT-DATA-6)
            if let Err(e) = store.set_hnsw_dirty(HnswKind::Enriched, false) {
                tracing::warn!(error = %e, "Failed to clear enriched HNSW dirty flag after HNSW save");
            }
            if !cli.quiet {
                println!("  HNSW index: {} vectors", total);
            }
        }

        // Phase 5: also build the base (non-enriched) HNSW index. Non-fatal
        // if it fails — fall back to enriched-only at query time.
        match build_hnsw_base_index(&store, &cqs_dir) {
            Ok(Some(total)) => {
                if let Err(e) = store.set_hnsw_dirty(HnswKind::Base, false) {
                    tracing::warn!(error = %e, "Failed to clear base HNSW dirty flag after base HNSW save");
                }
                if !cli.quiet {
                    println!("  HNSW base index: {} vectors", total);
                }
            }
            Ok(None) => {
                if !cli.quiet {
                    println!("  HNSW base index: skipped (no base embeddings yet)");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Base HNSW build failed, enriched index still usable");
                if !cli.quiet {
                    eprintln!("  Warning: base HNSW build failed ({e}); using enriched-only");
                }
            }
        }
    }

    // Clean up backup from --force (rebuild succeeded)
    if backup_path.exists() {
        let _ = std::fs::remove_file(&backup_path);
    }

    // P2.12: emit a structured summary envelope when --json is set.
    // Numbers come from the values already computed above so no extra DB
    // round trip is incurred. Progress prints stayed on the human path —
    // we don't gate them on `!want_json` since the index command's banner
    // is informational and many callers tee stdout/stderr; only the
    // *final* shape matters for the JSON contract.
    if want_json {
        let model_name = cli
            .try_model_config()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let chunk_count = store.chunk_count().unwrap_or(0);
        let obj = serde_json::json!({
            "indexed_files": existing_files.len(),
            "indexed_chunks": chunk_count,
            "took_ms": json_start.elapsed().as_millis() as u64,
            "model": model_name,
            "total_embedded": total_embedded,
            "total_cached": total_cached,
            "gpu_failures": gpu_failures,
            "pruned": pruned,
            "parse_errors": stats.parse_errors,
            "total_calls": stats.total_calls,
            "total_type_edges": stats.total_type_edges,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    }

    Ok(())
}

/// First-encounter shared-notes gate. (#1168)
///
/// Returns `true` if notes indexing should proceed, `false` to skip the
/// pass entirely (user declined OR non-TTY auto-skip).
///
/// Behaviour:
/// - No committed `docs/notes.toml` → return `true` (nothing to gate on).
/// - Marker file `.cqs/.accepted-shared-notes` already present → `true`.
/// - `accept_flag = true` (i.e. `--accept-shared-notes`) → write marker, `true`.
/// - Stdin not a TTY → loud warn + `false` (auto-skip; never hangs CI).
/// - TTY available → prompt; on Y or empty (default), write marker + `true`.
///   On any other reply → `false`.
fn first_encounter_notes_gate(
    root: &Path,
    project_cqs_dir: &Path,
    accept_flag: bool,
    quiet: bool,
) -> Result<bool> {
    use std::io::{IsTerminal, Write};

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(true);
    }
    let marker_path = project_cqs_dir.join(".accepted-shared-notes");
    if marker_path.exists() {
        return Ok(true);
    }

    // Count entries + sentiment buckets so the prompt is informative. A parse
    // failure here is non-fatal — fall through to gating with N=?, the
    // downstream `index_notes_from_file` will surface the parse error.
    let (total, positive, negative) = match cqs::parse_notes(&notes_path) {
        Ok(notes) => {
            let total = notes.len();
            let positive = notes.iter().filter(|n| n.sentiment > 0.0).count();
            let negative = notes.iter().filter(|n| n.sentiment < 0.0).count();
            (Some(total), Some(positive), Some(negative))
        }
        Err(_) => (None, None, None),
    };

    // Empty notes file → no payload to gate. Don't write the marker either;
    // the next index run with non-empty notes will prompt as expected.
    if matches!(total, Some(0)) {
        return Ok(true);
    }

    let count_label = total
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_string());
    let pos_label = positive
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_string());
    let neg_label = negative
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".to_string());

    if accept_flag {
        write_notes_acceptance_marker(&marker_path)?;
        if !quiet {
            println!(
                "Accepted committed shared notes ({} entries) — marker written to {}",
                count_label,
                marker_path.display()
            );
        }
        return Ok(true);
    }

    // Non-TTY: never hang. Auto-skip with a warning so CI / scripted runs
    // don't accidentally pull in untrusted shared notes; the user can
    // re-run with `--accept-shared-notes` once they've reviewed them.
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "Warning: docs/notes.toml exists ({} entries) but stdin is not a TTY. \
             Skipping notes indexing on this run. Pass --accept-shared-notes to opt in \
             on subsequent runs.",
            count_label
        );
        return Ok(false);
    }

    // Interactive prompt.
    eprintln!();
    eprintln!(
        "Detected committed notes at {}: {} entries ({} positive, {} negative).",
        notes_path.display(),
        count_label,
        pos_label,
        neg_label
    );
    eprintln!(
        "These will affect search rankings and be shown to AI agents using cqs against this repo."
    );
    eprint!(
        "Index them? [Y/n] (or run with --accept-shared-notes to skip this prompt next time): "
    );
    std::io::stderr().flush().ok();

    let mut reply = String::new();
    std::io::stdin()
        .read_line(&mut reply)
        .context("Failed to read response from stdin")?;
    let trimmed = reply.trim();
    let accepted = trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("y")
        || trimmed.eq_ignore_ascii_case("yes");
    if accepted {
        write_notes_acceptance_marker(&marker_path)?;
        Ok(true)
    } else {
        eprintln!("Skipping notes indexing on this run.");
        Ok(false)
    }
}

/// Write the `.accepted-shared-notes` marker with a UTC timestamp body.
fn write_notes_acceptance_marker(marker_path: &Path) -> Result<()> {
    if let Some(parent) = marker_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    std::fs::write(marker_path, format!("accepted_at = {}\n", now))
        .with_context(|| format!("Failed to write {}", marker_path.display()))?;
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

/// HNSW insert batch size.
/// Configurable via `CQS_HNSW_BATCH_SIZE` (default 10000).
fn hnsw_batch_size() -> usize {
    std::env::var("CQS_HNSW_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(10_000)
}

/// Build HNSW index from store embeddings
///
/// Creates an HNSW index containing chunk embeddings only.
///
/// Notes are excluded from HNSW — they use brute-force search from SQLite
/// so that notes are immediately searchable without rebuild.
pub(crate) fn build_hnsw_index(store: &Store, cqs_dir: &Path) -> Result<Option<usize>> {
    Ok(build_hnsw_index_owned(store, cqs_dir)?.map(|(h, _)| h.len()))
}

/// Build HNSW index and return the Owned index plus a per-id `content_hash`
/// snapshot for continued incremental use.
///
/// Builds from all chunk embeddings in the store, saves to disk, and returns
/// `(HnswIndex, snapshot_hashes)` where `snapshot_hashes[id]` is the
/// `content_hash` the embedding was generated from. Used by watch mode to
/// keep a mutable index in memory for `insert_batch` calls and (on the
/// background-rebuild path, #1124) to detect entries that were re-embedded
/// mid-rebuild — the swap-time drain replays them with the fresh vector
/// instead of dedup'ing them by id-only and silently dropping the update.
///
/// Both the embeddings and the hashes come from a single SQL pass via
/// [`Store::embedding_and_hash_batches`] so they're consistent under
/// concurrent writers (WAL snapshot isolation only holds within a
/// transaction).
pub(crate) fn build_hnsw_index_owned<M>(
    store: &cqs::store::Store<M>,
    cqs_dir: &Path,
) -> Result<Option<(HnswIndex, std::collections::HashMap<String, String>)>> {
    let chunk_count = store.chunk_count().context("Failed to read chunk count")? as usize;
    let _span = tracing::info_span!("build_hnsw_index_owned", chunk_count).entered();

    if chunk_count == 0 {
        return Ok(None);
    }

    let batch_size = hnsw_batch_size();

    // Tee the (id, embedding, hash) stream: HNSW build consumes
    // (id, embedding) pairs while we accumulate (id, hash) into a side map.
    // Single SQL pass — no second query, no risk of the hash drifting from
    // the embedding under concurrent writers.
    let mut snapshot_hashes: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(chunk_count);
    let chunk_batches = store.embedding_and_hash_batches(batch_size).map(|batch| {
        batch.map(|triples| {
            let mut pairs = Vec::with_capacity(triples.len());
            for (id, emb, hash) in triples {
                snapshot_hashes.insert(id.clone(), hash);
                pairs.push((id, emb));
            }
            pairs
        })
    });

    let hnsw = HnswIndex::build_batched_with_dim(chunk_batches, chunk_count, store.dim())?;
    hnsw.save(cqs_dir, "index")?;

    Ok(Some((hnsw, snapshot_hashes)))
}

/// Build the Phase 5 base HNSW index from `embedding_base` and save as
/// `index_base.hnsw.{graph,data,ids}`.
///
/// The base index contains the raw-NL embedding for each chunk (no LLM summary,
/// no call-graph enrichment). It's queried by the router when classification
/// picks a [`SearchStrategy::DenseBase`] — typically conceptual, behavioral,
/// and negation queries, where enrichment hurts signal.
///
/// Returns `Ok(None)` when the column is entirely NULL (e.g. just after the
/// v17→v18 migration before the next index pass has populated it). In that
/// case the router silently falls back to the enriched index.
pub(crate) fn build_hnsw_base_index<M>(
    store: &cqs::store::Store<M>,
    cqs_dir: &Path,
) -> Result<Option<usize>> {
    let _span = tracing::info_span!("build_hnsw_base_index").entered();

    // If the column hasn't been populated yet (e.g. fresh v17→v18 migration
    // before the next index pass), skip the build so we don't write an empty
    // HNSW file that misleads readers into thinking dual indexing is active.
    let base_count = store
        .base_embedding_count()
        .context("Failed to count rows with embedding_base")? as usize;

    if base_count == 0 {
        tracing::info!("No embedding_base rows yet — skipping base HNSW build");
        return Ok(None);
    }

    let batch_size = hnsw_batch_size();

    let chunk_batches = store.embedding_base_batches(batch_size);
    let hnsw = HnswIndex::build_batched_with_dim(chunk_batches, base_count, store.dim())?;
    hnsw.save(cqs_dir, "index_base")?;

    tracing::info!(base_count, "Base HNSW index built");
    Ok(Some(hnsw.len()))
}

// The data-flow for the dual HNSW build is covered by
// `store::chunks::async_helpers::tests::test_embedding_base_batches_*` in
// the library crate — those tests exercise populate-on-insert and
// NULL-row skipping, which are the two branches that matter here.
// The HNSW builder itself is covered by `hnsw::build` unit tests.

// P2.86: TC-HAP — direct happy-path tests for the two HNSW build helpers
// invoked by `cmd_index` and the watch loop. Lower-level unit tests cover
// `embedding_batches` / `embedding_base_batches` and the HNSW builder, but
// the join — store + cqs_dir → on-disk + in-memory index — had no direct
// pin. These tests close that gap with a minimal `dim=16` corpus so the
// HNSW build runs in milliseconds.
#[cfg(test)]
mod tests {
    use super::*;
    use cqs::embedder::Embedding;
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::ModelInfo;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_chunk(id: &str, dim: usize, seed: f32) -> (Chunk, Embedding) {
        let content = format!("fn {}() {{ }}", id);
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = Chunk {
            id: id.to_string(),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: id.to_string(),
            signature: format!("fn {}()", id),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        // Distinct unit-ish vectors per chunk so the HNSW builder has
        // something non-degenerate to insert.
        let mut emb = vec![0.0_f32; dim];
        emb[0] = seed;
        emb[1] = 1.0 - seed;
        (chunk, Embedding::new(emb))
    }

    /// Open a fresh store at the given dim and seed `n` chunks. Returns
    /// the temp dir (kept alive for test lifetime) and `cqs_dir` path.
    fn seed_store(n: usize, dim: usize) -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut store = Store::open(&index_path).expect("open store");
        store
            .init(&ModelInfo::new("test/model", dim))
            .expect("init store");
        // `Store::open` sees no metadata on a fresh DB and defaults `dim`
        // to EMBEDDING_DIM (1024). After `init` writes the real dim, sync
        // the cached value so the upsert dim-check below sees `dim` and
        // not the stale fallback.
        store.set_dim(dim);
        let pairs: Vec<_> = (0..n)
            .map(|i| make_chunk(&format!("c{i}"), dim, (i as f32 + 1.0) * 0.1))
            .collect();
        if !pairs.is_empty() {
            store.upsert_chunks_batch(&pairs, Some(0)).expect("upsert");
        }
        drop(store);
        (dir, cqs_dir)
    }

    #[test]
    fn build_hnsw_index_owned_returns_index_with_chunk_count() {
        let dim = 16;
        let n = 5;
        let (_tmp, cqs_dir) = seed_store(n, dim);
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let store = Store::open_readonly_after_init(&index_path, |_| Ok(())).expect("ro store");

        let (idx, snapshot_hashes) = build_hnsw_index_owned(&store, &cqs_dir)
            .expect("build_hnsw_index_owned must succeed")
            .expect("non-empty store must produce an index");
        assert_eq!(idx.len(), n, "index len must equal seeded chunk count");
        // P1.17 (#1124): snapshot must carry one (id, content_hash) entry
        // per built vector so the rebuild-window drain can detect stale
        // entries.
        assert_eq!(
            snapshot_hashes.len(),
            n,
            "snapshot_hashes must contain one entry per chunk"
        );
        for id in idx.ids() {
            assert!(
                snapshot_hashes.contains_key(id),
                "snapshot_hashes missing id {id}"
            );
        }
        // The hnsw save side-effect should land at `<cqs_dir>/index.hnsw.*`.
        // We can't easily round-trip-load without exposing private save
        // path constants, so just verify the in-memory state is correct.
    }

    #[test]
    fn build_hnsw_index_owned_returns_none_for_empty_store() {
        let dim = 16;
        let (_tmp, cqs_dir) = seed_store(0, dim);
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let store = Store::open_readonly_after_init(&index_path, |_| Ok(())).expect("ro store");
        let result = build_hnsw_index_owned(&store, &cqs_dir).expect("must not error");
        assert!(
            result.is_none(),
            "empty store must yield None (no on-disk index, no in-memory index)"
        );
    }

    #[test]
    fn build_hnsw_base_index_returns_some_when_base_populated() {
        // Phase 5 (v18): `upsert_chunks_batch` seeds embedding_base with the
        // same bytes as the enriched embedding on insert (see
        // `store::chunks::async_helpers`). So a freshly-seeded store has
        // base_embedding_count == n, and the base HNSW build is not
        // skipped. Pin that contract — a regression that stopped seeding
        // embedding_base on insert would silently break the dual-index
        // router.
        let dim = 16;
        let n = 4;
        let (_tmp, cqs_dir) = seed_store(n, dim);
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let store = Store::open_readonly_after_init(&index_path, |_| Ok(())).expect("ro store");
        let result = build_hnsw_base_index(&store, &cqs_dir).expect("must not error");
        assert_eq!(
            result,
            Some(n),
            "base index must contain all {n} seeded chunks"
        );
    }

    #[test]
    fn build_hnsw_base_index_returns_none_for_empty_store() {
        let dim = 16;
        let (_tmp, cqs_dir) = seed_store(0, dim);
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let store = Store::open_readonly_after_init(&index_path, |_| Ok(())).expect("ro store");
        let result = build_hnsw_base_index(&store, &cqs_dir).expect("must not error");
        assert!(
            result.is_none(),
            "empty store must yield None for base HNSW build"
        );
    }

    // ===== #1168: first_encounter_notes_gate =====

    #[test]
    fn first_encounter_gate_proceeds_when_no_notes_file() {
        let tmp = TempDir::new().unwrap();
        let cqs_dir = tmp.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let proceed = first_encounter_notes_gate(tmp.path(), &cqs_dir, false, true).unwrap();
        assert!(proceed, "no notes file → proceed");
    }

    #[test]
    fn first_encounter_gate_proceeds_when_marker_present() {
        let tmp = TempDir::new().unwrap();
        let cqs_dir = tmp.path().join(".cqs");
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        std::fs::write(
            tmp.path().join("docs/notes.toml"),
            r#"
[[note]]
text = "Watch out for X"
sentiment = -0.5
mentions = []
"#,
        )
        .unwrap();
        std::fs::create_dir_all(&cqs_dir).unwrap();
        std::fs::write(cqs_dir.join(".accepted-shared-notes"), "x").unwrap();
        let proceed = first_encounter_notes_gate(tmp.path(), &cqs_dir, false, true).unwrap();
        assert!(proceed, "marker present → proceed");
    }

    #[test]
    fn first_encounter_gate_accept_flag_writes_marker() {
        let tmp = TempDir::new().unwrap();
        let cqs_dir = tmp.path().join(".cqs");
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        std::fs::write(
            tmp.path().join("docs/notes.toml"),
            r#"
[[note]]
text = "Suspicious instruction"
sentiment = 0.0
mentions = []
"#,
        )
        .unwrap();
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let proceed = first_encounter_notes_gate(tmp.path(), &cqs_dir, true, true).unwrap();
        assert!(proceed, "--accept-shared-notes → proceed");
        let marker = cqs_dir.join(".accepted-shared-notes");
        assert!(marker.exists(), "marker must be written when accept_flag set");
        let body = std::fs::read_to_string(&marker).unwrap();
        assert!(
            body.contains("accepted_at"),
            "marker body should contain timestamp marker, got: {body}"
        );
    }

    #[test]
    fn first_encounter_gate_proceeds_when_notes_file_empty() {
        let tmp = TempDir::new().unwrap();
        let cqs_dir = tmp.path().join(".cqs");
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        std::fs::write(tmp.path().join("docs/notes.toml"), "").unwrap();
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let proceed = first_encounter_notes_gate(tmp.path(), &cqs_dir, false, true).unwrap();
        assert!(proceed, "empty notes → proceed (nothing to gate on)");
        // No marker written for the zero-entry case — the next non-empty
        // index run should still prompt.
        assert!(
            !cqs_dir.join(".accepted-shared-notes").exists(),
            "no marker should be written for empty notes file"
        );
    }
}
