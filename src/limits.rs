//! Library-side env-overridable size limits.
//!
//! Mirrors the spirit of `cli/limits.rs` but lives in the library layer so
//! `parser/`, `store/`, `convert/`, and `nl/fts.rs` can read the same env
//! vars. The CLI-side helpers (rerank pool sizing, diff input cap, daemon
//! response cap) stay in `cli/limits.rs` because they have no library
//! callers.
//!
//! Each helper reads its env var on every call. The cost is negligible
//! (one `getenv` syscall per check) and lets tests flip values inside a
//! single process without rebuilding.

// ============ P3 #102: FTS normalization cap ============

/// Default upper bound on `normalize_for_fts` output bytes. Pathological
/// inputs (multi-MB SQL builders, generated parser code, single huge
/// chunks) can otherwise blow the FTS5 indexer's memory budget. See P3
/// #102 in `docs/audit-findings.md`.
pub(crate) const FTS_NORMALIZE_MAX: usize = 16384;

/// Resolve the FTS cap honoring `CQS_FTS_NORMALIZE_MAX`.
pub(crate) fn fts_normalize_max() -> usize {
    parse_env_usize("CQS_FTS_NORMALIZE_MAX", FTS_NORMALIZE_MAX)
}

// ============ P3 #103: graph edge caps ============

/// Default cap on edges loaded into [`crate::store::Store::get_call_graph`].
/// Protects `cqs impact`, `cqs trace`, and `cqs related` from OOM on
/// adversarially-large `function_calls` tables. See P3 #103.
pub(crate) const CALL_GRAPH_MAX_EDGES: usize = 500_000;

/// Default cap on edges loaded into `Store::get_type_graph`.
pub(crate) const TYPE_GRAPH_MAX_EDGES: usize = 500_000;

/// Resolve the call-graph edge cap honoring `CQS_CALL_GRAPH_MAX_EDGES`.
pub(crate) fn call_graph_max_edges() -> usize {
    parse_env_usize("CQS_CALL_GRAPH_MAX_EDGES", CALL_GRAPH_MAX_EDGES)
}

/// Resolve the type-graph edge cap honoring `CQS_TYPE_GRAPH_MAX_EDGES`.
pub(crate) fn type_graph_max_edges() -> usize {
    parse_env_usize("CQS_TYPE_GRAPH_MAX_EDGES", TYPE_GRAPH_MAX_EDGES)
}

// ============ P3 #104, #105: parser size caps ============

/// Default per-file size cap for tree-sitter parsing (50 MiB). Distinct
/// from `CQS_MAX_FILE_SIZE` (file-discovery gate in `lib.rs::max_file_size`)
/// so per-stage knobs stay independent. See P3 #104.
pub(crate) const PARSER_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Default per-chunk byte cap (100 KiB). Chunks larger than this are
/// dropped at parse time before windowing sees them. See P3 #105.
pub(crate) const PARSER_MAX_CHUNK_BYTES: usize = 100_000;

/// Resolve the parser per-file size cap honoring `CQS_PARSER_MAX_FILE_SIZE`.
pub(crate) fn parser_max_file_size() -> u64 {
    parse_env_u64("CQS_PARSER_MAX_FILE_SIZE", PARSER_MAX_FILE_SIZE)
}

/// Resolve the per-chunk byte cap honoring `CQS_PARSER_MAX_CHUNK_BYTES`.
pub(crate) fn parser_max_chunk_bytes() -> usize {
    parse_env_usize("CQS_PARSER_MAX_CHUNK_BYTES", PARSER_MAX_CHUNK_BYTES)
}

// ============ P3 #106, #108: doc converter caps ============

/// Default ceiling on per-archive page count for CHM, web help, and any
/// future multi-page converter. See P3 #106.
pub(crate) const DEFAULT_DOC_MAX_PAGES: usize = 1000;

/// Default `walkdir` depth cap for `convert/mod.rs::convert_directory`.
/// See P3 #108.
pub(crate) const DEFAULT_DOC_MAX_WALK_DEPTH: usize = 50;

/// RM-V1.29-5: per-page byte cap for multi-page converters (CHM, WebHelp).
/// A malicious or pathological archive can ship a single 500MB "page"; the
/// outer `MAX_FILE_SIZE` check only gates the archive, not the extracted
/// pages. Default 10 MiB covers every real-world help page with margin.
pub(crate) const DEFAULT_CONVERT_PAGE_BYTES: u64 = 10 * 1024 * 1024;

/// Resolve the doc page cap honoring `CQS_CONVERT_MAX_PAGES`.
pub(crate) fn doc_max_pages() -> usize {
    parse_env_usize("CQS_CONVERT_MAX_PAGES", DEFAULT_DOC_MAX_PAGES)
}

/// Resolve the walkdir depth cap honoring `CQS_CONVERT_MAX_WALK_DEPTH`.
pub(crate) fn doc_max_walk_depth() -> usize {
    parse_env_usize("CQS_CONVERT_MAX_WALK_DEPTH", DEFAULT_DOC_MAX_WALK_DEPTH)
}

/// RM-V1.29-5: per-page byte cap for CHM / WebHelp page readers, honoring
/// `CQS_CONVERT_PAGE_BYTES`.
pub(crate) fn convert_page_bytes() -> u64 {
    parse_env_u64("CQS_CONVERT_PAGE_BYTES", DEFAULT_CONVERT_PAGE_BYTES)
}

/// SHL-V1.29-10: per-file byte cap for HTML / Markdown converters, honoring
/// `CQS_CONVERT_MAX_FILE_SIZE`. Defaults to 100 MB.
pub(crate) fn convert_file_size() -> u64 {
    const DEFAULT_CONVERT_MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
    parse_env_u64("CQS_CONVERT_MAX_FILE_SIZE", DEFAULT_CONVERT_MAX_FILE_SIZE)
}

// ============ SHL-V1.29-7: hotspot / dead-cluster thresholds ============
//
// `HOTSPOT_MIN_CALLERS`, `DEAD_CLUSTER_MIN_SIZE`, `SUGGEST_HOTSPOT_POOL`
// (`suggest.rs`) and `HEALTH_HOTSPOT_COUNT` (`health.rs`) were hardcoded
// to 5 / 5 / 20 / 5. The same threshold that sensibly flags a 2k-chunk
// hobby project as a "hotspot" is noise on a 500k-chunk monorepo where
// 5-caller functions are everywhere. These helpers scale the defaults
// logarithmically on corpus size (matching the `cagra_itopk_max_default`
// pattern) and accept env overrides that, when set, are taken verbatim.
//
// Formula (chosen so small projects see `5` and big ones see `~20-30`):
//   caller_threshold  = (log₂(n) * 0.7).clamp(5, 50)
//   dead_cluster_min  = (log₂(n) * 0.7).clamp(5, 50)
//   hotspot_count     = (log₂(n) * 1.0).clamp(5, 50)
//   suggest_pool      = 4 * hotspot_count (enough risk headroom)
//
// Rough anchor points:
//   n=1k    → caller=5, hotspot=10, pool=40
//   n=13k   → caller=9, hotspot=14, pool=56
//   n=100k  → caller=11, hotspot=17, pool=68
//   n=1M    → caller=14, hotspot=20, pool=80
//
// Override via env to pin exact values on policy-sensitive projects.

/// Floor for both hotspot caller thresholds and display counts.
const HOTSPOT_THRESHOLD_MIN: usize = 5;
/// Ceiling for both (stops the log scale from running away past usable values).
const HOTSPOT_THRESHOLD_MAX: usize = 50;

/// Log-scale a caller threshold on `chunk_count`.
/// 1k → 5, 13k → 9, 100k → 11, 1M → 14 — capped `[5, 50]`.
fn log_scaled_caller_threshold(chunk_count: usize) -> usize {
    let log2 = (chunk_count.max(1) as f64).log2();
    let scaled = (log2 * 0.7) as usize;
    scaled.clamp(HOTSPOT_THRESHOLD_MIN, HOTSPOT_THRESHOLD_MAX)
}

/// Log-scale a hotspot display count on `chunk_count`.
/// 1k → 10, 13k → 14, 100k → 17, 1M → 20 — capped `[5, 50]`.
fn log_scaled_hotspot_count(chunk_count: usize) -> usize {
    let log2 = (chunk_count.max(1) as f64).log2();
    let scaled = log2 as usize;
    scaled.clamp(HOTSPOT_THRESHOLD_MIN, HOTSPOT_THRESHOLD_MAX)
}

/// Minimum caller count for "untested hotspot" / "high-risk" detectors.
/// Honors `CQS_HOTSPOT_MIN_CALLERS` when set.
pub fn hotspot_min_callers(chunk_count: usize) -> usize {
    parse_env_usize(
        "CQS_HOTSPOT_MIN_CALLERS",
        log_scaled_caller_threshold(chunk_count),
    )
}

/// Minimum dead functions in a single file to flag as a "dead code cluster".
/// Honors `CQS_DEAD_CLUSTER_MIN_SIZE`.
pub fn dead_cluster_min_size(chunk_count: usize) -> usize {
    parse_env_usize(
        "CQS_DEAD_CLUSTER_MIN_SIZE",
        log_scaled_caller_threshold(chunk_count),
    )
}

/// Number of hotspots `health` reports in its summary.
/// Honors `CQS_HEALTH_HOTSPOT_COUNT`.
pub fn health_hotspot_count(chunk_count: usize) -> usize {
    parse_env_usize(
        "CQS_HEALTH_HOTSPOT_COUNT",
        log_scaled_hotspot_count(chunk_count),
    )
}

/// Pool size `suggest` evaluates for risk patterns. Honors
/// `CQS_SUGGEST_HOTSPOT_POOL`; default is 4× the hotspot display count so
/// the risk pass has enough candidates after filtering.
pub fn suggest_hotspot_pool(chunk_count: usize) -> usize {
    let default = (log_scaled_hotspot_count(chunk_count) * 4).clamp(20, 200);
    parse_env_usize("CQS_SUGGEST_HOTSPOT_POOL", default)
}

// ============ SHL-V1.29-8: risk score + blast-radius thresholds ============
//
// `RISK_THRESHOLD_HIGH=5.0`, `RISK_THRESHOLD_MEDIUM=2.0` (`impact/hints.rs`)
// and the blast-radius buckets (`0..=2` Low, `3..=10` Medium, `11+` High)
// drive `cqs review` CI gating. Wrong defaults silently alter classification
// on monorepos, so each is env-overridable.

/// Default risk score above which a function is "High" risk.
pub(crate) const RISK_THRESHOLD_HIGH_DEFAULT: f32 = 5.0;
/// Default risk score above which a function is "Medium" risk.
pub(crate) const RISK_THRESHOLD_MEDIUM_DEFAULT: f32 = 2.0;
/// Default upper bound on caller count for "Low" blast radius.
pub(crate) const BLAST_LOW_MAX_DEFAULT: usize = 2;
/// Default lower bound on caller count for "High" blast radius.
pub(crate) const BLAST_HIGH_MIN_DEFAULT: usize = 11;

/// Resolve the risk High threshold honoring `CQS_RISK_HIGH`.
pub fn risk_threshold_high() -> f32 {
    parse_env_f32("CQS_RISK_HIGH", RISK_THRESHOLD_HIGH_DEFAULT)
}

/// Resolve the risk Medium threshold honoring `CQS_RISK_MEDIUM`.
pub fn risk_threshold_medium() -> f32 {
    parse_env_f32("CQS_RISK_MEDIUM", RISK_THRESHOLD_MEDIUM_DEFAULT)
}

/// Inclusive upper bound for "Low" blast radius (callers `0..=N`).
/// Honors `CQS_BLAST_LOW_MAX`.
pub fn blast_low_max() -> usize {
    parse_env_usize("CQS_BLAST_LOW_MAX", BLAST_LOW_MAX_DEFAULT)
}

/// Inclusive lower bound for "High" blast radius (callers `N..`).
/// Honors `CQS_BLAST_HIGH_MIN`.
pub fn blast_high_min() -> usize {
    parse_env_usize("CQS_BLAST_HIGH_MIN", BLAST_HIGH_MIN_DEFAULT)
}

// ============ shared parsing helpers ============

/// Parse a finite positive `f32`-shaped env var, falling back to
/// `default` on missing/empty/garbage/non-finite/non-positive values.
/// Mirrors `parse_env_usize`'s "reject zero" stance so env-driven risk
/// thresholds can't silently collapse the classification by pinning 0.
fn parse_env_f32(key: &str, default: f32) -> f32 {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
            .unwrap_or(default),
        Err(_) => default,
    }
}

/// Parse a `usize`-shaped env var, falling back to `default` on
/// missing/empty/garbage/zero values.
fn parse_env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(default),
        Err(_) => default,
    }
}

/// Same as [`parse_env_usize`] but for `u64`-shaped byte limits.
fn parse_env_u64(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(v) => v.parse::<u64>().ok().filter(|n| *n > 0).unwrap_or(default),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_usize_handles_missing_and_garbage() {
        std::env::remove_var("CQS_TEST_LIMITS_USZ");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_USZ", "garbage");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_USZ", "0");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_USZ", "42");
        assert_eq!(parse_env_usize("CQS_TEST_LIMITS_USZ", 99), 42);
        std::env::remove_var("CQS_TEST_LIMITS_USZ");
    }

    #[test]
    fn parse_env_u64_handles_missing_and_garbage() {
        std::env::remove_var("CQS_TEST_LIMITS_U64");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_U64", "garbage");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_U64", "0");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 99);
        std::env::set_var("CQS_TEST_LIMITS_U64", "42");
        assert_eq!(parse_env_u64("CQS_TEST_LIMITS_U64", 99), 42);
        std::env::remove_var("CQS_TEST_LIMITS_U64");
    }
}
