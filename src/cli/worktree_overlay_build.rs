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
pub(crate) fn build_overlay(
    worktree_root: &Path,
    parent_root: &Path,
    parser: &CqParser,
    embedder: &Embedder,
    global_cache: Option<&cqs::cache::EmbeddingCache>,
) -> Result<Option<WorktreeOverlay>, cqs::worktree_overlay::OverlayError> {
    let _span = tracing::info_span!(
        "build_overlay",
        worktree = %worktree_root.display()
    )
    .entered();
    let started = Instant::now();

    let delta = discover_delta(worktree_root, parent_root)?;
    let fp = fingerprint(worktree_root, &delta);

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

        let overlay = build_overlay(&wt, &parent, &parser, &embedder, None)
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
}
