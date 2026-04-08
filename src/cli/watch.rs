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
/// Override with CQS_WATCH_REBUILD_THRESHOLD env var.
fn hnsw_rebuild_threshold() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_WATCH_REBUILD_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100)
    })
}

/// Maximum pending files to prevent unbounded memory growth.
/// Override with CQS_WATCH_MAX_PENDING env var.
fn max_pending_files() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_WATCH_MAX_PENDING")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000)
    })
}

/// Immutable references shared across the watch loop.
///
/// Does not include `Store` because it is re-opened each cycle (DS-9).
struct WatchConfig<'a> {
    root: &'a Path,
    cqs_dir: &'a Path,
    notes_path: &'a Path,
    supported_ext: &'a HashSet<&'a str>,
    parser: &'a CqParser,
    embedder: &'a OnceCell<Embedder>,
    quiet: bool,
    model_config: &'a ModelConfig,
}

/// Mutable session state that evolves across watch cycles.
struct WatchState {
    embedder_backoff: EmbedderBackoff,
    pending_files: HashSet<PathBuf>,
    pending_notes: bool,
    last_event: std::time::Instant,
    last_indexed_mtime: HashMap<PathBuf, SystemTime>,
    hnsw_index: Option<HnswIndex>,
    incremental_count: usize,
}

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
        self.next_retry = std::time::Instant::now();
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

    // Open store and reuse across reindex operations within a cycle.
    // Re-opened after each reindex cycle to clear stale OnceLock caches (DS-9).
    let mut store = Store::open(&index_path)
        .with_context(|| format!("Failed to open store at {}", index_path.display()))?;

    // Persistent HNSW state for incremental updates.
    // On first file change, does a full build and keeps the Owned index in memory.
    // Subsequent changes insert only changed chunks via insert_batch.
    // Full rebuild every hnsw_rebuild_threshold() incremental inserts to clean orphans.
    //
    // DS-35: Load existing HNSW index from disk if present, to avoid orphan accumulation
    // across restarts. Start incremental_count at threshold/2 so the first rebuild
    // happens sooner, cleaning any orphans from prior sessions.
    let (hnsw_index, incremental_count) =
        match HnswIndex::load_with_dim(cqs_dir.as_ref(), "index", store.dim()) {
            Ok(index) => {
                info!(vectors = index.len(), "Loaded existing HNSW index");
                (Some(index), hnsw_rebuild_threshold() / 2)
            }
            Err(_) => (None, 0),
        };

    let model_config = cli.try_model_config()?;
    let watch_cfg = WatchConfig {
        root: &root,
        cqs_dir: &cqs_dir,
        notes_path: &notes_path,
        supported_ext: &supported_ext,
        parser: &parser,
        embedder: &embedder,
        quiet: cli.quiet,
        model_config,
    };

    let mut state = WatchState {
        embedder_backoff: EmbedderBackoff::new(),
        pending_files: HashSet::new(),
        pending_notes: false,
        last_event: std::time::Instant::now(),
        // Track last-indexed mtime per file to skip duplicate WSL/NTFS events.
        // On WSL, inotify over 9P delivers repeated events for the same file change.
        // Bounded: pruned when >10k entries or >1k entries on single-file reindex.
        last_indexed_mtime: HashMap::with_capacity(1024),
        hnsw_index,
        incremental_count,
    };

    let mut cycles_since_clear: u32 = 0;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(event)) => {
                collect_events(&event, &watch_cfg, &mut state);
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Watch error");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let should_process = (!state.pending_files.is_empty() || state.pending_notes)
                    && state.last_event.elapsed() >= debounce;

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

                    if !state.pending_files.is_empty() {
                        process_file_changes(&watch_cfg, &store, &mut state);
                    }

                    if state.pending_notes {
                        state.pending_notes = false;
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
                        state.hnsw_index = None;
                        state.incremental_count = 0;
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
fn collect_events(event: &notify::Event, cfg: &WatchConfig, state: &mut WatchState) {
    for path in &event.paths {
        // PB-26: Skip canonicalize for deleted files — dunce::canonicalize
        // requires the file to exist (calls std::fs::canonicalize internally).
        let path = if path.exists() {
            dunce::canonicalize(path).unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };
        // Skip .cqs directory
        // PB-2: Deleted files can't be canonicalized (they don't exist), so
        // compare normalized string forms to handle slash differences on WSL.
        let norm_path = cqs::normalize_path(&path);
        let norm_cqs = cqs::normalize_path(cfg.cqs_dir);
        if norm_path.starts_with(&norm_cqs) {
            continue;
        }

        // Check if it's notes.toml
        let norm_notes = cqs::normalize_path(cfg.notes_path);
        if norm_path == norm_notes {
            state.pending_notes = true;
            state.last_event = std::time::Instant::now();
            continue;
        }

        // Skip if not a supported extension
        let ext_raw = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let ext = ext_raw.to_ascii_lowercase();
        if !cfg.supported_ext.contains(ext.as_str()) {
            continue;
        }

        // Convert to relative path
        if let Ok(rel) = path.strip_prefix(cfg.root) {
            // Skip if mtime unchanged since last index (dedup WSL/NTFS events)
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                if state
                    .last_indexed_mtime
                    .get(rel)
                    .is_some_and(|last| mtime <= *last)
                {
                    continue;
                }
            }
            if state.pending_files.len() < max_pending_files() {
                state.pending_files.insert(rel.to_path_buf());
            }
            state.last_event = std::time::Instant::now();
        }
    }
}

/// Process pending file changes: parse, embed, store atomically, then update HNSW.
///
/// Uses incremental HNSW insertion when an Owned index is available in memory.
/// Falls back to full rebuild on first run or after `hnsw_rebuild_threshold()` incremental inserts.
fn process_file_changes(cfg: &WatchConfig, store: &Store, state: &mut WatchState) {
    let files: Vec<PathBuf> = state.pending_files.drain().collect();
    let _span = info_span!("process_file_changes", file_count = files.len()).entered();
    state.pending_files.shrink_to(64);
    if !cfg.quiet {
        println!("\n{} file(s) changed, reindexing...", files.len());
        for f in &files {
            println!("  {}", f.display());
        }
    }

    let emb = match try_init_embedder(cfg.embedder, &mut state.embedder_backoff, cfg.model_config) {
        Some(e) => e,
        None => return,
    };

    // Capture mtimes BEFORE reindexing to avoid race condition
    let pre_mtimes: HashMap<PathBuf, SystemTime> = files
        .iter()
        .filter_map(|f| {
            std::fs::metadata(cfg.root.join(f))
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
    match reindex_files(cfg.root, store, &files, cfg.parser, emb, cfg.quiet) {
        Ok((count, content_hashes)) => {
            // Record mtimes to skip duplicate events
            for (file, mtime) in pre_mtimes {
                state.last_indexed_mtime.insert(file, mtime);
            }
            // RM-17: Prune entries for deleted files when mtime map grows large.
            // RM-4: Lowered from 10K to 5K — the map tracks every file we've ever
            // indexed in this session, so pruning earlier bounds memory without
            // affecting correctness (retain only keeps files that still exist).
            if state.last_indexed_mtime.len() > 5_000 {
                state
                    .last_indexed_mtime
                    .retain(|f, _| cfg.root.join(f).exists());
            }
            if !cfg.quiet {
                println!("Indexed {} chunk(s)", count);
            }

            // Incremental HNSW update: insert changed chunks into existing Owned index.
            // Falls back to full rebuild on first run or after hnsw_rebuild_threshold() inserts.
            let needs_full_rebuild =
                state.hnsw_index.is_none() || state.incremental_count >= hnsw_rebuild_threshold();

            // During full rebuild the old index and new batch coexist briefly,
            // but `build_batched` streams one batch at a time so peak memory is
            // old_index + one_batch, not 2× the full index.
            if needs_full_rebuild {
                match super::commands::build_hnsw_index_owned(store, cfg.cqs_dir) {
                    Ok(Some(index)) => {
                        let n = index.len();
                        state.hnsw_index = Some(index);
                        state.incremental_count = 0;
                        if let Err(e) = store.set_hnsw_dirty(false) {
                            tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                        }
                        info!(vectors = n, "HNSW index rebuilt (full)");
                        if !cfg.quiet {
                            println!("  HNSW index: {} vectors (full rebuild)", n);
                        }
                    }
                    Ok(None) => {
                        state.hnsw_index = None;
                    }
                    Err(e) => {
                        warn!(error = %e, "HNSW rebuild failed, removing stale HNSW files (search falls back to brute-force)");
                        state.hnsw_index = None;
                        for ext in cqs::hnsw::HNSW_ALL_EXTENSIONS {
                            let path = cfg.cqs_dir.join(format!("index.{}", ext));
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
                // cleaned on the next full rebuild (every hnsw_rebuild_threshold()).
                let hash_refs: Vec<&str> = content_hashes.iter().map(|s| s.as_str()).collect();
                match store.get_chunk_ids_and_embeddings_by_hashes(&hash_refs) {
                    Ok(pairs) if !pairs.is_empty() => {
                        let items: Vec<(String, &[f32])> = pairs
                            .iter()
                            .map(|(id, emb)| (id.clone(), emb.as_slice()))
                            .collect();
                        if let Some(ref mut index) = state.hnsw_index {
                            match index.insert_batch(&items) {
                                Ok(n) => {
                                    state.incremental_count += n;
                                    // Save updated index to disk for search processes
                                    if let Err(e) = index.save(cfg.cqs_dir, "index") {
                                        warn!(error = %e, "Failed to save HNSW after incremental insert");
                                    } else if let Err(e) = store.set_hnsw_dirty(false) {
                                        tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                                    }
                                    info!(
                                        inserted = n,
                                        total = index.len(),
                                        incremental_count = state.incremental_count,
                                        "HNSW incremental insert"
                                    );
                                    if !cfg.quiet {
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
                                    state.hnsw_index = None;
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
                // RT-DATA-7: File was deleted — remove its chunks from the store
                if let Err(e) = store.delete_by_origin(rel_path) {
                    tracing::warn!(
                        path = %rel_path.display(),
                        error = %e,
                        "Failed to delete chunks for deleted file"
                    );
                }
                return vec![];
            }
            match parser.parse_file_all(&abs_path) {
                Ok((mut file_chunks, calls, chunk_type_refs)) => {
                    // Rewrite paths to be relative (AC-2: fix both file and id)
                    for chunk in &mut file_chunks {
                        chunk.file = rel_path.clone();
                        // Rewrite id: replace absolute path prefix with relative
                        // ID format: {path}:{line_start}:{content_hash}
                        if let Some(rest) = chunk.id.strip_prefix(&abs_path.display().to_string()) {
                            chunk.id = format!("{}{}", rel_path.display(), rest);
                        }
                    }
                    // Stash type refs for upsert after chunks are stored
                    if !chunk_type_refs.is_empty() {
                        all_type_refs.push((rel_path.clone(), chunk_type_refs));
                    }
                    // RT-DATA-8: Write function_calls table (file-level call graph).
                    // Previously discarded — callers/impact/trace commands need this.
                    if !calls.is_empty() {
                        if let Err(e) = store.upsert_function_calls(rel_path, &calls) {
                            tracing::warn!(
                                path = %rel_path.display(),
                                error = %e,
                                "Failed to write function_calls for watched file"
                            );
                        }
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
    // Pre-group calls by chunk ID for O(1) lookup per file (PERF-4).
    let mut calls_by_id: HashMap<String, Vec<cqs::parser::CallSite>> = HashMap::new();
    for chunk in &chunks {
        let calls = parser.extract_calls_from_chunk(chunk);
        if !calls.is_empty() {
            calls_by_id
                .entry(chunk.id.clone())
                .or_default()
                .extend(calls);
        }
    }
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
        // PERF-4: O(1) lookup per chunk via pre-grouped HashMap instead of linear scan.
        let file_calls: Vec<_> = pairs
            .iter()
            .flat_map(|(c, _)| {
                calls_by_id
                    .get(&c.id)
                    .into_iter()
                    .flat_map(|calls| calls.iter().map(|call| (c.id.clone(), call.clone())))
            })
            .collect();
        store.upsert_chunks_and_calls(pairs, mtime, &file_calls)?;

        // DS-37 / RT-DATA-10: Delete phantom chunks — functions removed from the
        // file but still lingering in the index. The upsert above handles updates
        // and inserts; this cleans up deletions.
        //
        // Ideally this would share a transaction with upsert_chunks_and_calls, but
        // both methods manage their own internal transactions. A crash between the
        // two leaves phantoms that get cleaned on the next reindex. Propagate the
        // error rather than silently swallowing it.
        let live_ids: Vec<&str> = pairs.iter().map(|(c, _)| c.id.as_str()).collect();
        store.delete_phantom_chunks(file, &live_ids)?;
    }

    // Upsert type edges from the earlier parse_file_all() results.
    // Type edges are soft data — separate from chunk+call atomicity.
    // They depend on chunk IDs existing in the DB, which is why we upsert
    // them after chunks are stored above. Use batched version (single transaction).
    if let Err(e) = store.upsert_type_edges_for_files(&all_type_refs) {
        tracing::warn!(error = %e, "Failed to update type edges");
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

    // DS-34: Hold shared lock during read+index to prevent partial reads
    // if another process is writing notes concurrently (e.g., `cqs notes add`).
    let lock_file = std::fs::File::open(&notes_path)?;
    lock_file.lock_shared()?;

    let notes = parse_notes(&notes_path)?;
    if notes.is_empty() {
        drop(lock_file);
        return Ok(0);
    }

    let count = cqs::index_notes(&notes, &notes_path, store)?;

    drop(lock_file); // release lock after index completes

    if !quiet {
        let ns = store.note_stats()?;
        println!(
            "  Notes: {} total ({} warnings, {} patterns)",
            ns.total, ns.warnings, ns.patterns
        );
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    fn make_event(paths: Vec<PathBuf>, kind: EventKind) -> notify::Event {
        notify::Event {
            kind,
            paths,
            attrs: Default::default(),
        }
    }

    /// Helper to build a minimal WatchConfig for testing collect_events.
    fn test_watch_config<'a>(
        root: &'a Path,
        cqs_dir: &'a Path,
        notes_path: &'a Path,
        supported_ext: &'a HashSet<&'a str>,
    ) -> WatchConfig<'a> {
        // These fields are unused by collect_events but required by the struct.
        // We leak a parser since tests don't call process_file_changes.
        let parser = Box::leak(Box::new(CqParser::new().unwrap()));
        let embedder = Box::leak(Box::new(OnceCell::new()));
        let model_config = Box::leak(Box::new(ModelConfig::default_model()));
        WatchConfig {
            root,
            cqs_dir,
            notes_path,
            supported_ext,
            parser,
            embedder,
            quiet: true,
            model_config,
        }
    }

    fn test_watch_state() -> WatchState {
        WatchState {
            embedder_backoff: EmbedderBackoff::new(),
            pending_files: HashSet::new(),
            pending_notes: false,
            last_event: std::time::Instant::now(),
            last_indexed_mtime: HashMap::new(),
            hnsw_index: None,
            incremental_count: 0,
        }
    }

    // ===== EmbedderBackoff tests =====

    #[test]
    fn backoff_initial_state_allows_retry() {
        let backoff = EmbedderBackoff::new();
        assert!(backoff.should_retry(), "Fresh backoff should allow retry");
    }

    #[test]
    fn backoff_after_failure_delays_retry() {
        let mut backoff = EmbedderBackoff::new();
        backoff.record_failure();
        // After 1 failure, delay is 2^1 = 2 seconds
        assert!(
            !backoff.should_retry(),
            "Should not retry immediately after failure"
        );
        assert_eq!(backoff.failures, 1);
    }

    #[test]
    fn backoff_reset_clears_failures() {
        let mut backoff = EmbedderBackoff::new();
        backoff.record_failure();
        backoff.record_failure();
        backoff.reset();
        assert_eq!(backoff.failures, 0);
        assert!(backoff.should_retry());
    }

    #[test]
    fn backoff_caps_at_300s() {
        let mut backoff = EmbedderBackoff::new();
        // 2^9 = 512 > 300, so it should be capped
        for _ in 0..9 {
            backoff.record_failure();
        }
        // Verify it doesn't panic or overflow
        assert_eq!(backoff.failures, 9);
    }

    #[test]
    fn backoff_saturating_add_no_overflow() {
        let mut backoff = EmbedderBackoff::new();
        backoff.failures = u32::MAX;
        backoff.record_failure();
        assert_eq!(backoff.failures, u32::MAX, "Should saturate, not overflow");
    }

    // ===== collect_events tests =====

    #[test]
    fn collect_events_filters_unsupported_extensions() {
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs", "py", "js"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        // .txt is not supported
        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/readme.txt")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(
            state.pending_files.is_empty(),
            "Unsupported extension should not be added"
        );
        assert!(!state.pending_notes);
    }

    #[test]
    fn collect_events_skips_cqs_dir() {
        let root = PathBuf::from("/tmp/test_project");
        let cqs_dir = PathBuf::from("/tmp/test_project/.cqs");
        let notes_path = PathBuf::from("/tmp/test_project/docs/notes.toml");
        let supported: HashSet<&str> = ["rs", "db"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        let event = make_event(
            vec![PathBuf::from("/tmp/test_project/.cqs/index.db")],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(
            state.pending_files.is_empty(),
            ".cqs dir events should be skipped"
        );
    }

    #[test]
    fn collect_events_detects_notes_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let notes_dir = root.join("docs");
        std::fs::create_dir_all(&notes_dir).unwrap();
        let notes_path = notes_dir.join("notes.toml");
        std::fs::write(&notes_path, "# notes").unwrap();

        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        let event = make_event(
            vec![notes_path.clone()],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(state.pending_notes, "Notes path should set pending_notes");
        assert!(
            state.pending_files.is_empty(),
            "Notes should not be added to pending_files"
        );
    }

    #[test]
    fn collect_events_respects_max_pending_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let notes_path = root.join("docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        // Pre-fill pending_files to max_pending_files()
        for i in 0..max_pending_files() {
            state
                .pending_files
                .insert(PathBuf::from(format!("f{}.rs", i)));
        }

        // Create a real file so mtime check passes
        let new_file = root.join("overflow.rs");
        std::fs::write(&new_file, "fn main() {}").unwrap();

        let event = make_event(
            vec![new_file],
            EventKind::Create(notify::event::CreateKind::File),
        );

        collect_events(&event, &cfg, &mut state);

        assert_eq!(
            state.pending_files.len(),
            max_pending_files(),
            "Should not exceed max_pending_files()"
        );
    }

    #[test]
    fn collect_events_skips_unchanged_mtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let notes_path = root.join("docs/notes.toml");
        let supported: HashSet<&str> = ["rs"].iter().cloned().collect();
        let cfg = test_watch_config(&root, &cqs_dir, &notes_path, &supported);
        let mut state = test_watch_state();

        // Create a file and record its mtime as already indexed
        let file = root.join("src/lib.rs");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        state
            .last_indexed_mtime
            .insert(PathBuf::from("src/lib.rs"), mtime);

        let event = make_event(
            vec![file],
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
        );

        collect_events(&event, &cfg, &mut state);

        assert!(
            state.pending_files.is_empty(),
            "Unchanged mtime should be skipped"
        );
    }

    // ===== Constants tests =====

    #[test]
    fn hnsw_rebuild_threshold_is_reasonable() {
        assert!(hnsw_rebuild_threshold() > 0);
        assert!(hnsw_rebuild_threshold() <= 1000);
    }

    #[test]
    fn max_pending_files_is_bounded() {
        assert!(max_pending_files() > 0);
        assert!(max_pending_files() <= 100_000);
    }
}
