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
use std::time::Duration;

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
    let cqs_dir = dunce::canonicalize(&cqs_dir).unwrap_or(cqs_dir);
    let notes_path = dunce::canonicalize(&notes_path).unwrap_or(notes_path);

    // Lazy-initialized embedder (~500MB, avoids startup delay unless changes occur).
    // Once initialized, stays in memory for fast reindexing. See module docs for memory details.
    let embedder: OnceCell<Embedder> = OnceCell::new();

    // Open store once and reuse across all reindex operations.
    // Store uses connection pooling internally, so this is efficient.
    let store = Store::open(&index_path)?;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                for path in event.paths {
                    let path = dunce::canonicalize(&path).unwrap_or(path);
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
                    // Reindex code files if any changed
                    if !pending_files.is_empty() {
                        let files: Vec<PathBuf> = pending_files.drain().collect();
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
                        match reindex_files(&root, &store, &files, &parser, emb, cli.quiet) {
                            Ok(count) => {
                                if !cli.quiet {
                                    println!("Indexed {} chunk(s)", count);
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

/// Reindex specific files
fn reindex_files(
    root: &Path,
    store: &Store,
    files: &[PathBuf],
    parser: &CqParser,
    embedder: &Embedder,
    quiet: bool,
) -> Result<usize> {
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
        return Ok(0);
    }

    // Generate embeddings with neutral sentiment for code chunks
    let texts: Vec<String> = chunks.iter().map(generate_nl_description).collect();
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let embeddings: Vec<Embedding> = embedder
        .embed_documents(&text_refs)?
        .into_iter()
        .map(|e| e.with_sentiment(0.0))
        .collect();

    // Delete old chunks for these files and insert new ones
    for rel_path in files {
        store.delete_by_origin(rel_path)?;
    }

    let mut mtime_cache: HashMap<&std::path::Path, Option<i64>> = HashMap::new();
    for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
        let mtime = *mtime_cache.entry(chunk.file.as_path()).or_insert_with(|| {
            let abs_path = root.join(&chunk.file);
            abs_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
        });
        store.upsert_chunk(chunk, embedding, mtime)?;
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

    Ok(chunks.len())
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
