//! Batch command handlers — one function per BatchCmd variant.
//!
//! Split into submodules by concern:
//! - `search` - search/query dispatch
//! - `graph` - callers, callees, deps, impact, test-map, trace, related, impact-diff
//! - `analysis` - dead, health, stale, suggest, review, ci
//! - `info` - stats, context, explain, similar, read, blame, onboard
//! - `misc` - notes, gc, plan, task, scout, where, gather, diff, drift, refresh, help

mod analysis;
mod graph;
mod info;
mod misc;
mod search;

// Smoke tests for 11 of the 16 dispatch handlers (the remaining 5 require
// embedder cold-load and are intentionally skipped — see module docs).
#[cfg(test)]
mod dispatch_tests;

pub(super) use analysis::{
    dispatch_ci, dispatch_dead, dispatch_health, dispatch_review, dispatch_stale, dispatch_suggest,
};
pub(super) use graph::{
    dispatch_callees, dispatch_callers, dispatch_deps, dispatch_impact, dispatch_impact_diff,
    dispatch_related, dispatch_test_map, dispatch_trace,
};
pub(super) use info::{
    dispatch_blame, dispatch_context, dispatch_explain, dispatch_onboard, dispatch_read,
    dispatch_similar, dispatch_stats,
};
pub(super) use misc::{
    dispatch_diff, dispatch_drift, dispatch_gather, dispatch_gc, dispatch_help, dispatch_index,
    dispatch_notes, dispatch_notes_add, dispatch_notes_remove, dispatch_notes_update,
    dispatch_ping, dispatch_plan, dispatch_reconcile, dispatch_refresh, dispatch_scout,
    dispatch_status, dispatch_task, dispatch_wait_fresh, dispatch_where,
};
pub(super) use search::{dispatch_search, dispatch_search_legs};

use super::BatchView;
use anyhow::Result;
use std::path::Path;

/// Reset per-thread overlay state and (when requested) validate + stamp the
/// worktree overlay request for this dispatch, from the raw tri-state fields.
///
/// The surface-agnostic core of the search handler's `prepare_overlay_request`,
/// shared so the seed-overlaid graph-adjacent commands (`scout` / `gather` /
/// `task`) honor the same activation precedence and the same
/// security validation as `search`. Called at the TOP of each overlay-capable
/// dispatcher, BEFORE the core runs, on the daemon worker thread that serves
/// the query:
///
/// 1. **`clear_overlay_meta()` unconditionally** — a reused worker thread must
///    not leak the previous query's `_meta.worktree_overlay` into this one.
/// 2. **Validate + stamp `--overlay-root`** when overlay is active (wire flag
///    OR the daemon's own env; `overlay_eligible = false` because default-on is
///    a client-side decision the daemon never makes — it only ever sees an
///    overlay the client forwarded). With no `--overlay-root` the request stays
///    `None` (no-op for this query). A foreign root is rejected as a wire error.
pub(super) fn prepare_overlay_request_fields(
    ctx: &BatchView,
    overlay: bool,
    no_overlay: bool,
    overlay_root: Option<&Path>,
) -> Result<()> {
    cqs::worktree_overlay::clear_overlay_meta();
    let active =
        crate::cli::commands::search::query::resolve_overlay_active(overlay, no_overlay, false);
    if !active {
        return Ok(());
    }
    if let Some(root) = overlay_root {
        // Reject (wire error) if the path is not a worktree of this project.
        ctx.set_validated_overlay_request(root)?;
    } else {
        tracing::debug!(
            "overlay requested but no --overlay-root on the wire — serving parent index"
        );
    }
    Ok(())
}

/// Inject the `_meta.overlay_graph = "seed-only"` marker into a dispatcher's
/// serialized payload (Part A). Called by `scout` / `gather` / `task`
/// when the worktree overlay shadowed the SEED search but the downstream call-
/// graph expansion still reflects parent-truth — the marker makes that honest
/// so a consumer knows the seeds are overlaid while the graph is not.
///
/// Skip-when-absent: only call this when an overlay was actually applied; with
/// no overlay the payload carries no marker. Writes into a reserved top-level
/// `_meta` object the daemon envelope lifts onto the wire `_meta` (sibling of
/// `data`), the same channel `worktree_overlay` and `stale_origins` use; the
/// two coexist (`merged_meta_value` merges the per-response entry with the
/// process-level `worktree_overlay`).
pub(super) fn attach_overlay_graph_meta(value: &mut serde_json::Value) {
    attach_overlay_graph_marker(value, "seed-only");
}

/// Inject the `_meta.overlay_graph = "full"` marker (#1858 Part B). Called by
/// the call-graph dispatchers (`callers` / `callees` / `dead`) when the worktree
/// overlay shadowed the graph query ITSELF and the ENTIRE answer reflects the
/// worktree delta — not just the seed, and not just one section. `dead`'s answer
/// is fully determined by the merged caller graph (no transitive/test sections),
/// so it earns `"full"`. Distinguished from `"seed-only"` so a consumer can tell
/// a fully-overlaid graph answer from a scout/gather answer whose seed was
/// overlaid but whose BFS expansion stayed on parent-truth.
pub(super) fn attach_overlay_graph_meta_full(value: &mut serde_json::Value) {
    attach_overlay_graph_marker(value, "full");
}

/// Inject the `_meta.overlay_graph = "callers-only"` marker (#1858 Part B).
/// Called by `dispatch_impact`: impact's direct-`callers` section reflects the
/// worktree delta, but its affected-tests, transitive-caller, and type-impacted
/// sections stay on parent-truth (a fully-merged call graph is a separate, larger
/// surface). The honest middle value between `"seed-only"` and `"full"` — it tells
/// a consumer exactly which section of the impact answer is overlaid, so the
/// transitive/test sections are NOT mistaken for delta-aware.
pub(super) fn attach_overlay_graph_meta_callers_only(value: &mut serde_json::Value) {
    attach_overlay_graph_marker(value, "callers-only");
}

/// Shared writer for the `_meta.overlay_graph` marker. The three named wrappers
/// pin the only valid values (`"seed-only"`, `"callers-only"`, `"full"`) at their
/// call sites.
fn attach_overlay_graph_marker(value: &mut serde_json::Value, marker: &str) {
    if let Some(obj) = value.as_object_mut() {
        let meta = obj
            .entry("_meta")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(meta_obj) = meta.as_object_mut() {
            meta_obj.insert(
                "overlay_graph".to_string(),
                serde_json::Value::String(marker.to_string()),
            );
        }
    }
}

#[cfg(test)]
mod overlay_marker_tests {
    use super::*;

    /// `dead`'s overlay marker is `"full"` — the candidate-recompute strengthening
    /// keeps the whole answer (caller graph AND candidate section) overlaid, so the
    /// honest marker stays `"full"`, not downgraded to `"callers-only"`. Guards the
    /// exact string `dispatch_dead` attaches on a participating overlay.
    #[test]
    fn dead_overlay_marker_is_full() {
        let mut v = serde_json::json!({"dead": []});
        attach_overlay_graph_meta_full(&mut v);
        assert_eq!(v["_meta"]["overlay_graph"], "full");
    }
}
