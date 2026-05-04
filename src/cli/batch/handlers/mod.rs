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

// TC-HAP-1.29-2: smoke tests for 11 of the 16 dispatch handlers named in
// the audit (the remaining 5 require embedder cold-load and are intentionally
// skipped — see module docs).
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
    dispatch_diff, dispatch_drift, dispatch_gather, dispatch_gc, dispatch_help, dispatch_notes,
    dispatch_ping, dispatch_plan, dispatch_reconcile, dispatch_refresh, dispatch_scout,
    dispatch_status, dispatch_task, dispatch_wait_fresh, dispatch_where,
};
pub(super) use search::dispatch_search;
