//! Index commands — indexing, stats, staleness, garbage collection

mod build;
mod gc;
mod index_args;
mod stale;
mod stats;
mod umap;

pub(crate) use build::{
    build_hnsw_base_index, build_hnsw_index, build_hnsw_index_owned, cmd_index,
    snapshot_fingerprint,
};
pub(crate) use gc::cmd_gc;
// The Phase-0 JsonSchema core for the `cqs_index` MCP tool (Phase 2b). Distinct
// from the clap-side `crate::cli::args::IndexArgs` — this is the non-destructive
// wire slice the bridge advertises and the daemon deserializes.
pub(crate) use index_args::IndexArgs;
pub(crate) use stale::{cmd_stale, stale_core, StaleArgs};
pub(crate) use stats::{cmd_stats, stats_core, StatsArgs};
