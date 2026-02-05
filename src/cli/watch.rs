//! Watch mode - monitor for file changes and reindex

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Result};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{info, info_span, warn};

use cqs::embedder::{Embedder, Embedding};
use cqs::nl::generate_nl_description;
use cqs::note::parse_notes;
use cqs::parser::Parser as CqParser;
use cqs::store::Store;

use super::{check_interrupted, find_project_root, Cli};

/// Maximum pending files to prevent unbounded memory growth
const MAX_PENDING_FILES: usize = 10_000;

pub fn cmd_watch(cli: &Cli, debounce_ms: u64, _no_ignore: bool) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

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

    // Lazy-initialized embedder (avoids 500ms startup delay unless changes occur)
    let embedder: OnceCell<Embedder> = OnceCell::new();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                for path in event.paths {
                    // Skip .cq directory
                    if path.starts_with(&cq_dir) {
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
                        match reindex_files(&root, &index_path, &files, &parser, emb, cli.quiet) {
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
                        match reindex_notes(&root, &index_path, emb, cli.quiet) {
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
    index_path: &Path,
    files: &[PathBuf],
    parser: &CqParser,
    embedder: &Embedder,
    quiet: bool,
) -> Result<usize> {
    let _span = info_span!("reindex_files", file_count = files.len()).entered();
    info!(file_count = files.len(), "Reindexing files");

    let store = Store::open(index_path)?;

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

    for (chunk, embedding) in chunks.iter().zip(embeddings.iter()) {
        let abs_path = root.join(&chunk.file);
        let mtime = abs_path
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        store.upsert_chunk(chunk, embedding, mtime)?;
    }

    if !quiet {
        println!("Updated {} file(s)", files.len());
    }

    Ok(chunks.len())
}

/// Reindex notes from docs/notes.toml
fn reindex_notes(
    root: &Path,
    index_path: &Path,
    embedder: &Embedder,
    quiet: bool,
) -> Result<usize> {
    let _span = info_span!("reindex_notes").entered();

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(0);
    }

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        return Ok(0);
    }

    let store = Store::open(index_path)?;
    let count = cqs::index_notes(&notes, &notes_path, embedder, &store)?;

    if !quiet {
        let (total, warnings, patterns) = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            total, warnings, patterns
        );
    }

    Ok(count)
}
