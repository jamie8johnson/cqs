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

use anyhow::{bail, Context, Result};
use notify::{Config, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{info, info_span, warn};

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::generate_nl_description;
use cqs::hnsw::HnswIndex;
use cqs::note::parse_notes;
use cqs::parser::{ChunkTypeRefs, Parser as CqParser};
use cqs::store::Store;

use super::{check_interrupted, find_project_root, try_acquire_index_lock, Cli};

/// Full HNSW rebuild after this many incremental inserts to clean orphaned vectors.
const HNSW_REBUILD_THRESHOLD: usize = 100;

/// Maximum pending files to prevent unbounded memory growth
const MAX_PENDING_FILES: usize = 10_000;

/// Track exponential backoff state for embedder initialization retries.
///
/// On repeated failures, backs off from 0s to max 5 minutes between attempts
/// to avoid burning CPU retrying a broken ONNX model load every ~2s cycle.
struct EmbedderBackoff {
    /// Number of consecutive failures
    failures: u32,
    /// Instant when the next retry is allowed
    next_retry: std::time::Instant,
}

impl EmbedderBackoff {
    fn new() -> Self {
        Self {
            failures: 0,
            next_retry: std::time::Instant::now(),
        }
    }

    /// Record a failure and compute the next retry time with exponential backoff.
    /// Backoff: 2^failures seconds, capped at 300s (5 min).
    fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let delay_secs = 2u64.saturating_pow(self.failures).min(300);
        self.next_retry = std::time::Instant::now() + Duration::from_secs(delay_secs);
        warn!(
            failures = self.failures,
            next_retry_secs = delay_secs,
            "Embedder init failed, backing off"
        );
    }

    /// Reset backoff on success.
    fn reset(&mut self) {
        self.failures = 0;
    }

    /// Whether we should attempt initialization (backoff expired).
    fn should_retry(&self) -> bool {
        std::time::Instant::now() >= self.next_retry
    }
}

/// Try to initialize the embedder, returning a reference from the OnceCell.
/// Deduplicates the 7-line pattern that appeared twice in cmd_watch.
/// Uses `backoff` to apply exponential backoff on repeated failures (RM-24).
fn try_init_embedder<'a>(
    embedder: &'a OnceCell<Embedder>,
    backoff: &mut EmbedderBackoff,
    model_config: &ModelConfig,
) -> Option<&'a Embedder> {
    match embedder.get() {
        Some(e) => Some(e),
        None => {
            if !backoff.should_retry() {
                return None;
            }
            match Embedder::new(model_config.clone()) {
                Ok(e) => {
                    backoff.reset();
                    Some(embedder.get_or_init(|| e))
                }
                Err(e) => {
                    warn!(error = %e, "Failed to initialize embedder");
                    backoff.record_failure();
                    None
                }
            }
        }
    }
}

/// Watches the project for file changes and updates the code search index incrementally.
///
/// # Arguments
///
/// * `cli` - Command-line interface context
/// * `debounce_ms` - Debounce interval in milliseconds for file change events
/// * `no_ignore` - If true, ignores `.gitignore` rules (not yet implemented)
/// * `poll` - If true, uses polling instead of inotify for file system monitoring
///
/// # Returns
///
/// Returns `Ok(())` on successful completion, or an error if the index doesn't exist or watch setup fails.
///
/// # Errors
///
/// * If the project index is not found (user should run `cqs index` first)
/// * If setting up file system watching fails
pub fn cmd_watch(cli: &Cli, debounce_ms: u64, no_ignore: bool, poll: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_watch", debounce_ms, poll).entered();
    if no_ignore {
        tracing::warn!("--no-ignore is not yet implemented for watch mode");
    }

    let root = find_project_root();

    // Auto-detect when polling is needed: WSL + /mnt/ path.
    //
    // Detection is prefix-based (/mnt/) rather than filesystem-based (statfs NTFS/FAT magic)
    // because that's pragmatic: paths under /mnt/ in WSL are DrvFs mounts of Windows
    // filesystems (NTFS, FAT32, exFAT), none of which support inotify. A statfs check would
    // give the same answer with more syscalls and less portability across WSL versions.
    // If the project root is on a Linux filesystem inside WSL (e.g. /home/...), inotify works
    // fine and we leave use_poll false.
    // PB-21: Also detect //wsl.localhost/ and //wsl$/ UNC paths
    let use_poll = poll
        || (cqs::config::is_wsl()
            && root
                .to_str()
                .is_some_and(|p| p.starts_with("/mnt/") || p.starts_with("//wsl")));

    if cqs::config::is_wsl() && !use_poll {
        tracing::warn!("WSL detected: inotify may be unreliable on Windows filesystem mounts. Use --poll or 'cqs index' periodically.");
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

    // Box<dyn Watcher> so both watcher types work with the same variable
    let mut watcher: Box<dyn Watcher> = if use_poll {
        println!("Using poll watcher (interval: {}ms)", debounce_ms);
        Box::new(PollWatcher::new(tx, config)?)
    } else {
        Box::new(RecommendedWatcher::new(tx, config)?)
    };
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
    let mut embedder_backoff = EmbedderBackoff::new();

    // Open store and reuse across reindex operations within a cycle.
    // Re-opened after each reindex cycle to clear stale OnceLock caches (DS-9).
    let mut store = Store::open(&index_path)
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    // Track last-indexed mtime per file to skip duplicate WSL/NTFS events.
    // On WSL, inotify over 9P delivers repeated events for the same file change.
    // Bounded: pruned when >10k entries or >1k entries on single-file reindex.
    let mut last_indexed_mtime: HashMap<PathBuf, SystemTime> = HashMap::with_capacity(1024);

    let mut cycles_since_clear: u32 = 0;

    // Persistent HNSW state for incremental updates.
    // On first file change, does a full build and keeps the Owned index in memory.
    // Subsequent changes insert only changed chunks via insert_batch.
    // Full rebuild every HNSW_REBUILD_THRESHOLD incremental inserts to clean orphans.
    let mut hnsw_index: Option<HnswIndex> = None;
    let mut incremental_count: usize = 0;

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
                            &mut embedder_backoff,
                            &mut pending_files,
                            &mut last_indexed_mtime,
                            &mut hnsw_index,
                            &mut incremental_count,
                            cli.quiet,
                            cli.model_config(),
                        );
                    }

                    if pending_notes {
                        pending_notes = false;
                        process_note_changes(&root, &store, cli.quiet);
                    }

                    // DS-9: Re-open Store to clear stale OnceLock caches
                    // (call_graph_cache, test_chunks_cache). The documented contract
                    // in store/mod.rs requires re-opening after index changes.
                    drop(store);
                    store = Store::open(&index_path).with_context(|| {
                        format!("Failed to re-open store at {}", index_path.display())
                    })?;

                    // DS-1: Release lock after all reindex work (including HNSW rebuild)
                    drop(lock);
                } else {
                    cycles_since_clear += 1;
                    // Clear embedder session and HNSW index after ~5 minutes idle
                    // (3000 cycles at 100ms). Frees GPU/memory when watch is idle.
                    if cycles_since_clear >= 3000 {
                        if let Some(emb) = embedder.get() {
                            emb.clear_session();
                        }
                        hnsw_index = None;
                        incremental_count = 0;
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
        // PB-26: Skip canonicalize for deleted files — dunce::canonicalize
        // requires the file to exist (calls std::fs::canonicalize internally).
        let path = if path.exists() {
            dunce::canonicalize(path).unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };
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
        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();
        if !supported_ext.contains(ext.as_str()) {
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

/// Process pending file changes: parse, embed, store atomically, then update HNSW.
///
/// Uses incremental HNSW insertion when an Owned index is available in memory.
/// Falls back to full rebuild on first run or after `HNSW_REBUILD_THRESHOLD` incremental inserts.
#[allow(clippy::too_many_arguments)]
fn process_file_changes(
    root: &Path,
    cqs_dir: &Path,
    store: &Store,
    parser: &CqParser,
    embedder: &OnceCell<Embedder>,
    embedder_backoff: &mut EmbedderBackoff,
    pending_files: &mut HashSet<PathBuf>,
    last_indexed_mtime: &mut HashMap<PathBuf, SystemTime>,
    hnsw_index: &mut Option<HnswIndex>,
    incremental_count: &mut usize,
    quiet: bool,
    model_config: &ModelConfig,
) {
    let files: Vec<PathBuf> = pending_files.drain().collect();
    let _span = info_span!("process_file_changes", file_count = files.len()).entered();
    pending_files.shrink_to(64);
    if !quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }

    let emb = match try_init_embedder(embedder, embedder_backoff, model_config) {
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
    //
    // Mark HNSW dirty before writing chunks (RT-DATA-6).
    if let Err(e) = store.set_hnsw_dirty(true) {
        tracing::warn!(error = %e, "Cannot set HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
    match reindex_files(root, store, &files, parser, emb, quiet) {
        Ok((count, content_hashes)) => {
            // Record mtimes to skip duplicate events
            for (file, mtime) in pre_mtimes {
                last_indexed_mtime.insert(file, mtime);
            }
            // RM-17: Prune entries for deleted files when >1,000 entries regardless
            // of batch size. The files.len() == 1 guard was overly conservative.
            if last_indexed_mtime.len() > 10_000 {
                last_indexed_mtime.retain(|f, _| root.join(f).exists());
            }
            if !quiet {
                println!("Indexed {} chunk(s)", count);
            }

            // Incremental HNSW update: insert changed chunks into existing Owned index.
            // Falls back to full rebuild on first run or after HNSW_REBUILD_THRESHOLD inserts.
            let needs_full_rebuild =
                hnsw_index.is_none() || *incremental_count >= HNSW_REBUILD_THRESHOLD;

            // During full rebuild the old index and new batch coexist briefly,
            // but `build_batched` streams one batch at a time so peak memory is
            // old_index + one_batch, not 2× the full index.
            if needs_full_rebuild {
                match super::commands::build_hnsw_index_owned(store, cqs_dir) {
                    Ok(Some(index)) => {
                        let n = index.len();
                        *hnsw_index = Some(index);
                        *incremental_count = 0;
                        if let Err(e) = store.set_hnsw_dirty(false) {
                            tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                        }
                        info!(vectors = n, "HNSW index rebuilt (full)");
                        if !quiet {
                            println!("  HNSW index: {} vectors (full rebuild)", n);
                        }
                    }
                    Ok(None) => {
                        *hnsw_index = None;
                    }
                    Err(e) => {
                        warn!(error = %e, "HNSW rebuild failed, removing stale HNSW files (search falls back to brute-force)");
                        *hnsw_index = None;
                        for ext in cqs::hnsw::HNSW_ALL_EXTENSIONS {
                            let path = cqs_dir.join(format!("index.{}", ext));
                            if path.exists() {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                    }
                }
            } else if !content_hashes.is_empty() {
                // Incremental path: insert only newly-embedded chunks.
                // Modified chunks get new IDs, so old vectors become orphans in
                // the HNSW graph (hnsw_rs has no deletion). Orphans are harmless:
                // search post-filters against live SQLite chunk IDs. They're
                // cleaned on the next full rebuild (every HNSW_REBUILD_THRESHOLD).
                let hash_refs: Vec<&str> = content_hashes.iter().map(|s| s.as_str()).collect();
                match store.get_chunk_ids_and_embeddings_by_hashes(&hash_refs) {
                    Ok(pairs) if !pairs.is_empty() => {
                        let items: Vec<(String, &[f32])> = pairs
                            .iter()
                            .map(|(id, emb)| (id.clone(), emb.as_slice()))
                            .collect();
                        if let Some(ref mut index) = hnsw_index {
                            match index.insert_batch(&items) {
                                Ok(n) => {
                                    *incremental_count += n;
                                    // Save updated index to disk for search processes
                                    if let Err(e) = index.save(cqs_dir, "index") {
                                        warn!(error = %e, "Failed to save HNSW after incremental insert");
                                    } else if let Err(e) = store.set_hnsw_dirty(false) {
                                        tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                                    }
                                    info!(
                                        inserted = n,
                                        total = index.len(),
                                        incremental_count = *incremental_count,
                                        "HNSW incremental insert"
                                    );
                                    if !quiet {
                                        println!(
                                            "  HNSW index: +{} vectors (incremental, {} total)",
                                            n,
                                            index.len()
                                        );
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "HNSW incremental insert failed, will rebuild next cycle");
                                    // Force full rebuild next cycle
                                    *hnsw_index = None;
                                }
                            }
                        }
                    }
                    Ok(_) => {} // no embeddings found for hashes
                    Err(e) => {
                        warn!(error = %e, "Failed to fetch embeddings for HNSW incremental insert");
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Reindex error");
        }
    }
}

/// Process notes.toml changes: parse and store notes (no embedding needed, SQ-9).
fn process_note_changes(root: &Path, store: &Store, quiet: bool) {
    if !quiet {
        println!("\nNotes changed, reindexing...");
    }
    match reindex_notes(root, store, quiet) {
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

    // Parse changed files once — extract chunks, calls, AND type refs in a single pass.
    // Avoids the previous double-read + double-parse per file.
    let mut all_type_refs: Vec<(PathBuf, Vec<ChunkTypeRefs>)> = Vec::new();
    let chunks: Vec<_> = files
        .iter()
        .flat_map(|rel_path| {
            let abs_path = root.join(rel_path);
            if !abs_path.exists() {
                // File was deleted, we'll handle this by removing old chunks
                return vec![];
            }
            match parser.parse_file_all(&abs_path) {
                Ok((mut file_chunks, _calls, chunk_type_refs)) => {
                    // Rewrite paths to be relative
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                    }
                    // Stash type refs for upsert after chunks are stored
                    if !chunk_type_refs.is_empty() {
                        all_type_refs.push((rel_path.clone(), chunk_type_refs));
                    }
                    file_chunks
                }
                Err(e) => {
                    tracing::warn!(path = %abs_path.display(), error = %e, "Failed to parse file");
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

    // Collect content hashes of NEWLY EMBEDDED chunks only (for incremental HNSW).
    // Unchanged chunks (cache hits) are already in the HNSW index from a prior cycle,
    // so re-inserting them would create duplicates (hnsw_rs has no dedup).
    let content_hashes: Vec<String> = to_embed
        .iter()
        .map(|(_, c)| c.content_hash.clone())
        .collect();

    // Only embed chunks that don't have cached embeddings
    let new_embeddings: Vec<Embedding> = if to_embed.is_empty() {
        vec![]
    } else {
        let texts: Vec<String> = to_embed
            .iter()
            .map(|(_, c)| generate_nl_description(c))
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        embedder.embed_documents(&text_refs)?.into_iter().collect()
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
                .map(|d| d.as_millis() as i64)
        });
        // `all_calls` is scoped to the changed files in this reindex batch, not the full
        // index, so this filter is O(changed_chunks) rather than O(all_chunks).
        // Filter calls to only those belonging to chunks in this file
        let chunk_ids: HashSet<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
        let file_calls: Vec<_> = all_calls
            .iter()
            .filter(|(id, _)| chunk_ids.contains(id.as_str()))
            .cloned()
            .collect();
        store.upsert_chunks_and_calls(pairs, mtime, &file_calls)?;
    }

    // Upsert type edges from the earlier parse_file_all() results.
    // Type edges are soft data — separate from chunk+call atomicity.
    // They depend on chunk IDs existing in the DB, which is why we upsert
    // them after chunks are stored above.
    for (rel_path, chunk_type_refs) in &all_type_refs {
        if let Err(e) = store.upsert_type_edges_for_file(rel_path, chunk_type_refs) {
            tracing::warn!(file = %rel_path.display(), error = %e, "Failed to update type edges");
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
fn reindex_notes(root: &Path, store: &Store, quiet: bool) -> Result<usize> {
    let _span = info_span!("reindex_notes").entered();

    let notes_path = root.join("docs/notes.toml");
    if !notes_path.exists() {
        return Ok(0);
    }

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        return Ok(0);
    }

    let count = cqs::index_notes(&notes, &notes_path, store)?;

    if !quiet {
        let ns = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            ns.total, ns.warnings, ns.patterns
        );
    }

    Ok(count)
}
