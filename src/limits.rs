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
//
// P2.4: these helpers were previously module-private. The same
// `env::var(...).parse().ok().filter(...).unwrap_or(default)` pattern was
// duplicated across 25+ sites (`watch.rs`, `llm/mod.rs`, `pipeline/types.rs`,
// `hnsw/persist.rs`, `embedder/`, `cache.rs`, `gather.rs`,
// `commands/graph/trace.rs`, `impact/bfs.rs`, `reranker.rs`) with subtle
// drift in zero-handling. They are now `pub` so any module can route through
// a single contract: missing/empty/garbage/zero -> default, otherwise the
// parsed value. Behavioral note: zero is treated as invalid by all three
// helpers — call sites that *want* zero to mean "disabled" need to spell it
// (`if value == 0 { return default; }` after a custom parse).

/// Parse a finite positive `f32`-shaped env var, falling back to
/// `default` on missing/empty/garbage/non-finite/non-positive values.
/// Mirrors `parse_env_usize`'s "reject zero" stance so env-driven risk
/// thresholds can't silently collapse the classification by pinning 0.
pub fn parse_env_f32(key: &str, default: f32) -> f32 {
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
pub fn parse_env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(default),
        Err(_) => default,
    }
}

/// Parse a `usize`-shaped env var clamped to `[min, max]`. Out-of-range
/// values are clamped (not rejected) so a misconfigured env var still
/// yields a usable value rather than the unclamped default. Missing/zero/
/// garbage falls back to `default` (also clamped to the range).
pub fn parse_env_usize_clamped(key: &str, default: usize, min: usize, max: usize) -> usize {
    let clamp = |n: usize| n.clamp(min, max);
    match std::env::var(key) {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => clamp(n),
            _ => clamp(default),
        },
        Err(_) => clamp(default),
    }
}

/// P2.39 — Resolve the LLM batch-submit cap (Anthropic Batches API tops out
/// at 100,000 items per submission; default 10,000 is a safety margin). Env
/// override `CQS_LLM_MAX_BATCH_SIZE` clamped to `[1, 100_000]`.
pub fn llm_max_batch_size() -> usize {
    parse_env_usize_clamped("CQS_LLM_MAX_BATCH_SIZE", 10_000, 1, 100_000)
}

// ============ P2.40 — serve graph/chunk-detail caps ============
//
// `cqs serve` previously hardcoded its DoS-prevention caps in `serve/data.rs`
// (`ABS_MAX_GRAPH_NODES=50_000`, `ABS_MAX_GRAPH_EDGES=500_000`,
// `ABS_MAX_CLUSTER_NODES=50_000`, plus per-list `LIMIT 50/50/20` inside
// `build_chunk_detail`). Cytoscape stalls past ~5-10k nodes so the
// default is too high for the UI; power-user queries on a monorepo
// occasionally need more than 50k. Each is now env-overridable with a
// hard ceiling so a misconfiguration can't unbound the response.

/// SEC-3 cap on `/api/graph` nodes. Default 50k. Env: `CQS_SERVE_GRAPH_MAX_NODES`.
pub fn serve_graph_max_nodes() -> usize {
    parse_env_usize_clamped("CQS_SERVE_GRAPH_MAX_NODES", 50_000, 1, 1_000_000)
}

/// SEC-3 cap on `/api/graph` edges. Default 500k. Env: `CQS_SERVE_GRAPH_MAX_EDGES`.
pub fn serve_graph_max_edges() -> usize {
    parse_env_usize_clamped("CQS_SERVE_GRAPH_MAX_EDGES", 500_000, 1, 10_000_000)
}

/// SEC-3 cap on `/api/embed/2d` nodes. Default 50k. Env: `CQS_SERVE_CLUSTER_MAX_NODES`.
pub fn serve_cluster_max_nodes() -> usize {
    parse_env_usize_clamped("CQS_SERVE_CLUSTER_MAX_NODES", 50_000, 1, 1_000_000)
}

/// `build_chunk_detail` per-list LIMIT for callers. Default 50.
/// Env: `CQS_SERVE_CHUNK_DETAIL_CALLERS`.
pub fn serve_chunk_detail_callers_limit() -> usize {
    parse_env_usize_clamped("CQS_SERVE_CHUNK_DETAIL_CALLERS", 50, 1, 1_000)
}

/// `build_chunk_detail` per-list LIMIT for callees. Default 50.
/// Env: `CQS_SERVE_CHUNK_DETAIL_CALLEES`.
pub fn serve_chunk_detail_callees_limit() -> usize {
    parse_env_usize_clamped("CQS_SERVE_CHUNK_DETAIL_CALLEES", 50, 1, 1_000)
}

/// `build_chunk_detail` per-list LIMIT for tests-that-cover. Default 20.
/// Env: `CQS_SERVE_CHUNK_DETAIL_TESTS`.
pub fn serve_chunk_detail_tests_limit() -> usize {
    parse_env_usize_clamped("CQS_SERVE_CHUNK_DETAIL_TESTS", 20, 1, 1_000)
}

/// P2.76 — cap on concurrent `spawn_blocking` jobs in `cqs serve`. Default 32.
/// Env: `CQS_SERVE_BLOCKING_PERMITS`. Bounded to `[1, 1024]`.
pub fn serve_blocking_permits() -> usize {
    parse_env_usize_clamped("CQS_SERVE_BLOCKING_PERMITS", 32, 1, 1024)
}

/// Same as [`parse_env_usize`] but for `u64`-shaped byte limits.
pub fn parse_env_u64(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(v) => v.parse::<u64>().ok().filter(|n| *n > 0).unwrap_or(default),
        Err(_) => default,
    }
}

// ============ #1182 wait_for_fresh poll cadence ============

/// Default initial poll interval (milliseconds) for `wait_for_fresh`. The
/// poll loop starts here and doubles up to a 2 s ceiling. 100 ms is fast
/// enough that an already-fresh tree returns within a tick, slow enough
/// that 600 s × 100 ms ≈ 6000 connect/parse round-trips can't pin a
/// host's socket budget on a stuck-stale daemon.
///
/// SHL-V1.30-2: env override `CQS_FRESHNESS_POLL_MS` clamped to
/// `[25, 5000]` so a misconfigured `=1` doesn't burn CPU and `=60000`
/// doesn't masquerade as a hang.
pub const FRESHNESS_POLL_MS_INITIAL_DEFAULT: u64 = 100;

/// Resolve the initial poll interval honoring `CQS_FRESHNESS_POLL_MS`.
/// Floor 25 ms, ceiling 5000 ms. See [`FRESHNESS_POLL_MS_INITIAL_DEFAULT`].
pub fn freshness_poll_ms_initial() -> u64 {
    match std::env::var("CQS_FRESHNESS_POLL_MS") {
        Ok(v) => v
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(|n| n.clamp(25, 5000))
            .unwrap_or(FRESHNESS_POLL_MS_INITIAL_DEFAULT),
        Err(_) => FRESHNESS_POLL_MS_INITIAL_DEFAULT,
    }
}

/// Parse a duration-in-seconds env var into a `std::time::Duration`,
/// falling back to `default_secs` on missing/empty/garbage/zero values.
/// P2.4: convenience wrapper for the common `parse_env_u64(...) -> from_secs`
/// pattern used by serve/watch timeouts.
///
/// Marked `#[allow(dead_code)]` because the call sites that will consume it
/// (`serve` shutdown grace, `watch` debounce ceiling) live in agent-D-owned
/// modules and will land in a follow-up. The helper is parked here to keep
/// the env-var contract centralized.
#[allow(dead_code)]
pub fn parse_env_duration_secs(key: &str, default_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs(parse_env_u64(key, default_secs))
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

    /// SHL-V1.30-2: `freshness_poll_ms_initial` honors the env override and
    /// clamps to `[25, 5000]`. Missing / garbage / zero falls back to the
    /// 100 ms default.
    #[test]
    fn freshness_poll_ms_initial_default_and_clamp() {
        // Default path
        std::env::remove_var("CQS_FRESHNESS_POLL_MS");
        assert_eq!(freshness_poll_ms_initial(), 100);
        // Garbage / zero falls back to default
        std::env::set_var("CQS_FRESHNESS_POLL_MS", "garbage");
        assert_eq!(freshness_poll_ms_initial(), 100);
        std::env::set_var("CQS_FRESHNESS_POLL_MS", "0");
        assert_eq!(freshness_poll_ms_initial(), 100);
        // Below floor → clamped to 25
        std::env::set_var("CQS_FRESHNESS_POLL_MS", "1");
        assert_eq!(freshness_poll_ms_initial(), 25);
        // Above ceiling → clamped to 5000
        std::env::set_var("CQS_FRESHNESS_POLL_MS", "60000");
        assert_eq!(freshness_poll_ms_initial(), 5000);
        // In-range value passes through
        std::env::set_var("CQS_FRESHNESS_POLL_MS", "250");
        assert_eq!(freshness_poll_ms_initial(), 250);
        std::env::remove_var("CQS_FRESHNESS_POLL_MS");
    }

    // ============ TC-ADV-V1.33-7: parse_env_usize_clamped + parse_env_f32 ====

    /// TC-ADV-V1.33-7: above-max value clamps to max (not "use default").
    #[test]
    fn parse_env_usize_clamped_clamps_above_max() {
        let key = "CQS_TEST_CLAMP_ABOVE_MAX";
        std::env::set_var(key, "9999999");
        // default=10, range [1, 100] — 9999999 must clamp to 100.
        assert_eq!(parse_env_usize_clamped(key, 10, 1, 100), 100);
        std::env::remove_var(key);
    }

    /// TC-ADV-V1.33-7: below-min value clamps to min.
    #[test]
    fn parse_env_usize_clamped_clamps_below_min() {
        let key = "CQS_TEST_CLAMP_BELOW_MIN";
        // Note: parse_env_usize_clamped requires `n > 0` to not fall back
        // to default — pick min=5 so 1 is parseable but below-floor.
        std::env::set_var(key, "1");
        assert_eq!(parse_env_usize_clamped(key, 10, 5, 100), 5);
        std::env::remove_var(key);
    }

    /// TC-ADV-V1.33-7: garbage value triggers fallback to (clamped) default.
    /// Out-of-range default also gets clamped — pin the "default also
    /// passes through clamp()" path documented in the helper.
    #[test]
    fn parse_env_usize_clamped_garbage_uses_clamped_default() {
        let key = "CQS_TEST_CLAMP_GARBAGE";
        std::env::set_var(key, "not_a_number");
        // default=200 is above max=100 — clamp must apply to the default
        // too, returning 100 not 200.
        assert_eq!(parse_env_usize_clamped(key, 200, 1, 100), 100);
        std::env::remove_var(key);
    }

    /// TC-ADV-V1.33-7: NaN string for `parse_env_f32` falls back to default
    /// (`is_finite` filter rejects NaN before the unwrap_or).
    #[test]
    fn parse_env_f32_rejects_nan() {
        let key = "CQS_TEST_F32_NAN";
        std::env::set_var(key, "NaN");
        assert_eq!(parse_env_f32(key, 0.5), 0.5);
        std::env::remove_var(key);
    }

    /// TC-ADV-V1.33-7: `parse_env_f32` rejects negative and zero — the
    /// helper's `*n > 0.0` filter is load-bearing for risk thresholds
    /// (a 0.0 threshold would silently collapse the classification).
    #[test]
    fn parse_env_f32_rejects_negative_and_zero() {
        let key = "CQS_TEST_F32_NEG";
        std::env::set_var(key, "-1.5");
        assert_eq!(parse_env_f32(key, 0.5), 0.5, "negative falls back");
        std::env::set_var(key, "0.0");
        assert_eq!(parse_env_f32(key, 0.5), 0.5, "zero falls back");
        std::env::set_var(key, "0");
        assert_eq!(parse_env_f32(key, 0.5), 0.5, "integer zero falls back");
        std::env::remove_var(key);
    }
}
