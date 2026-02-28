//! Watch mode - monitor for file changes and reindex
//!
//! ## Memory Usage
//!
//! Watch mode holds several resources in memory while idle:
//!
//! - **Parser**: ~1MB for tree-sitter queries (allocated immediately)
//! - **Store**: SQLite connection pool with up to 4 connections (allocated immediately)
//! - **Embedder**: ~500MB for ONNX model (lazy-loaded on first file change)
//!
//! The Embedder is the largest resource and is only loaded when files actually change.
//! Once loaded, it remains in memory for fast subsequent reindexing. This tradeoff
//! favors responsiveness over memory efficiency for long-running watch sessions.
//!
//! For memory-constrained environments, consider running `cqs index` manually instead
//! of using watch mode.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Result};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{info, info_span, warn};

use cqs::embedder::{Embedder, Embedding};
use cqs::generate_nl_description;
use cqs::note::parse_notes;
use cqs::parser::Parser as CqParser;
use cqs::store::Store;

use super::{check_interrupted, find_project_root, try_acquire_index_lock, Cli};

/// Maximum pending files to prevent unbounded memory growth
const MAX_PENDING_FILES: usize = 10_000;

/// Try to initialize the embedder, returning a reference from the OnceCell.
/// Deduplicates the 7-line pattern that appeared twice in cmd_watch.
fn try_init_embedder(embedder: &OnceCell<Embedder>) -> Option<&Embedder> {
    match embedder.get() {
        Some(e) => Some(e),
        None => match Embedder::new() {
            Ok(e) => Some(embedder.get_or_init(|| e)),
            Err(e) => {
                warn!(error = %e, "Failed to initialize embedder");
                None
            }
        },
    }
}

pub fn cmd_watch(cli: &Cli, debounce_ms: u64, no_ignore: bool) -> Result<()> {
    if no_ignore {
        tracing::warn!("--no-ignore is not yet implemented for watch mode");
    }

    let root = find_project_root();

    if cqs::config::is_wsl() {
        tracing::warn!("WSL detected: inotify may be unreliable on Windows filesystem mounts. Consider running 'cqs index' periodically.");
    }

    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("No index found. Run 'cqs index' first.");
    }

    let parser = CqParser::new()?;
    let supported_ext: HashSet<_> = parser.supported_extensions().iter().cloned().collect();

    println!(
        "Watching {} for changes (Ctrl+C to stop)...",
        root.display()
    );
    println!(
        "Code extensions: {}",
        supported_ext.iter().cloned().collect::<Vec<_>>().join(", ")
    );
    println!("Also watching: docs/notes.toml");

    let (tx, rx) = mpsc::channel();

    let config = Config::default().with_poll_interval(Duration::from_millis(debounce_ms));

    let mut watcher = RecommendedWatcher::new(tx, config)?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    // Track pending changes for debouncing
    let mut pending_files: HashSet<PathBuf> = HashSet::new();
    let mut pending_notes = false;
    let mut last_event = std::time::Instant::now();
    let debounce = Duration::from_millis(debounce_ms);
    let notes_path = root.join("docs/notes.toml");
    let cqs_dir = dunce::canonicalize(&cqs_dir).unwrap_or_else(|e| {
        tracing::debug!(path = %cqs_dir.display(), error = %e, "canonicalize failed, using original");
        cqs_dir
    });
    let notes_path = dunce::canonicalize(&notes_path).unwrap_or_else(|e| {
        tracing::debug!(path = %notes_path.display(), error = %e, "canonicalize failed, using original");
        notes_path
    });

    // Lazy-initialized embedder (~500MB, avoids startup delay unless changes occur).
    // Once initialized, stays in memory for fast reindexing. See module docs for memory details.
    let embedder: OnceCell<Embedder> = OnceCell::new();

    // Open store once and reuse across all reindex operations.
    // Store uses connection pooling internally, so this is efficient.
    let store = Store::open(&index_path)?;

    // Track last-indexed mtime per file to skip duplicate WSL/NTFS events.
    // On WSL, inotify over 9P delivers repeated events for the same file change.
    let mut last_indexed_mtime: HashMap<PathBuf, SystemTime> = HashMap::new();

    let mut cycles_since_clear: u32 = 0;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                collect_events(
                    &event,
                    &root,
                    &cqs_dir,
                    &notes_path,
                    &supported_ext,
                    &mut pending_files,
                    &mut pending_notes,
                    &mut last_event,
                    &last_indexed_mtime,
                );
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Watch error");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let should_process = (!pending_files.is_empty() || pending_notes)
                    && last_event.elapsed() >= debounce;

                if should_process {
                    cycles_since_clear = 0;

                    // DS-1: Acquire index lock before reindexing. If another process
                    // (cqs index, cqs gc) holds it, skip this cycle.
                    let lock = match try_acquire_index_lock(&cqs_dir) {
                        Ok(Some(lock)) => lock,
                        Ok(None) => {
                            info!("Index lock held by another process, skipping reindex cycle");
                            continue;
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to create index lock file");
                            continue;
                        }
                    };

                    if !pending_files.is_empty() {
                        process_file_changes(
                            &root,
                            &cqs_dir,
                            &store,
                            &parser,
                            &embedder,
                            &mut pending_files,
                            &mut last_indexed_mtime,
                            cli.quiet,
                        );
                    }

                    if pending_notes {
                        pending_notes = false;
                        process_note_changes(&root, &store, &embedder, cli.quiet);
                    }

                    // DS-1: Release lock after all reindex work (including HNSW rebuild)
                    drop(lock);
                } else {
                    cycles_since_clear += 1;
                    // Clear embedder session after ~5 minutes idle (3000 cycles at 100ms)
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = embedder.get() {
                            emb.clear_session();
                        }
                        cycles_since_clear = 0;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!(
                    "File watcher disconnected unexpectedly. \
                     Hint: Restart 'cqs watch' to resume monitoring."
                );
            }
        }

        if check_interrupted() {
            println!("\nStopping watch...");
            break;
        }
    }

    Ok(())
}

/// Collect file system events into pending sets, filtering by extension and deduplicating.
#[allow(clippy::too_many_arguments)]
fn collect_events(
    event: &notify::Event,
    root: &Path,
    cqs_dir: &Path,
    notes_path: &Path,
    supported_ext: &HashSet<&str>,
    pending_files: &mut HashSet<PathBuf>,
    pending_notes: &mut bool,
    last_event: &mut std::time::Instant,
    last_indexed_mtime: &HashMap<PathBuf, SystemTime>,
) {
    for path in &event.paths {
        let path = dunce::canonicalize(path).unwrap_or_else(|e| {
            tracing::debug!(path = %path.display(), error = %e, "canonicalize failed, using original");
            path.clone()
        });
        // Skip .cqs directory
        if path.starts_with(cqs_dir) {
            continue;
        }

        // Check if it's notes.toml
        if path == notes_path {
            *pending_notes = true;
            *last_event = std::time::Instant::now();
            continue;
        }

        // Skip if not a supported extension
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !supported_ext.contains(ext) {
            continue;
        }

        // Convert to relative path
        if let Ok(rel) = path.strip_prefix(root) {
            // Skip if mtime unchanged since last index (dedup WSL/NTFS events)
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                if last_indexed_mtime
                    .get(rel)
                    .is_some_and(|last| mtime <= *last)
                {
                    continue;
                }
            }
            if pending_files.len() < MAX_PENDING_FILES {
                pending_files.insert(rel.to_path_buf());
            }
            *last_event = std::time::Instant::now();
        }
    }
}

/// Process pending file changes: parse, embed, and store atomically, then rebuild HNSW.
#[allow(clippy::too_many_arguments)]
fn process_file_changes(
    root: &Path,
    cqs_dir: &Path,
    store: &Store,
    parser: &CqParser,
    embedder: &OnceCell<Embedder>,
    pending_files: &mut HashSet<PathBuf>,
    last_indexed_mtime: &mut HashMap<PathBuf, SystemTime>,
    quiet: bool,
) {
    let files: Vec<PathBuf> = pending_files.drain().collect();
    pending_files.shrink_to(64);
    if !quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }

    let emb = match try_init_embedder(embedder) {
        Some(e) => e,
        None => return,
    };

    // Capture mtimes BEFORE reindexing to avoid race condition
    let pre_mtimes: HashMap<PathBuf, SystemTime> = files
        .iter()
        .filter_map(|f| {
            std::fs::metadata(root.join(f))
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (f.clone(), t))
        })
        .collect();

    // Note: concurrent searches during this window may see partial
    // results (RT-DATA-3). Per-file transactions are atomic but the
    // batch is not — files indexed so far are visible, remaining are
    // stale. Self-heals after HNSW rebuild. Acceptable for a dev tool.
    match reindex_files(root, store, &files, parser, emb, quiet) {
        Ok((count, _content_hashes)) => {
            // Record mtimes to skip duplicate events
            for (file, mtime) in pre_mtimes {
                last_indexed_mtime.insert(file, mtime);
            }
            // Prune entries for deleted files to prevent unbounded growth
            last_indexed_mtime.retain(|f, _| root.join(f).exists());
            if !quiet {
                println!("Indexed {} chunk(s)", count);
            }
            // Rebuild HNSW so index is fresh
            match super::commands::build_hnsw_index(store, cqs_dir) {
                Ok(Some(n)) => {
                    info!(vectors = n, "HNSW index rebuilt");
                    if !quiet {
                        println!("  HNSW index: {} vectors", n);
                    }
                }
                Ok(None) => {} // empty store
                Err(e) => {
                    warn!(error = %e, "HNSW rebuild failed, removing stale HNSW files (search falls back to brute-force)");
                    // Delete stale HNSW files so search doesn't use an outdated index
                    for ext in cqs::hnsw::HNSW_ALL_EXTENSIONS {
                        let path = cqs_dir.join(format!("index.{}", ext));
                        if path.exists() {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Reindex error");
        }
    }
}

/// Process notes.toml changes: parse and re-embed notes.
fn process_note_changes(root: &Path, store: &Store, embedder: &OnceCell<Embedder>, quiet: bool) {
    if !quiet {
        println!("\nNotes changed, reindexing...");
    }
    let emb = match try_init_embedder(embedder) {
        Some(e) => e,
        None => return,
    };
    match reindex_notes(root, store, emb, quiet) {
        Ok(count) => {
            if !quiet {
                println!("Indexed {} note(s)", count);
            }
        }
        Err(e) => {
            warn!(error = %e, "Notes reindex error");
        }
    }
}

/// Reindex specific files.
///
/// Returns `(chunk_count, content_hashes)` — the content hashes can be used for
/// incremental HNSW insertion (looking up embeddings by hash instead of
/// rebuilding the full index).
fn reindex_files(
    root: &Path,
    store: &Store,
    files: &[PathBuf],
    parser: &CqParser,
    embedder: &Embedder,
    quiet: bool,
) -> Result<(usize, Vec<String>)> {
    let _span = info_span!("reindex_files", file_count = files.len()).entered();
    info!(file_count = files.len(), "Reindexing files");

    // Parse the changed files
    let chunks: Vec<_> = files
        .iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            if !abs_path.exists() {
                // File was deleted, we'll handle this by removing old chunks
                return vec![];
            }
            match parser.parse_file(&abs_path) {
                Ok(mut file_chunks) => {
                    // Rewrite paths to be relative
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                    }
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", abs_path.display(), e);
                    vec![]
                }
            }
        })
        .collect();

    // Apply windowing to split long chunks into overlapping windows
    let chunks = crate::cli::pipeline::apply_windowing(chunks, embedder);

    if chunks.is_empty() {
        return Ok((0, Vec::new()));
    }

    // Collect content hashes before chunks are consumed (for incremental HNSW)
    let content_hashes: Vec<String> = chunks.iter().map(|c| c.content_hash.clone()).collect();

    // Check content hash cache to skip re-embedding unchanged chunks
    let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
    let existing = store.get_embeddings_by_hashes(&hashes)?;

    let mut cached: Vec<(usize, Embedding)> = Vec::new();
    let mut to_embed: Vec<(usize, &cqs::Chunk)> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if let Some(emb) = existing.get(&chunk.content_hash) {
            cached.push((i, emb.clone()));
        } else {
            to_embed.push((i, chunk));
        }
    }

    // Only embed chunks that don't have cached embeddings
    let new_embeddings: Vec<Embedding> = if to_embed.is_empty() {
        vec![]
    } else {
        let texts: Vec<String> = to_embed
            .iter()
            .map(|(_, c)| generate_nl_description(c))
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        embedder
            .embed_documents(&text_refs)?
            .into_iter()
            .map(|e| e.with_sentiment(0.0))
            .collect()
    };

    // Merge cached and new embeddings in original chunk order
    let chunk_count = chunks.len();
    let mut embeddings: Vec<Embedding> = vec![Embedding::new(vec![]); chunk_count];
    for (i, emb) in cached {
        embeddings[i] = emb;
    }
    for ((i, _), emb) in to_embed.into_iter().zip(new_embeddings) {
        embeddings[i] = emb;
    }

    // DS-2: Extract call graph from chunks (same loop), then use atomic upsert.
    // This mirrors the pipeline's approach: extract_calls_from_chunk per chunk,
    // then upsert_chunks_and_calls in a single transaction per file.
    let all_calls: Vec<(String, cqs::parser::CallSite)> = chunks
        .iter()
        .flat_map(|chunk| {
            let calls = parser.extract_calls_from_chunk(chunk);
            calls
                .into_iter()
                .map(|c| (chunk.id.clone(), c))
                .collect::<Vec<_>>()
        })
        .collect();

    // Group chunks by file and atomically upsert chunks + calls in a single transaction
    let mut mtime_cache: HashMap<PathBuf, Option<i64>> = HashMap::new();
    let mut by_file: HashMap<PathBuf, Vec<(cqs::Chunk, Embedding)>> = HashMap::new();
    for (chunk, embedding) in chunks.into_iter().zip(embeddings.into_iter()) {
        let file_key = chunk.file.clone();
        by_file
            .entry(file_key)
            .or_default()
            .push((chunk, embedding));
    }
    for (file, pairs) in &by_file {
        let mtime = *mtime_cache.entry(file.clone()).or_insert_with(|| {
            let abs_path = root.join(file);
            abs_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        });
        // Filter calls to only those belonging to chunks in this file
        let chunk_ids: HashSet<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
        let file_calls: Vec<_> = all_calls
            .iter()
            .filter(|(id, _)| chunk_ids.contains(id.as_str()))
            .cloned()
            .collect();
        store.upsert_chunks_and_calls(pairs, mtime, &file_calls)?;
    }

    // Extract type edges for changed files (separate from chunk+call atomicity,
    // since type edges are not part of the core data consistency concern)
    for rel_path in files {
        let abs_path = root.join(rel_path);
        if !abs_path.exists() {
            continue;
        }
        match parser.parse_file_relationships(&abs_path) {
            Ok((_function_calls, chunk_type_refs)) => {
                if !chunk_type_refs.is_empty() {
                    if let Err(e) = store.upsert_type_edges_for_file(rel_path, &chunk_type_refs) {
                        tracing::warn!(file = %rel_path.display(), error = %e, "Failed to update type edges");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(file = %abs_path.display(), error = %e, "Failed to extract type edges");
            }
        }
    }

    if let Err(e) = store.touch_updated_at() {
        tracing::warn!(error = %e, "Failed to update timestamp");
    }

    if !quiet {
        println!("Updated {} file(s)", files.len());
    }

    Ok((chunk_count, content_hashes))
}

/// Reindex notes from docs/notes.toml
fn reindex_notes(root: &Path, store: &Store, embedder: &Embedder, quiet: bool) -> Result<usize> {
    let _span = info_span!("reindex_notes").entered();

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(0);
    }

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        return Ok(0);
    }

    let count = cqs::index_notes(&notes, &notes_path, embedder, store)?;

    if !quiet {
        let ns = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            ns.total, ns.warnings, ns.patterns
        );
    }

    Ok(count)
}
