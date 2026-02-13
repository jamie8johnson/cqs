//! CLI command handlers
//!
//! Each submodule handles one CLI subcommand.

mod audit_mode;
mod context;
#[cfg(feature = "convert")]
mod convert;
mod dead;
mod diff;
mod doctor;
mod explain;
mod gather;
mod gc;
mod graph;
mod impact;
mod impact_diff;
mod index;
mod init;
mod notes;
mod project;
mod query;
mod read;
mod reference;
mod related;
pub(crate) mod resolve;
mod review;
mod scout;
mod similar;
mod stale;
mod stats;
mod test_map;
mod trace;
mod where_cmd;

pub(crate) use audit_mode::cmd_audit_mode;
pub(crate) use context::cmd_context;
#[cfg(feature = "convert")]
pub(crate) use convert::cmd_convert;
pub(crate) use dead::cmd_dead;
pub(crate) use diff::cmd_diff;
pub(crate) use doctor::cmd_doctor;
pub(crate) use explain::cmd_explain;
pub(crate) use gather::cmd_gather;
pub(crate) use gc::cmd_gc;
pub(crate) use graph::{cmd_callees, cmd_callers};
pub(crate) use impact::cmd_impact;
pub(crate) use impact_diff::cmd_impact_diff;
pub(crate) use index::{build_hnsw_index, cmd_index};
pub(crate) use init::cmd_init;
pub(crate) use notes::{cmd_notes, NotesCommand};
pub(crate) use project::{cmd_project, ProjectCommand};
pub(crate) use query::cmd_query;
pub(crate) use read::cmd_read;
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use related::cmd_related;
pub(crate) use review::cmd_review;
pub(crate) use scout::cmd_scout;
pub(crate) use similar::cmd_similar;
pub(crate) use stale::cmd_stale;
pub(crate) use stats::cmd_stats;
pub(crate) use test_map::cmd_test_map;
pub(crate) use trace::cmd_trace;
pub(crate) use where_cmd::cmd_where;

/// Count tokens for text, with fallback estimation on error.
///
/// Used by `--tokens` token-budgeted output across multiple commands.
pub(crate) fn count_tokens(embedder: &cqs::Embedder, text: &str, label: &str) -> usize {
    embedder.token_count(text).unwrap_or_else(|e| {
        tracing::warn!(error = %e, chunk = label, "Token count failed, estimating");
        text.len() / 4
    })
}

/// Batch-count tokens for multiple texts.
///
/// Uses `encode_batch` for better throughput than individual `count_tokens` calls.
/// Falls back to per-text estimation on error.
pub(crate) fn count_tokens_batch(embedder: &cqs::Embedder, texts: &[&str]) -> Vec<usize> {
    embedder.token_counts_batch(texts).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Batch token count failed, estimating per-text");
        texts.iter().map(|t| t.len() / 4).collect()
    })
}
