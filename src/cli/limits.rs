//! Shared clamp ceilings used by both the CLI dispatchers and the batch
//! dispatchers for parity.
//!
//! # Why this file exists
//!
//! CQ-V1.25-2 (v1.25.0 audit): the CLI path and the batch/daemon path
//! independently clamped `--limit` on several commands, and the two sides
//! drifted out of sync — e.g. `cqs scout` clamped to 10 on the CLI but
//! 50 in the batch handler, so the same query could return a different
//! number of results depending on whether the daemon was up.
//!
//! All callers now clamp via these constants, so updating one value
//! updates both paths atomically.

/// Maximum `--limit` accepted by `cqs scout` and the batch `scout`
/// handler. Scout's downstream grouping and token packing scale roughly
/// linearly in this number, so we keep the ceiling modest.
pub(crate) const SCOUT_LIMIT_MAX: usize = 50;

/// Maximum `--limit` accepted by `cqs similar` and the batch `similar`
/// handler. Similar performs a direct vector query + filter; higher
/// ceilings are safe but rarely useful.
pub(crate) const SIMILAR_LIMIT_MAX: usize = 100;

/// Maximum `--limit` (per category) accepted by `cqs related` and the
/// batch `related` handler. The three categories (callers / callees /
/// types) each get their own top-N, so the total return cap is 3× this.
pub(crate) const RELATED_LIMIT_MAX: usize = 50;
