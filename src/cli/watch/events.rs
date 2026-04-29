//! Watch loop event collection and per-debounce-cycle dispatch.
//!
//! `collect_events` filters raw `notify::Event` into the pending-files /
//! pending-notes sets on `WatchState`; `process_file_changes` /
//! `process_note_changes` drain those sets into reindex calls. The
//! coalesce-then-dispatch shape here is what keeps a 100-file save burst
//! from triggering 100 separate embedder runs.

use super::*;

/// Maximum pending files to prevent unbounded memory growth.
/// Override with CQS_WATCH_MAX_PENDING env var.
pub(super) fn max_pending_files() -> usize {
    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("CQS_WATCH_MAX_PENDING")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000)
    })
}
/// Collect file system events into pending sets, filtering by extension and deduplicating.
pub(super) fn collect_events(event: &notify::Event, cfg: &WatchConfig, state: &mut WatchState) {
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
            tracing::debug!(path = %norm_path, "Skipping .cqs directory event");
            continue;
        }

        // #1002: .gitignore-matched paths are skipped. The matcher was
        // built once at cmd_watch startup; when it's None the user either
        // set CQS_WATCH_RESPECT_GITIGNORE=0, passed --no-ignore, or has no
        // .gitignore. The hardcoded `.cqs/` skip above still runs
        // regardless so the system's own files are always excluded.
        //
        // `matched_path_or_any_parents` walks up the path's parents so
        // that a file at `.claude/worktrees/agent-x/src/lib.rs` is
        // ignored when `.claude/` is in .gitignore. The leaf-only
        // `matched()` would miss this.
        if let Ok(matcher_guard) = cfg.gitignore.read() {
            if let Some(matcher) = matcher_guard.as_ref() {
                if matcher
                    .matched_path_or_any_parents(&path, false)
                    .is_ignore()
                {
                    tracing::trace!(
                        path = %norm_path,
                        "Skipping gitignore-matched path (#1002)"
                    );
                    continue;
                }
            }
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
            tracing::debug!(path = %path.display(), ext = %ext, "Skipping unsupported extension");
            continue;
        }

        // Convert to relative path
        if let Ok(rel) = path.strip_prefix(cfg.root) {
            // P2.56: dedup WSL/NTFS events. NTFS keeps 100 ns mtime resolution,
            // but FAT32 mounts have a 2-second granularity floor — two saves
            // within 2 s collide on the *same* mtime, so a strict `mtime <=
            // last` check would skip the second save. On WSL drvfs
            // (`/mnt/<letter>/`, where the drive may well be FAT32-formatted)
            // we treat ties as "not stale" — i.e. only skip when `mtime` is
            // strictly older than the cached `last`. On Linux/macOS we keep
            // the original `<=` because sub-second mtimes there are reliable
            // and equality genuinely means "same content, no reindex needed".
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                let coarse_fs = cqs::config::is_wsl_drvfs_path(&path);
                let stale = state.last_indexed_mtime.get(rel).is_some_and(|last| {
                    if coarse_fs {
                        mtime < *last
                    } else {
                        mtime <= *last
                    }
                });
                if stale {
                    tracing::trace!(path = %rel.display(), "Skipping unchanged mtime");
                    continue;
                }
            }
            if state.pending_files.len() < max_pending_files() {
                // PF-V1.30.1-9 / #1245: keep the queue keyed on
                // slash-normalized paths so a Windows-side edit and a
                // reconcile-side walk can't double-queue the same file
                // under two separators.
                state
                    .pending_files
                    .insert(super::reconcile::normalize_pending_path(rel));
            } else {
                // RM-V1.25-23: log per-event at debug (spammy on bulk
                // drops) and accumulate a counter; the once-per-cycle
                // summary fires in process_file_changes so operators
                // see the total truncation even if the level is info.
                state.dropped_this_cycle = state.dropped_this_cycle.saturating_add(1);
                tracing::debug!(
                    max = max_pending_files(),
                    path = %rel.display(),
                    "Watch pending_files full, dropping file event"
                );
            }
            state.last_event = std::time::Instant::now();
        }
    }
}

/// Process pending file changes: parse, embed, store atomically, then update HNSW.
///
/// Uses incremental HNSW insertion when an Owned index is available in memory.
/// Falls back to full rebuild on first run or after `hnsw_rebuild_threshold()` incremental inserts.
pub(super) fn process_file_changes(cfg: &WatchConfig, store: &Store, state: &mut WatchState) {
    let files: Vec<PathBuf> = state.pending_files.drain().collect();
    let _span = info_span!("process_file_changes", file_count = files.len()).entered();
    state.pending_files.shrink_to(64);

    // CQ-V1.30.1-1 / AC-V1.30.1-4 / DS-V1.30.1-D8: warn at the top so
    // operators see the count, but DO NOT reset the counter here — the
    // outer loop's `publish_watch_snapshot` runs after this function
    // returns, and `WatchSnapshot::compute` uses `dropped_this_cycle > 0`
    // as a Stale signal. If we zero it before the embedder check below
    // (which may early-return on init failure), the snapshot reports
    // `Fresh` even though events were dropped and never reindexed —
    // defeating `cqs eval --require-fresh`. Reset only after a
    // successful drain so the next cycle's snapshot reflects the
    // truthful state.
    if state.dropped_this_cycle > 0 {
        tracing::warn!(
            dropped = state.dropped_this_cycle,
            cap = max_pending_files(),
            "Watch event queue full this cycle; dropping events. Run `cqs index` to catch up"
        );
    }
    // OB-V1.30.1-9: replace stdout println with structured tracing.
    // The daemon has no terminal — stdout goes to journald via the
    // systemd unit which writes unstructured. Tracing routes through
    // the configured subscriber (journald JSON or stderr text) and
    // honours filter levels.
    tracing::info!(
        file_count = files.len(),
        files = ?files,
        "watch: reindexing changed files",
    );

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
    // Mark both HNSW kinds dirty before writing chunks (RT-DATA-6). The base
    // index derives from the same chunks as enriched, so a crash mid-write
    // can leave either graph stale.
    if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Enriched, true) {
        tracing::warn!(error = %e, "Cannot set enriched HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
    if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Base, true) {
        tracing::warn!(error = %e, "Cannot set base HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
    match reindex_files(
        cfg.root,
        store,
        &files,
        cfg.parser,
        emb,
        cfg.global_cache,
        cfg.quiet,
    ) {
        Ok((count, content_hashes)) => {
            // Record mtimes to skip duplicate events
            for (file, mtime) in pre_mtimes {
                state.last_indexed_mtime.insert(file, mtime);
            }
            // CQ-V1.30.1-1 / AC-V1.30.1-4: reset only after a successful
            // drain. The dropped events surfaced in the warn above are
            // also queued for reconcile (Layer 2) on the next idle pass,
            // so the count stays meaningful exactly until the reconcile
            // refills `pending_files` with the same paths.
            state.dropped_this_cycle = 0;
            // #969: recency prune for the mtime map. Previously this called
            // `Path::exists()` per entry, which on WSL 9P mounts issued up to
            // 5000 serial `stat()` syscalls on the watch thread. The map's
            // `SystemTime` values let us age out stale entries in-memory.
            // Re-adding a surviving file on its next event is a trivial insert.
            let pruned = prune_last_indexed_mtime(&mut state.last_indexed_mtime);
            if pruned > 0 {
                tracing::debug!(
                    pruned,
                    remaining = state.last_indexed_mtime.len(),
                    "Pruned stale last_indexed_mtime entries"
                );
            }
            if !cfg.quiet {
                println!("Indexed {} chunk(s)", count);
            }

            // #1004: incremental SPLADE encoding. Encoder is held in
            // WatchConfig and stays resident for the daemon's lifetime.
            // We encode every chunk in the files that were reindexed —
            // upsert_sparse_vectors is idempotent, so re-encoding an
            // unchanged chunk is correct just slightly wasteful. The
            // cheaper content-hash-dedup optimization is a follow-up.
            if count > 0 {
                match cfg.splade_encoder {
                    Some(encoder_mu) => {
                        // Build the list of files that actually had chunks
                        // reindexed (excluding deleted ones, which are
                        // handled by the FK CASCADE on DELETE FROM chunks).
                        // We re-use the original `files` snapshot — the
                        // ones that survived parsing are still tracked.
                        encode_splade_for_changed_files(encoder_mu, store, &files);
                    }
                    None if cqs::splade::resolve_splade_model_dir().is_some() => {
                        tracing::debug!(
                            new_chunks = count,
                            "SPLADE model present but encoder disabled this daemon — \
                             sparse coverage will drift until manual 'cqs index' \
                             (CQS_WATCH_INCREMENTAL_SPLADE=0 or load failed)"
                        );
                    }
                    None => {
                        // No SPLADE model configured — nothing to do.
                    }
                }
            }

            // === HNSW maintenance ===
            //
            // #1090: rebuilds run in a background thread (`spawn_hnsw_rebuild`).
            // The watch loop's responsibilities each cycle are:
            //
            //   1. Drain a completed rebuild — replay any (id, embedding) the
            //      loop captured during the build window into the new index,
            //      save, and atomically swap into `state.hnsw_index`.
            //   2. Decide whether to start a *new* rebuild (Owned-needed, or
            //      threshold reached) — and if a rebuild is already in flight,
            //      just record this cycle's chunks in the pending delta so
            //      they survive the swap.
            //   3. Otherwise (no rebuild needed, no rebuild in flight): take
            //      the fast incremental path on the in-memory Owned index.
            //
            // The result: incremental_insert never blocks on a full rebuild,
            // editor saves don't pause for 10-30s of CUDA work, and search
            // keeps using the prior index until the new one is ready.

            // 1. Drain a completed rebuild, if any.
            drain_pending_rebuild(cfg, store, state);

            let rebuild_in_flight = state.pending_rebuild.is_some();
            let needs_owned =
                state.hnsw_index.is_none() || state.incremental_count >= hnsw_rebuild_threshold();

            // 2. Start a new rebuild, if appropriate.
            if needs_owned && !rebuild_in_flight {
                let context = if state.hnsw_index.is_none() {
                    "rebuild_from_empty"
                } else {
                    "threshold_rebuild"
                };
                let pending = spawn_hnsw_rebuild(
                    cfg.cqs_dir.to_path_buf(),
                    cfg.cqs_dir.join(cqs::INDEX_DB_FILENAME),
                    store.dim(),
                    context,
                );
                info!(context, "Spawned background HNSW rebuild");
                if !cfg.quiet {
                    println!(
                        "  HNSW index: rebuild started in background ({}, search keeps using current index)",
                        context
                    );
                }
                state.pending_rebuild = Some(pending);
            }

            // 3. Either drop new chunks into the in-flight rebuild's delta,
            //    or run the fast incremental path.
            if !content_hashes.is_empty() {
                let hash_refs: Vec<&str> = content_hashes.iter().map(|s| s.as_str()).collect();
                match store.get_chunk_ids_and_embeddings_by_hashes(&hash_refs) {
                    Ok(pairs) if !pairs.is_empty() => {
                        if let Some(ref mut pending) = state.pending_rebuild {
                            // A rebuild is in flight (just spawned this cycle,
                            // or carried over from a prior one). The rebuild
                            // thread's snapshot may not include these chunks —
                            // capture them so `drain_pending_rebuild` can
                            // replay them after the swap.
                            //
                            // P2.72: cap the delta. If the rebuild stalls long
                            // enough to accumulate >MAX_PENDING_REBUILD_DELTA
                            // entries, latch `delta_saturated` and stop
                            // appending. The drain path will discard the
                            // rebuilt index instead of swapping a stale
                            // snapshot; the next threshold rebuild reads
                            // SQLite fresh and recovers everything.
                            if pending.delta.len() + pairs.len() > MAX_PENDING_REBUILD_DELTA {
                                if !pending.delta_saturated {
                                    tracing::warn!(
                                        cap = MAX_PENDING_REBUILD_DELTA,
                                        current = pending.delta.len(),
                                        "Pending HNSW rebuild delta saturated; \
                                         abandoning in-flight rebuild — next threshold \
                                         rebuild will pick up changes from SQLite"
                                    );
                                    pending.delta_saturated = true;
                                }
                                // Drop the new pairs; SQLite is the source of truth.
                            } else {
                                let added = pairs.len();
                                pending.delta.extend(pairs);
                                tracing::debug!(
                                    added,
                                    total_delta = pending.delta.len(),
                                    "Captured chunks in pending rebuild delta"
                                );
                                if !cfg.quiet {
                                    println!(
                                        "  HNSW index: +{} vectors queued for in-flight rebuild ({} total deferred)",
                                        added,
                                        pending.delta.len()
                                    );
                                }
                            }
                        } else if let Some(ref mut index) = state.hnsw_index {
                            // Fast incremental path — Owned in memory, no rebuild pending.
                            // Modified chunks get new IDs; old vectors become orphans
                            // in the HNSW graph (hnsw_rs has no deletion). Orphans are
                            // harmless: search post-filters against live SQLite chunk
                            // IDs. They're cleaned on the next threshold rebuild.
                            //
                            // P1.17 / #1124: `pairs` carries content_hash as the
                            // third tuple slot for the rebuild-window path; the
                            // incremental insert only needs (id, embedding).
                            let items: Vec<(String, &[f32])> = pairs
                                .iter()
                                .map(|(id, emb, _hash)| (id.clone(), emb.as_slice()))
                                .collect();
                            match index.insert_batch(&items) {
                                Ok(n) => {
                                    state.incremental_count += n;
                                    if let Err(e) = index.save(cfg.cqs_dir, "index") {
                                        warn!(error = %e, "Failed to save HNSW after incremental insert");
                                    } else {
                                        clear_hnsw_dirty_with_retry(
                                            store,
                                            cqs::HnswKind::Enriched,
                                            "incremental_insert",
                                        );
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
                                    // Insert failed. Rather than blocking on a
                                    // synchronous rebuild (the old behavior),
                                    // queue a background one — search keeps
                                    // serving from the current index meanwhile.
                                    warn!(
                                        error = %e,
                                        "HNSW incremental insert failed; spawning background rebuild"
                                    );
                                    let pending = spawn_hnsw_rebuild(
                                        cfg.cqs_dir.to_path_buf(),
                                        cfg.cqs_dir.join(cqs::INDEX_DB_FILENAME),
                                        store.dim(),
                                        "incremental_insert_failure",
                                    );
                                    // Carry these new chunks over into the new
                                    // rebuild's delta so they survive the swap.
                                    let mut p = pending;
                                    p.delta.extend(pairs);
                                    state.pending_rebuild = Some(p);
                                }
                            }
                        }
                        // No pending and no in-memory index → first save with
                        // empty store. The needs_owned branch above already
                        // spawned a rebuild this cycle; pairs were captured
                        // there. Nothing to do here.
                    }
                    Ok(_) => {} // no embeddings found for hashes
                    Err(e) => {
                        warn!(error = %e, "Failed to fetch embeddings for HNSW update");
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Reindex error");
            // EH-V1.30.1-8: the dirty flag was set above (lines 184/189)
            // before the reindex attempt and the success path's
            // `clear_hnsw_dirty_with_retry` is unreachable from this arm.
            // Surface that the HNSW will be marked dirty on disk until a
            // successful reindex cycle clears it — operators correlate
            // this with the prior `Reindex error` to diagnose persistent
            // dirty state (SQLite busy / OOM / etc.) and search may
            // serve stale results in the meantime.
            tracing::warn!(
                hnsw_kinds = "enriched,base",
                "HNSW dirty flag remains set after reindex failure; \
                 search may serve stale results until next successful \
                 reindex cycle clears it"
            );
        }
    }
}

/// Process notes.toml changes: parse and store notes (no embedding needed, SQ-9).
pub(super) fn process_note_changes(root: &Path, store: &Store, quiet: bool) {
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
