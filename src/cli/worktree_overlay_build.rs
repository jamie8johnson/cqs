//! Bin-side builder for the worktree search overlay.
//!
//! The crate-portable pieces — delta discovery, the `-z` parser, the
//! fingerprint, and the [`WorktreeOverlay`] struct — live in the lib module
//! `cqs::worktree_overlay`. The *build* lives here because it drives the
//! incremental indexing pipeline (`reindex_files`), which is a bin-crate
//! function: a lib module cannot call it across the lib/bin boundary.
//!
//! `build_overlay` is the single entry: discover the dirty delta, open a
//! fresh in-memory store, init its schema at the daemon's model dimension,
//! parse+embed the parse set into it, and assemble the overlay (store +
//! mask set + fingerprint + stats). Nothing here touches the query path —
//! that is PR-2.
//!
//! ## Cache write note (intentional cross-boundary write)
//!
//! When a `global_cache` is supplied (the daemon passes the parent's
//! `embeddings_cache.db`), the overlay build **writes** content-addressed
//! cache rows from worktree content into the parent's cache. This is
//! deliberate: repeat builds across fingerprints become cheap, the cache is
//! not index truth (content-addressed, rebuildable), and the #1814 parent-
//! index write guard covers CLI write *commands*, not daemon cache
//! maintenance. It is a behavior decision, recorded here and in the module
//! docs of `cqs::worktree_overlay`.

use std::path::Path;
use std::time::Instant;

use cqs::embedder::Embedder;
use cqs::parser::Parser as CqParser;
use cqs::store::{ModelInfo, Store};
use cqs::worktree_overlay::{discover_delta, fingerprint, OverlayStats, WorktreeOverlay};

/// Build a [`WorktreeOverlay`] for `worktree_root` against `parent_root`.
///
/// - `worktree_root`: the dirty checkout to overlay (validated by the
///   caller via `cqs::worktree::overlay_root` / daemon-side root checks).
/// - `parent_root`: the parent project root whose index the overlay shadows
///   (the diff base is *its* HEAD — see plan correction #2).
/// - `parser` / `embedder`: the session's resident parser + embedder (the
///   daemon's, so overlay embeddings are dimension-compatible with prepared
///   queries by construction).
/// - `parent_store`: the resident parent index. Its SQLite notes are copied
///   into the overlay store so the overlay leg's note-boost index computes the
///   same sentiment multiplier and records the same `note_boost` provenance the
///   parent would — notes are project-level metadata keyed on mentions, not on
///   uncommitted content, so they belong in a faithful shadow.
/// - `global_cache`: optional embedding cache (see the cache-write note in
///   the module docs).
///
/// Returns `Ok(None)` when there is no delta to overlay (clean worktree) —
/// the caller serves the parent index unchanged. Returns
/// `Err(OverlayError::DeltaTooLarge)` when the delta exceeds the cap; the
/// caller maps that to `skipped-delta-too-large`.
// Plumbing landed ahead of its caller: the daemon's `BatchView::overlay()`
// (PR-3) is the production caller. Exercised under `slow-tests` by
// `build_overlay_indexes_dirty_delta` below.
#[allow(dead_code)]
pub(crate) fn build_overlay<M>(
    worktree_root: &Path,
    parent_root: &Path,
    parser: &CqParser,
    embedder: &Embedder,
    parent_store: &Store<M>,
    global_cache: Option<&cqs::cache::EmbeddingCache>,
) -> Result<Option<WorktreeOverlay>, cqs::worktree_overlay::OverlayError> {
    let _span = tracing::info_span!(
        "build_overlay",
        worktree = %worktree_root.display()
    )
    .entered();
    let started = Instant::now();

    let delta = discover_delta(worktree_root, parent_root)?;
    // Fold the parent's notes-revision token into the overlay's cache identity.
    // Notes are copied into the shadow store below (so its `note_boost` matches
    // the parent), so they must participate in the fingerprint too: a parent
    // notes mutation flips the token → the LRU rebuilds with fresh notes rather
    // than serving a stale boost until the deferrable overlay-clear. A read
    // failure degrades to a zero token (a stable sentinel) — the overlay still
    // serves correct hits; it just won't auto-invalidate on a notes change,
    // which the deferrable clear still covers.
    let notes_revision = parent_store.notes_revision().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "overlay: failed to read parent notes-revision — fingerprint will not track notes for this build");
        [0u8; 32]
    });
    let fp = fingerprint(worktree_root, &delta, &notes_revision);

    // A clean worktree (no touched origins) has nothing to overlay. Return
    // None so the caller skips the merge entirely rather than building an
    // empty store and an all-pass mask.
    if delta.masked_origins.is_empty() {
        tracing::debug!("overlay: clean worktree, nothing to overlay");
        return Ok(None);
    }

    // Fresh in-memory store. `open_memory` pins one connection so the
    // `:memory:` DB survives; `init` creates the schema; `set_dim` syncs the
    // real embedding dimension (the metadata write in `init` does not update
    // the already-read `dim` field).
    let mut store = Store::open_memory()?;
    let model_info = ModelInfo::new(&embedder.model_config().repo, embedder.embedding_dim());
    store.init(&model_info)?;
    store.set_dim(model_info.dimensions);

    // Copy the parent's notes into the shadow store so the overlay leg's
    // note-boost index demotes/promotes a noted-and-edited file the same way
    // the parent leg would — and records the `note_boost` provenance. Notes are
    // keyed on mentions, not on the dirty content the overlay replaces, so the
    // shadow is faithful only with them. A copy failure is non-fatal: the
    // overlay still serves correct hits, just without the note multiplier on
    // the masked-and-re-served files (degrade, don't drop the overlay).
    match store.copy_notes_from(parent_store) {
        Ok(n) => {
            if n > 0 {
                tracing::debug!(notes = n, "overlay: copied parent notes into shadow store");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "overlay: failed to copy parent notes — note boosts will be absent on overlay hits");
        }
    }

    // Parse + embed the parse set into the overlay store via the incremental
    // pipeline. `reindex_files` tolerates per-file parse failure and rewrites
    // chunk ids/paths to relative — exactly the watch hot path. The `D`-path
    // delete branch inside it is never reached here (D paths are masked-only,
    // excluded from the parse set in `discover_delta`).
    let (chunks_indexed, errors) = crate::cli::watch::overlay_reindex_files(
        worktree_root,
        &store,
        &delta.parse_set,
        parser,
        embedder,
        global_cache,
        /* quiet */ true,
    )
    .map_err(|e| cqs::worktree_overlay::OverlayError::Build(e.to_string()))?;
    if !errors.is_empty() {
        // Per-file failures are non-fatal: the origin is still masked (the
        // overlay is the authority for it), it just contributes no hits.
        tracing::warn!(
            failed = errors.len(),
            "overlay: some delta files failed to parse (origins remain masked)"
        );
    }

    let stats = OverlayStats {
        files_in_delta: delta.masked_origins.len(),
        chunks_indexed,
        build_ms: started.elapsed().as_millis(),
    };
    tracing::info!(
        files = stats.files_in_delta,
        chunks = stats.chunks_indexed,
        build_ms = stats.build_ms,
        "overlay built"
    );

    Ok(Some(WorktreeOverlay {
        store,
        masked_origins: delta.masked_origins,
        fingerprint: fp,
        worktree_root: worktree_root.to_path_buf(),
        stats,
    }))
}

#[cfg(all(test, feature = "slow-tests"))]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// End-to-end: a dirty worktree delta is parsed + embedded into the
    /// overlay's in-memory store, and the overlay carries the mask set and
    /// chunk count. Gated behind `slow-tests` because it cold-loads the
    /// embedder. Justifies `build_overlay` having a caller in PR-1.
    #[test]
    fn build_overlay_indexes_dirty_delta() {
        use cqs::embedder::ModelConfig;

        let embedder = match cqs::Embedder::new_cpu(ModelConfig::resolve(None, None)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Skipping build_overlay e2e: embedder init failed: {e}");
                return;
            }
        };
        let parser = CqParser::new().expect("parser");

        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir_all(parent.join("src")).unwrap();
        std::fs::write(parent.join("src/lib.rs"), "pub fn alpha() -> i32 { 1 }\n").unwrap();
        git(&parent, &["init", "-q", "-b", "main"]);
        git(&parent, &["config", "user.email", "t@e.com"]);
        git(&parent, &["config", "user.name", "T"]);
        git(&parent, &["add", "-A"]);
        git(&parent, &["commit", "-q", "-m", "init"]);

        let wt = tmp.path().join("wt");
        git(
            &parent,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );

        // Add a fresh worktree-only file (the lane's new code).
        std::fs::write(
            wt.join("src/feature.rs"),
            "pub fn brand_new_overlay_symbol() -> i32 { 7 }\n",
        )
        .unwrap();

        let parent = dunce::canonicalize(&parent).unwrap_or(parent);
        let wt = dunce::canonicalize(&wt).unwrap_or(wt);

        // A minimal parent store (no notes) — this test exercises masking, not
        // the note copy; the empty-notes case must leave the overlay unchanged.
        let parent_store = Store::open_memory().expect("parent store");
        let parent_model = ModelInfo::new(&embedder.model_config().repo, embedder.embedding_dim());
        parent_store.init(&parent_model).expect("init parent store");

        let overlay = build_overlay(&wt, &parent, &parser, &embedder, &parent_store, None)
            .expect("build_overlay")
            .expect("non-clean worktree yields Some(overlay)");

        assert!(
            overlay
                .masked_origins
                .contains(&std::path::PathBuf::from("src/feature.rs")),
            "new file masked"
        );
        assert!(
            overlay.stats.chunks_indexed > 0,
            "overlay indexed at least one chunk from the new file"
        );
        assert_eq!(overlay.worktree_root, wt);
    }

    /// `build_overlay` copies the parent's notes into the shadow store so a
    /// file that is both noted in the parent AND edited in the worktree keeps
    /// its note multiplier when re-served from the overlay leg. End-to-end
    /// through `build_overlay` (not the unit `copy_notes_from`): proves the copy
    /// is actually wired into the production build, against a real parse+embed.
    #[test]
    fn build_overlay_copies_parent_notes_into_shadow() {
        use cqs::embedder::ModelConfig;
        use cqs::note::Note;

        let embedder = match cqs::Embedder::new_cpu(ModelConfig::resolve(None, None)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Skipping build_overlay notes e2e: embedder init failed: {e}");
                return;
            }
        };
        let parser = CqParser::new().expect("parser");

        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir_all(parent.join("src")).unwrap();
        std::fs::write(parent.join("src/lib.rs"), "pub fn alpha() -> i32 { 1 }\n").unwrap();
        git(&parent, &["init", "-q", "-b", "main"]);
        git(&parent, &["config", "user.email", "t@e.com"]);
        git(&parent, &["config", "user.name", "T"]);
        git(&parent, &["add", "-A"]);
        git(&parent, &["commit", "-q", "-m", "init"]);

        let wt = tmp.path().join("wt");
        git(
            &parent,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );

        // Edit lib.rs in the worktree — this masks `src/lib.rs` out of the
        // parent leg and re-serves it from the overlay. The parent has a -0.5
        // note on exactly this file.
        std::fs::write(
            wt.join("src/lib.rs"),
            "pub fn alpha() -> i32 { 2 }\npub fn beta() -> i32 { 3 }\n",
        )
        .unwrap();

        let parent = dunce::canonicalize(&parent).unwrap_or(parent);
        let wt = dunce::canonicalize(&wt).unwrap_or(wt);

        // Parent store with a note mentioning the edited file.
        let parent_store = Store::open_memory().expect("parent store");
        let parent_model = ModelInfo::new(&embedder.model_config().repo, embedder.embedding_dim());
        parent_store.init(&parent_model).expect("init parent store");
        let note = Note {
            id: "note:0".to_string(),
            text: "lib is load-bearing".to_string(),
            sentiment: -0.5,
            mentions: vec!["src/lib.rs".to_string()],
            kind: None,
        };
        parent_store
            .upsert_notes_batch(&[note], std::path::Path::new("docs/notes.toml"), 100)
            .expect("seed parent note");

        let overlay = build_overlay(&wt, &parent, &parser, &embedder, &parent_store, None)
            .expect("build_overlay")
            .expect("non-clean worktree yields Some(overlay)");

        // The note crossed into the shadow store's notes table (public API:
        // `cached_note_boost_index` is lib-crate-private, so assert through the
        // public notes surface + a real search instead).
        assert_eq!(
            overlay.store.note_count().expect("shadow note count"),
            1,
            "build_overlay must copy the parent note into the shadow store"
        );
        let summaries = overlay
            .store
            .list_notes_summaries()
            .expect("shadow note summaries");
        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].mentions,
            vec!["src/lib.rs".to_string()],
            "the copied note keeps its mention"
        );

        // End-to-end provenance: a search of the shadow store for the edited
        // file records `note_boost` with the -0.5 demotion — the multiplier the
        // empty-notes overlay would have silently dropped.
        let query_emb = embedder.embed_query("beta function").expect("embed query");
        let mut filter = cqs::SearchFilter::default();
        filter.query_text = "beta function".to_string();
        filter.record_rank_signals = true;
        let hits = overlay
            .store
            .search_filtered_with_index(&query_emb, &filter, 10, 0.0, None)
            .expect("shadow search");
        let lib_hit = hits
            .iter()
            .find(|r| r.chunk.file == std::path::Path::new("src/lib.rs"))
            .expect("a chunk from the edited+noted file is retrieved");
        let note_signal = lib_hit
            .rank_signals
            .iter()
            .find(|s| s.signal == "note_boost")
            .expect("overlay hit records note_boost provenance");
        assert!(
            note_signal.value < 1.0,
            "note_boost reflects the -0.5 demotion, got {}",
            note_signal.value
        );
    }

    /// End-to-end invalidation: a parent notes mutation moves the overlay's
    /// fingerprint and the rebuilt shadow store's `note_boost` reflects the new
    /// sentiment. Proven through the real build: the notes-revision token
    /// folded into the fingerprint means a
    /// `cqs notes update` on the parent invalidates the overlay's cache identity
    /// (fingerprint differs → LRU miss → rebuild with fresh notes) instead of
    /// serving a stale boost until the deferrable overlay-clear runs.
    #[test]
    fn build_overlay_fingerprint_tracks_parent_note_sentiment() {
        use cqs::embedder::ModelConfig;
        use cqs::note::Note;

        let embedder = match cqs::Embedder::new_cpu(ModelConfig::resolve(None, None)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Skipping build_overlay notes-revision e2e: embedder init failed: {e}");
                return;
            }
        };
        let parser = CqParser::new().expect("parser");

        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir_all(parent.join("src")).unwrap();
        std::fs::write(parent.join("src/lib.rs"), "pub fn alpha() -> i32 { 1 }\n").unwrap();
        git(&parent, &["init", "-q", "-b", "main"]);
        git(&parent, &["config", "user.email", "t@e.com"]);
        git(&parent, &["config", "user.name", "T"]);
        git(&parent, &["add", "-A"]);
        git(&parent, &["commit", "-q", "-m", "init"]);

        let wt = tmp.path().join("wt");
        git(
            &parent,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );

        // Edit lib.rs in the worktree so its origin is masked + re-served from
        // the overlay. The worktree delta is held FIXED across the two builds —
        // only the parent note changes — so any fingerprint move is attributable
        // to the notes-revision token, not the delta.
        std::fs::write(
            wt.join("src/lib.rs"),
            "pub fn alpha() -> i32 { 2 }\npub fn beta() -> i32 { 3 }\n",
        )
        .unwrap();

        let parent = dunce::canonicalize(&parent).unwrap_or(parent);
        let wt = dunce::canonicalize(&wt).unwrap_or(wt);

        let parent_store = Store::open_memory().expect("parent store");
        let parent_model = ModelInfo::new(&embedder.model_config().repo, embedder.embedding_dim());
        parent_store.init(&parent_model).expect("init parent store");

        // First build: a -0.5 (demotion) note on the edited file.
        let note_neg = Note {
            id: "note:0".to_string(),
            text: "lib is load-bearing".to_string(),
            sentiment: -0.5,
            mentions: vec!["src/lib.rs".to_string()],
            kind: None,
        };
        parent_store
            .upsert_notes_batch(&[note_neg], std::path::Path::new("docs/notes.toml"), 100)
            .expect("seed parent note");

        let overlay0 = build_overlay(&wt, &parent, &parser, &embedder, &parent_store, None)
            .expect("build_overlay (neg)")
            .expect("non-clean worktree yields Some(overlay)");
        let fp0 = overlay0.fingerprint;

        // Mutate the parent note's sentiment (the `cqs notes update` path):
        // -0.5 → +0.5. INSERT OR REPLACE on the same id is the update.
        let note_pos = Note {
            id: "note:0".to_string(),
            text: "lib is load-bearing".to_string(),
            sentiment: 0.5,
            mentions: vec!["src/lib.rs".to_string()],
            kind: None,
        };
        parent_store
            .upsert_notes_batch(&[note_pos], std::path::Path::new("docs/notes.toml"), 200)
            .expect("update parent note");

        let overlay1 = build_overlay(&wt, &parent, &parser, &embedder, &parent_store, None)
            .expect("build_overlay (pos)")
            .expect("non-clean worktree yields Some(overlay)");
        let fp1 = overlay1.fingerprint;

        // The fingerprint moved — so the LRU treats overlay1 as a miss and
        // serves the rebuilt store, not the stale overlay0.
        assert_ne!(
            fp0, fp1,
            "a parent note sentiment change must move the overlay fingerprint"
        );

        // And the rebuilt overlay's note_boost reflects the NEW (+0.5) sentiment:
        // a boost above 1.0, where overlay0 carried a demotion below 1.0.
        let query_emb = embedder.embed_query("beta function").expect("embed query");
        let mut filter = cqs::SearchFilter::default();
        filter.query_text = "beta function".to_string();
        filter.record_rank_signals = true;

        let boost_of = |ov: &WorktreeOverlay| -> f32 {
            let hits = ov
                .store
                .search_filtered_with_index(&query_emb, &filter, 10, 0.0, None)
                .expect("shadow search");
            let lib_hit = hits
                .iter()
                .find(|r| r.chunk.file == std::path::Path::new("src/lib.rs"))
                .expect("a chunk from the edited+noted file is retrieved");
            lib_hit
                .rank_signals
                .iter()
                .find(|s| s.signal == "note_boost")
                .expect("overlay hit records note_boost provenance")
                .value
        };

        assert!(
            boost_of(&overlay0) < 1.0,
            "overlay0 carried the -0.5 demotion"
        );
        assert!(
            boost_of(&overlay1) > 1.0,
            "overlay1 (rebuilt after the notes update) carries the +0.5 boost"
        );
    }

    /// End-to-end candidate-recompute under the overlay: a parent-LIVE function
    /// `target` (real `call` edge from `caller` in `src/lib.rs`) loses that edge
    /// in the worktree (the worktree's `src/lib.rs` no longer calls it), so it is
    /// a Direction-B addition (`overlay_dead`). A worktree-NEW file
    /// `src/feature.rs` references `target` as a bare fn-pointer argument
    /// (`register(target)`) — a `bare_arg_unresolved` `candidate_edges` row the
    /// confident extractor declines to resolve, landing in the OVERLAY store.
    /// The verdict classifier must consult the overlay-merged candidate map and
    /// relabel `target` `low-confidence-live`, NOT `dead` — the bug this fixes.
    /// The `_meta.overlay_graph` marker stays `"full"` (the candidate section is
    /// now overlaid too), gated on participation, which a Direction-B addition
    /// raises. Mirrors `build_overlay_indexes_dirty_delta` for the real-overlay
    /// scaffolding; cold-loads the embedder, so `slow-tests`.
    #[test]
    fn overlay_candidate_recompute_relabels_low_confidence_live() {
        use cqs::embedder::ModelConfig;
        use cqs::store::{DeadConfidence, ReadOnly};

        let embedder = match cqs::Embedder::new_cpu(ModelConfig::resolve(None, None)) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Skipping overlay candidate-recompute e2e: embedder init failed: {e}");
                return;
            }
        };
        let parser = CqParser::new().expect("parser");

        let tmp = tempfile::TempDir::new().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir_all(parent.join("src")).unwrap();
        // Parent: `caller` makes a real `call` edge to `target`, so `target` is
        // LIVE in the parent index.
        std::fs::write(
            parent.join("src/lib.rs"),
            "pub fn target() -> i32 { 1 }\npub fn caller() -> i32 { target() }\n",
        )
        .unwrap();
        git(&parent, &["init", "-q", "-b", "main"]);
        git(&parent, &["config", "user.email", "t@e.com"]);
        git(&parent, &["config", "user.name", "T"]);
        git(&parent, &["add", "-A"]);
        git(&parent, &["commit", "-q", "-m", "init"]);

        let wt = tmp.path().join("wt");
        git(
            &parent,
            &["worktree", "add", "-q", "-b", "lane", wt.to_str().unwrap()],
        );

        // Worktree edit: `caller` no longer calls `target` — its only real-caller
        // edge lived in `src/lib.rs` (now masked + re-served from the overlay
        // without the call), so the merged real caller graph has zero callers for
        // `target` → it becomes a Direction-B addition.
        std::fs::write(
            wt.join("src/lib.rs"),
            "pub fn target() -> i32 { 1 }\npub fn caller() -> i32 { 0 }\n",
        )
        .unwrap();
        // Worktree-new file: a bare fn-pointer arg `target` the confident pass
        // drops (cross-file, not defined here) → a `bare_arg_unresolved`
        // candidate naming `target`, landing in the overlay store.
        std::fs::write(
            wt.join("src/feature.rs"),
            "fn register(_f: fn() -> i32) {}\npub fn use_it() { register(target); }\n",
        )
        .unwrap();

        let parent = dunce::canonicalize(&parent).unwrap_or(parent);
        let wt = dunce::canonicalize(&wt).unwrap_or(wt);

        // Build the parent index ON DISK so it can be re-opened ReadOnly (the
        // dead-overlay core takes a `Store<ReadOnly>`). Index parent `src/lib.rs`
        // through the real pipeline so the parent's `caller→target` call edge and
        // `target`'s def chunk are present.
        let parent_db = parent.join("parent-index.db");
        {
            let parent_store = Store::open(&parent_db).expect("open parent store");
            let parent_model =
                ModelInfo::new(&embedder.model_config().repo, embedder.embedding_dim());
            parent_store.init(&parent_model).expect("init parent store");
            crate::cli::watch::overlay_reindex_files(
                &parent,
                &parent_store,
                &[std::path::PathBuf::from("src/lib.rs")],
                &parser,
                &embedder,
                None,
                /* quiet */ true,
            )
            .expect("index parent files");
        }
        let parent_store_ro = Store::<ReadOnly>::open_readonly(&parent_db).expect("reopen parent");

        // Parent-truth sanity: `target` is NOT dead (it has a real caller).
        let parent_only = crate::cli::commands::dead_overlay(
            &parent_store_ro,
            &parent,
            &crate::cli::commands::DeadArgs {
                include_pub: true,
                min_confidence: DeadConfidence::Low,
                verdict: None,
            },
            None,
        )
        .expect("parent-only dead")
        .0;
        assert!(
            !parent_only.dead.iter().any(|e| e.name == "target"),
            "parent-truth: `target` has a real caller and must not be dead: {:?}",
            parent_only.dead.iter().map(|e| &e.name).collect::<Vec<_>>()
        );

        // Build the worktree overlay against the on-disk parent store.
        let overlay = {
            let parent_store_for_build = Store::open(&parent_db).expect("open parent for build");
            build_overlay(
                &wt,
                &parent,
                &parser,
                &embedder,
                &parent_store_for_build,
                None,
            )
            .expect("build_overlay")
            .expect("non-clean worktree yields Some(overlay)")
        };

        // The candidate landed in the overlay store (the KEY FACT this fix rests
        // on): `target` is named in the overlay's `candidate_edges`.
        let overlay_cands = overlay
            .store
            .candidate_edge_contributions()
            .expect("overlay candidate contributions");
        assert!(
            overlay_cands.iter().any(|(name, _, _)| name == "target"),
            "the worktree candidate edge must land in the overlay store: {overlay_cands:?}"
        );

        // Run the overlay-aware dead core: `apply_dead_overlay` adds `target` as a
        // Direction-B `overlay_dead` entry, and `build_dead_output` consults the
        // overlay-merged candidate map → `low-confidence-live`.
        let (output, participated) = crate::cli::commands::dead_overlay(
            &parent_store_ro,
            &parent,
            &crate::cli::commands::DeadArgs {
                include_pub: true,
                min_confidence: DeadConfidence::Low,
                verdict: None,
            },
            Some(&overlay),
        )
        .expect("overlay dead");

        let entry = output
            .dead
            .iter()
            .find(|e| e.name == "target")
            .unwrap_or_else(|| {
                panic!(
                    "`target` must surface as a Direction-B addition: {:?}",
                    output.dead.iter().map(|e| &e.name).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            entry.verdict, "low-confidence-live",
            "a candidate-only Direction-B addition must relabel low-confidence-live, not dead: \
             {entry:?}"
        );
        assert!(
            entry.verdict_reason.contains("candidate edge")
                && entry.verdict_reason.contains("bare_arg_unresolved"),
            "reason must name the merged candidate kind/count: {}",
            entry.verdict_reason
        );

        // Participation is true (a Direction-B addition changed the set), which is
        // what gates the honest `_meta.overlay_graph = "full"` marker.
        assert!(
            participated,
            "a Direction-B addition must report overlay participation (gates the `full` marker)"
        );
    }
}
