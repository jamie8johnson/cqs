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

use super::{check_interrupted, find_project_root, Cli};

/// Maximum pending files to prevent unbounded memory growth
const MAX_PENDING_FILES: usize = 10_000;

pub fn cmd_watch(cli: &Cli, debounce_ms: u64, no_ignore: bool) -> Result<()> {
    if no_ignore {
        tracing::warn!("--no-ignore is not yet implemented for watch mode");
    }

    let root = find_project_root();
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
                for path in event.paths {
                    let path = dunce::canonicalize(&path).unwrap_or_else(|e| {
                        tracing::debug!(path = %path.display(), error = %e, "canonicalize failed, using original");
                        path
                    });
                    // Skip .cqs directory
                    if path.starts_with(&cqs_dir) {
                        continue;
                    }

                    // Check if it's notes.toml
                    if path == notes_path {
                        pending_notes = true;
                        last_event = std::time::Instant::now();
                        continue;
                    }

                    // Skip if not a supported extension
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if !supported_ext.contains(ext) {
                        continue;
                    }

                    // Convert to relative path
                    if let Ok(rel) = path.strip_prefix(&root) {
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
                        last_event = std::time::Instant::now();
                    }
                }
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Watch error");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Check if we should process pending changes
                let should_process = (!pending_files.is_empty() || pending_notes)
                    && last_event.elapsed() >= debounce;

                if should_process {
                    cycles_since_clear = 0;

                    // Reindex code files if any changed
                    if !pending_files.is_empty() {
                        let files: Vec<PathBuf> = pending_files.drain().collect();
                        pending_files.shrink_to(64);
                        if !cli.quiet {
                            println!("\n{} file(s) changed, reindexing...", files.len());
                            for f in &files {
                                println!("  {}", f.display());
                            }
                        }

                        // Initialize embedder on first use (lazy ~500ms init)
                        let emb = match embedder.get() {
                            Some(e) => e,
                            None => match Embedder::new() {
                                Ok(e) => embedder.get_or_init(|| e),
                                Err(e) => {
                                    warn!(error = %e, "Failed to initialize embedder");
                                    continue;
                                }
                            },
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

                        match reindex_files(&root, &store, &files, &parser, emb, cli.quiet) {
                            Ok((count, _content_hashes)) => {
                                // Record mtimes to skip duplicate events
                                for (file, mtime) in pre_mtimes {
                                    last_indexed_mtime.insert(file, mtime);
                                }
                                // Prune entries for deleted files to prevent unbounded growth
                                last_indexed_mtime.retain(|f, _| root.join(f).exists());
                                if !cli.quiet {
                                    println!("Indexed {} chunk(s)", count);
                                }
                                // Rebuild HNSW so index is fresh
                                match super::commands::build_hnsw_index(&store, &cqs_dir) {
                                    Ok(Some(n)) => {
                                        info!(vectors = n, "HNSW index rebuilt");
                                        if !cli.quiet {
                                            println!("  HNSW index: {} vectors", n);
                                        }
                                    }
                                    Ok(None) => {} // empty store
                                    Err(e) => {
                                        warn!(error = %e, "HNSW rebuild failed (search falls back to brute-force)");
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Reindex error");
                            }
                        }
                    }

                    // Reindex notes if notes.toml changed
                    if pending_notes {
                        pending_notes = false;
                        if !cli.quiet {
                            println!("\nNotes changed, reindexing...");
                        }
                        let emb = match embedder.get() {
                            Some(e) => e,
                            None => match Embedder::new() {
                                Ok(e) => embedder.get_or_init(|| e),
                                Err(e) => {
                                    warn!(error = %e, "Failed to initialize embedder");
                                    continue;
                                }
                            },
                        };
                        match reindex_notes(&root, &store, emb, cli.quiet) {
                            Ok(count) => {
                                if !cli.quiet {
                                    println!("Indexed {} note(s)", count);
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Notes reindex error");
                            }
                        }
                    }
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

    if chunks.is_empty() {
        return Ok((0, Vec::new()));
    }

    // Collect content hashes before chunks are consumed (for incremental HNSW)
    let content_hashes: Vec<String> = chunks.iter().map(|c| c.content_hash.clone()).collect();

    // Check content hash cache to skip re-embedding unchanged chunks
    let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
    let existing = store.get_embeddings_by_hashes(&hashes);

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

    // Group chunks by file and atomically replace (delete + insert in single transaction)
    // Uses into_iter() to move ownership instead of cloning each chunk/embedding.
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
        store.replace_file_chunks(file, pairs, mtime)?;
    }

    // Extract call graph for changed files
    for rel_path in files {
        let abs_path = root.join(rel_path);
        if !abs_path.exists() {
            continue;
        }
        match parser.parse_file_calls(&abs_path) {
            Ok(function_calls) => {
                if let Err(e) = store.upsert_function_calls(rel_path, &function_calls) {
                    tracing::warn!(file = %rel_path.display(), error = %e, "Failed to update call graph");
                }
            }
            Err(e) => {
                tracing::warn!(file = %abs_path.display(), error = %e, "Failed to extract calls");
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
