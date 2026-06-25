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

// ============ FTS normalization cap ============

/// Default upper bound on `normalize_for_fts` output bytes. Pathological
/// inputs (multi-MB SQL builders, generated parser code, single huge
/// chunks) can otherwise blow the FTS5 indexer's memory budget.
pub(crate) const FTS_NORMALIZE_MAX: usize = 16384;

/// Resolve the FTS cap honoring `CQS_FTS_NORMALIZE_MAX`.
pub(crate) fn fts_normalize_max() -> usize {
    parse_env_usize("CQS_FTS_NORMALIZE_MAX", FTS_NORMALIZE_MAX)
}

// ============ graph edge caps ============

/// Default cap on edges loaded into [`crate::store::Store::get_call_graph`].
/// Protects `cqs impact`, `cqs trace`, and `cqs related` from OOM on
/// adversarially-large `function_calls` tables.
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

// ============ parser size caps ============

/// Default per-file size cap for tree-sitter parsing (50 MiB). Distinct
/// from `CQS_MAX_FILE_SIZE` (file-discovery gate in `lib.rs::max_file_size`)
/// so per-stage knobs stay independent.
pub(crate) const PARSER_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Default per-chunk byte cap (100 KiB). Chunks larger than this are
/// dropped at parse time before windowing sees them.
pub(crate) const PARSER_MAX_CHUNK_BYTES: usize = 100_000;

/// Resolve the parser per-file size cap honoring `CQS_PARSER_MAX_FILE_SIZE`.
pub(crate) fn parser_max_file_size() -> u64 {
    parse_env_u64("CQS_PARSER_MAX_FILE_SIZE", PARSER_MAX_FILE_SIZE)
}

/// Resolve the per-chunk byte cap honoring `CQS_PARSER_MAX_CHUNK_BYTES`.
pub(crate) fn parser_max_chunk_bytes() -> usize {
    parse_env_usize("CQS_PARSER_MAX_CHUNK_BYTES", PARSER_MAX_CHUNK_BYTES)
}

/// Default tree-walk recursion-depth ceiling for the parser's recursive
/// relationship-extraction passes (`collect_macro_calls`,
/// `collect_fn_pointer_args`, and their candidate mirrors in
/// `src/parser/calls.rs`). tree-sitter's own parse is iterative and tolerates
/// arbitrarily deep nesting cheaply, but these passes recurse one stack frame
/// per tree level — a deeply-nested macro `token_tree` / parenthesized
/// expression / array literal (which an adversarial indexed file can produce in
/// a few KB) overflows the rayon parser-stage worker stack and SIGSEGV/aborts
/// the whole index/watch/daemon process. 800 is a DoS rail, not a tuning knob:
/// no real source nests anywhere near this (tree-sitter's own grammar limits and
/// human-written code sit in the tens, generated code in the low hundreds), and
/// an 800-deep walk completes inside a 1 MiB worker stack even in an unoptimized
/// debug build (release frames are far leaner), so it can never trip on a
/// legitimate file. Override via `CQS_PARSER_MAX_WALK_DEPTH`. Past the cap the
/// walk stops descending (it does NOT abort the file) — the truncated subtree is
/// the same one whose enclosing chunk would exceed `PARSER_MAX_CHUNK_BYTES` and
/// be dropped anyway, so legitimate output is unchanged.
pub(crate) const PARSER_MAX_WALK_DEPTH: usize = 800;

/// Resolve the recursive tree-walk depth ceiling honoring
/// `CQS_PARSER_MAX_WALK_DEPTH`.
pub(crate) fn parser_max_walk_depth() -> usize {
    parse_env_usize("CQS_PARSER_MAX_WALK_DEPTH", PARSER_MAX_WALK_DEPTH)
}

/// Default worker-thread stack size (bytes) for the dedicated rayon pool the
/// parse stage runs on (2 MiB). The recursive tree-walk is bounded by
/// `PARSER_MAX_WALK_DEPTH` (800), which is sized to complete inside a 1 MiB
/// stack even in an unoptimized debug build; this default doubles that, giving
/// the depth rail headroom rather than relying on the platform/runtime default
/// happening to be large enough. Making the parse pool's stack size explicit
/// turns "the depth rail fits the stack" into a load-bearing-by-design
/// invariant instead of an ambient assumption. Override via
/// `CQS_PARSER_STACK_SIZE`.
pub(crate) const PARSER_STACK_SIZE: usize = 2 * 1024 * 1024;

/// Floor for the parse-pool worker stack (1 MiB) — the size the depth rail
/// (`PARSER_MAX_WALK_DEPTH`) is sized to fit. An operator override below this
/// is clamped up so the walk can never overflow a too-small stack.
pub(crate) const PARSER_STACK_SIZE_MIN: usize = 1024 * 1024;

/// Resolve the parse-pool worker stack size in bytes honoring
/// `CQS_PARSER_STACK_SIZE`, clamped to `[PARSER_STACK_SIZE_MIN, 256 MiB]` so
/// the depth rail always fits and a fat-fingered value can't exhaust address
/// space. Missing/zero/garbage falls back to `PARSER_STACK_SIZE`.
///
/// `pub` so the indexing pipeline (binary crate) can size its dedicated parse
/// thread pool from the same resolver the depth-rail invariant is written
/// against.
pub fn parser_stack_size() -> usize {
    parse_env_usize_clamped(
        "CQS_PARSER_STACK_SIZE",
        PARSER_STACK_SIZE,
        PARSER_STACK_SIZE_MIN,
        256 * 1024 * 1024,
    )
}

/// Default wall-clock budget (milliseconds) for a single tree-sitter parse.
/// tree-sitter has no internal time bound, so an adversarial token stream that
/// drives superlinear error recovery (or a merely huge file) can pin a parser
/// thread for tens of seconds with no cancellation, stalling the index/watch
/// pass. A 5 s budget is enormous relative to a legitimate parse (cqs's own
/// largest source files parse in single-digit milliseconds; even a 1 MiB file
/// is well under a second), so it never trips on real input — it exists purely
/// to abort a pathological parse, which the caller then skips with a warn.
/// Override via `CQS_PARSER_TIMEOUT_MS`. `0` is treated as "no timeout"
/// (the pre-guard behavior) for operators who deliberately want it off.
pub(crate) const PARSER_TIMEOUT_MS: u64 = 5_000;

/// Resolve the per-parse wall-clock budget in milliseconds honoring
/// `CQS_PARSER_TIMEOUT_MS`. Returns `None` when the resolved value is `0`
/// (timeout disabled).
pub(crate) fn parser_timeout_ms() -> Option<u64> {
    // Read directly so an explicit `0` means "disabled" rather than falling
    // back to the default (which `parse_env_u64` would do, since it rejects 0).
    match std::env::var("CQS_PARSER_TIMEOUT_MS") {
        Ok(v) => match v.parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    env = "CQS_PARSER_TIMEOUT_MS",
                    value = %v,
                    "Invalid env var (must be a u64 milliseconds, 0 to disable), using default {PARSER_TIMEOUT_MS}"
                );
                Some(PARSER_TIMEOUT_MS)
            }
        },
        Err(_) => Some(PARSER_TIMEOUT_MS),
    }
}

// ============ file-enumeration walk caps ============

/// Default recursion-depth ceiling for `enumerate_files_iter`'s directory
/// walk. A deeply-nested or symlink-loop-shaped tree (the walk never follows
/// symlinks, but a pathological real directory layout can still nest
/// arbitrarily) would otherwise make the walk descend without bound. 64 is a
/// DoS rail, not a tuning knob — no real source tree nests this deep.
pub(crate) const WALK_MAX_DEPTH: usize = 64;

/// Default cap on files *yielded* by `enumerate_files_iter` (post-filter). A
/// repo with millions of matching files would otherwise make every index /
/// reconcile walk unbounded in time. 500k is generous — large monorepos sit
/// well under it — and exists only to bound an adversarial/pathological tree.
pub(crate) const WALK_MAX_FILES: usize = 500_000;

/// Resolve the walk depth ceiling honoring `CQS_WALK_MAX_DEPTH`.
pub(crate) fn walk_max_depth() -> usize {
    parse_env_usize("CQS_WALK_MAX_DEPTH", WALK_MAX_DEPTH)
}

/// Resolve the yielded-file ceiling honoring `CQS_WALK_MAX_FILES`.
pub(crate) fn walk_max_files() -> usize {
    parse_env_usize("CQS_WALK_MAX_FILES", WALK_MAX_FILES)
}

// ============ doc converter caps ============

/// Default ceiling on per-archive page count for CHM, web help, and any
/// future multi-page converter.
pub(crate) const DEFAULT_DOC_MAX_PAGES: usize = 1000;

/// Default `walkdir` depth cap for `convert/mod.rs::convert_directory`.
pub(crate) const DEFAULT_DOC_MAX_WALK_DEPTH: usize = 50;

/// Per-page byte cap for multi-page converters (CHM, WebHelp).
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

/// Per-page byte cap for CHM / WebHelp page readers, honoring
/// `CQS_CONVERT_PAGE_BYTES`.
pub(crate) fn convert_page_bytes() -> u64 {
    parse_env_u64("CQS_CONVERT_PAGE_BYTES", DEFAULT_CONVERT_PAGE_BYTES)
}

/// Per-file byte cap for HTML / Markdown converters, honoring
/// `CQS_CONVERT_MAX_FILE_SIZE`. Defaults to 100 MB.
pub(crate) fn convert_file_size() -> u64 {
    const DEFAULT_CONVERT_MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
    parse_env_u64("CQS_CONVERT_MAX_FILE_SIZE", DEFAULT_CONVERT_MAX_FILE_SIZE)
}

/// Small-file byte cap for ad-hoc reads of config-shaped files
/// (slot.toml, git hooks, parent-context fallbacks, doc-rewriter sources).
/// Defaults to 4 MiB. Honored via `CQS_SMALL_FILE_MAX_BYTES`. Source files for
/// the doc rewriter and parent-context lookups can legitimately exceed 1 MiB
/// (the file-discovery gate) but should never reach tens of MiB.
pub fn small_file_max_bytes() -> u64 {
    const DEFAULT_SMALL_FILE_MAX: u64 = 4 * 1024 * 1024;
    parse_env_u64("CQS_SMALL_FILE_MAX_BYTES", DEFAULT_SMALL_FILE_MAX)
}

/// Scale a baseline batch size by embedding dim. The baseline is calibrated
/// for a 1024-dim model, so divide by `dim/1024` to keep the per-batch heap
/// footprint roughly constant as the user opts into 2560-dim or 4096-dim
/// presets (qwen3-embedding-{4b,8b}).
///
/// Returns `baseline.clamp(min, max)` if `dim == 0` so callers don't
/// need to special-case zero or unwrap.
///
/// ```text
/// dim=1024 → baseline (no change)
/// dim=2048 → baseline / 2
/// dim=4096 → baseline / 4
/// dim=768  → baseline * 4/3 (slight bump for sub-1024 models)
/// ```
pub fn dim_scaled_batch(baseline: usize, dim: usize, min: usize, max: usize) -> usize {
    if dim == 0 {
        return baseline.clamp(min, max);
    }
    // Multiply first so small `baseline` × small `dim` still has headroom.
    // saturating_mul because callers feed pathological `baseline` from env.
    let scaled = baseline.saturating_mul(1024) / dim;
    scaled.clamp(min, max)
}

/// Stage-1 dense-retrieval candidate pool size for `search_hybrid` /
/// `search_filtered_with_index`. The pool feeds RRF + SPLADE fusion +
/// reranker; it caps the recall ceiling regardless of how many results
/// the operator finally asks for.
///
/// The pool is `limit * 5` with a floor of 500 — gold for the harder
/// queries sits deeper in the dense ranking than 100 candidates allows.
/// `CQS_SEARCH_CANDIDATE_FLOOR` overrides the floor for operators who want
/// to push it further (or lower it on memory-constrained boxes where the
/// 5× HNSW work isn't worth the recall lift).
///
/// `saturating_mul` guards against pathological `limit`: `limit >= usize::MAX / 5`
/// would panic with naive `limit * 5`.
pub fn candidate_count_for(limit: usize) -> usize {
    use std::sync::OnceLock;
    static FLOOR: OnceLock<usize> = OnceLock::new();
    let floor = *FLOOR.get_or_init(|| parse_env_usize("CQS_SEARCH_CANDIDATE_FLOOR", 500));
    limit.saturating_mul(5).max(floor)
}

// ============ hotspot / dead-cluster thresholds ============
//
// The same caller threshold that sensibly flags a 2k-chunk hobby project
// as a "hotspot" is noise on a 500k-chunk monorepo where 5-caller functions
// are everywhere. These helpers scale the defaults logarithmically on corpus
// size and accept env overrides that, when set, are taken verbatim.
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

// ============ risk score + blast-radius thresholds ============
//
// The risk thresholds (High 5.0, Medium 2.0) and the blast-radius buckets
// (`0..=2` Low, `3..=10` Medium, `11+` High) drive `cqs review` CI gating.
// Wrong defaults silently alter classification on monorepos, so each is
// env-overridable.

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
// A single contract for env-driven size limits: missing -> default;
// empty/garbage/zero -> default *with a warn* so an operator who typoed the
// value sees the silent fall-through instead of debugging "why isn't my env
// var doing anything." Zero is treated as invalid by all three helpers —
// call sites that *want* zero to mean "disabled" need to spell it
// (`if value == 0 { return default; }` after a custom parse).

/// Parse a finite positive `f32`-shaped env var, falling back to
/// `default` on missing/empty/garbage/non-finite/non-positive values.
/// Mirrors `parse_env_usize`'s "reject zero" stance so env-driven risk
/// thresholds can't silently collapse the classification by pinning 0.
/// Warns on a malformed-but-set value.
pub fn parse_env_f32(key: &str, default: f32) -> f32 {
    match std::env::var(key) {
        Ok(v) => match v.parse::<f32>() {
            Ok(n) if n.is_finite() && n > 0.0 => n,
            _ => {
                tracing::warn!(
                    env = key,
                    value = %v,
                    "Invalid env var (must be a finite positive number), using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Parse a `usize`-shaped env var, falling back to `default` on
/// missing/empty/garbage/zero values. Warns on a malformed-but-set value.
pub fn parse_env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    env = key,
                    value = %v,
                    "Invalid env var (must be a positive usize), using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Parse a `usize`-shaped env var clamped to `[min, max]`. Out-of-range
/// values are clamped (not rejected) so a misconfigured env var still
/// yields a usable value rather than the unclamped default. Missing/zero/
/// garbage falls back to `default` (also clamped to the range).
///
/// An operator setting an unrealistically large value (e.g.
/// `CQS_SERVE_MAX_CONCURRENT_REQUESTS=4294967295` to try to "disable" the
/// cap) sees a structured warn so the clamp isn't silent.
/// The value still clamps to `max` — operator gets a usable value plus
/// an audit trail.
pub fn parse_env_usize_clamped(key: &str, default: usize, min: usize, max: usize) -> usize {
    let clamp_with_warn = |n: usize, source: &str| {
        let clamped = n.clamp(min, max);
        if n != clamped {
            tracing::warn!(
                env = key,
                value = n,
                clamped,
                min,
                max,
                source,
                "env var clamped to allowed range"
            );
        }
        clamped
    };
    match std::env::var(key) {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => clamp_with_warn(n, "env"),
            _ => clamp_with_warn(default, "default-after-parse-failure"),
        },
        Err(_) => default.clamp(min, max),
    }
}

/// Resolve the LLM batch-submit cap (Anthropic Batches API tops out at
/// 100,000 items per submission; default 10,000 is a safety margin). Env
/// override `CQS_LLM_MAX_BATCH_SIZE` clamped to `[1, 100_000]`.
pub fn llm_max_batch_size() -> usize {
    parse_env_usize_clamped("CQS_LLM_MAX_BATCH_SIZE", 10_000, 1, 100_000)
}

// ============ serve graph/chunk-detail caps ============
//
// `cqs serve`'s DoS-prevention caps. Cytoscape stalls past ~5-10k nodes so
// the default is too high for the UI; power-user queries on a monorepo
// occasionally need more than 50k. Each is env-overridable with a hard
// ceiling so a misconfiguration can't unbound the response.

/// Cap on `/api/graph` nodes. Default 50k. Env: `CQS_SERVE_GRAPH_MAX_NODES`.
pub fn serve_graph_max_nodes() -> usize {
    parse_env_usize_clamped("CQS_SERVE_GRAPH_MAX_NODES", 50_000, 1, 1_000_000)
}

/// Cap on `/api/graph` edges. Default 500k. Env: `CQS_SERVE_GRAPH_MAX_EDGES`.
pub fn serve_graph_max_edges() -> usize {
    parse_env_usize_clamped("CQS_SERVE_GRAPH_MAX_EDGES", 500_000, 1, 10_000_000)
}

/// Cap on `/api/embed/2d` nodes. Default 50k. Env: `CQS_SERVE_CLUSTER_MAX_NODES`.
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

/// Cap on concurrent `spawn_blocking` jobs in `cqs serve`. Default tracks
/// the SQLite connection pool size (default
/// 4 from `CQS_MAX_CONNECTIONS`) so the permit budget can't outrun the pool
/// budget. Env: `CQS_SERVE_BLOCKING_PERMITS`. Bounded to `[1, 1024]` and
/// then clamped at runtime to `<= CQS_MAX_CONNECTIONS`.
///
/// **Why the coupling matters.** Each handler `spawn_blocking` body holds a
/// permit AND borrows a SQLite connection from the pool. If permits > pool
/// connections, panicking handlers can drop their permit while the
/// connection takes longer to return cleanly to the pool — the permit
/// budget says "plenty of headroom" while the pool starves. Capping
/// permits at pool size makes the actual budget the visible one.
///
/// Operators raising both knobs in lockstep (e.g.
/// `CQS_SERVE_BLOCKING_PERMITS=16 CQS_MAX_CONNECTIONS=16`) get the higher
/// concurrency they asked for. Setting only `CQS_SERVE_BLOCKING_PERMITS`
/// without raising `CQS_MAX_CONNECTIONS` clamps to the pool size and
/// emits a one-time warn so the misconfiguration is visible.
pub fn serve_blocking_permits() -> usize {
    let max_connections = std::env::var("CQS_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4);
    let requested = parse_env_usize_clamped("CQS_SERVE_BLOCKING_PERMITS", max_connections, 1, 1024);
    if requested > max_connections {
        tracing::warn!(
            requested,
            max_connections,
            "CQS_SERVE_BLOCKING_PERMITS exceeds CQS_MAX_CONNECTIONS — clamping. \
             Raise CQS_MAX_CONNECTIONS in lockstep to grow the actual concurrency budget. \
             RM-V1.33-10 / #1346"
        );
        max_connections
    } else {
        requested
    }
}

/// Same as [`parse_env_usize`] but for `u64`-shaped byte limits.
/// Warns on a malformed-but-set value.
pub fn parse_env_u64(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(v) => match v.parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    env = key,
                    value = %v,
                    "Invalid env var (must be a positive u64), using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// ============ daemon response cap ============

/// Default 16 MiB cap on a single daemon-to-client response buffer. Bounds the
/// `read_line` allocation against a rogue/buggy daemon while staying large
/// enough for real `gather` / `search` / `task` JSON outputs on big corpora.
///
/// Lives in the library `limits` module (not `cli::limits`) because BOTH the
/// binary CLI daemon-forward path AND the library-side MCP relay
/// (`daemon_translate::daemon_json_args_request`) size their read cap from it —
/// a single source of truth so the relay can never be narrower than the CLI it
/// fronts. `cli::limits::max_daemon_response_bytes` delegates here.
pub const MAX_DAEMON_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve the daemon response cap honoring `CQS_DAEMON_MAX_RESPONSE_BYTES`.
pub fn max_daemon_response_bytes() -> u64 {
    parse_env_u64("CQS_DAEMON_MAX_RESPONSE_BYTES", MAX_DAEMON_RESPONSE_BYTES)
}

// ============ cqs serve idle eviction ============

/// Default idle-shutdown threshold for `cqs serve` in minutes. After this
/// many minutes of no incoming requests, the server shuts down gracefully
/// to release the `Store<ReadOnly>` mmap, the spawn-blocking semaphore,
/// and the tokio runtime. `0` disables the idle shutdown entirely.
pub const SERVE_IDLE_MINUTES_DEFAULT: u64 = 30;

// ============ cqs serve concurrent-request cap ============

/// Default ceiling on concurrent in-flight requests in `cqs serve`. Sized
/// for an interactive single-user UI plus a generous buffer for fan-out
/// from agent tooling. Each request can hold up to a 64 KiB body buffer
/// pre-auth (RequestBodyLimitLayer) plus per-handler scratch state, so
/// 256 × 64 KiB = 16 MiB worst-case pre-auth memory ceiling — well below
/// any sensible memory budget. Bound only by FD limit otherwise.
pub const SERVE_MAX_CONCURRENT_REQUESTS_DEFAULT: usize = 256;

/// Resolve the in-flight request cap for `cqs serve`.
///
/// The request body limit is per-request; without an outer cap, an attacker
/// on `--bind 0.0.0.0` (or any LAN/--no-auth bind)
/// can fan out N concurrent connections, each holding a 64 KiB pre-auth
/// buffer. Bounded only by FD limit. The cap below sits on the outermost
/// middleware layer (`enforce_concurrency_cap` in `src/serve/mod.rs`) and
/// uses `try_acquire` so saturation returns `503 Service Unavailable`
/// instantly — no queueing, no allocation.
///
/// `CQS_SERVE_MAX_CONCURRENT_REQUESTS` overrides per-launch, clamped to
/// `[1, 8192]`. Tests can drive saturation by setting this to `1`.
pub fn serve_max_concurrent_requests() -> usize {
    parse_env_usize_clamped(
        "CQS_SERVE_MAX_CONCURRENT_REQUESTS",
        SERVE_MAX_CONCURRENT_REQUESTS_DEFAULT,
        1,
        8192,
    )
}

/// Resolve the idle-shutdown threshold for `cqs serve` in minutes.
/// `CQS_SERVE_IDLE_MINUTES=0` disables idle eviction (server runs until
/// killed). Garbage / missing values fall back to
/// [`SERVE_IDLE_MINUTES_DEFAULT`].
///
/// `0` is the only special value: any other parseable u64 wins (no upper
/// clamp — operators may legitimately want a multi-day idle window for a
/// dashboard left open across a weekend).
pub fn serve_idle_minutes() -> u64 {
    match std::env::var("CQS_SERVE_IDLE_MINUTES") {
        Ok(v) => v.parse::<u64>().unwrap_or(SERVE_IDLE_MINUTES_DEFAULT),
        Err(_) => SERVE_IDLE_MINUTES_DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Clear every env var the four `serve_blocking_permits` tests mutate so a
    /// run with leftover state from a crashed earlier test still starts clean.
    fn clear_serve_blocking_env() {
        std::env::remove_var("CQS_SERVE_BLOCKING_PERMITS");
        std::env::remove_var("CQS_MAX_CONNECTIONS");
    }

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

    /// All four `serve_blocking_permits_*` tests mutate the same process-global
    /// `CQS_SERVE_BLOCKING_PERMITS` / `CQS_MAX_CONNECTIONS` env vars. cqs CI
    /// runs lib tests in parallel, so a sibling test's env value can leak
    /// between this test's `remove_var` and its `serve_blocking_permits()`
    /// call. `#[serial]` from the `serial_test` crate forces a process-wide
    /// mutex around the cohort.
    #[test]
    #[serial]
    fn serve_blocking_permits_defaults_to_max_connections_default() {
        clear_serve_blocking_env();
        assert_eq!(serve_blocking_permits(), 4);
    }

    #[test]
    #[serial]
    fn serve_blocking_permits_tracks_max_connections_when_unset() {
        clear_serve_blocking_env();
        std::env::set_var("CQS_MAX_CONNECTIONS", "8");
        assert_eq!(serve_blocking_permits(), 8);
        clear_serve_blocking_env();
    }

    #[test]
    #[serial]
    fn serve_blocking_permits_respects_explicit_when_under_max_connections() {
        clear_serve_blocking_env();
        std::env::set_var("CQS_SERVE_BLOCKING_PERMITS", "2");
        std::env::set_var("CQS_MAX_CONNECTIONS", "8");
        assert_eq!(serve_blocking_permits(), 2);
        clear_serve_blocking_env();
    }

    #[test]
    #[serial]
    fn serve_blocking_permits_clamps_above_max_connections() {
        clear_serve_blocking_env();
        std::env::set_var("CQS_SERVE_BLOCKING_PERMITS", "32");
        std::env::set_var("CQS_MAX_CONNECTIONS", "4");
        // Clamps to max_connections so the permit budget can't outrun the
        // SQLite pool budget.
        assert_eq!(serve_blocking_permits(), 4);
        clear_serve_blocking_env();
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

    // ============ parse_env_usize_clamped + parse_env_f32 ====

    /// Above-max value clamps to max (not "use default").
    #[test]
    fn parse_env_usize_clamped_clamps_above_max() {
        let key = "CQS_TEST_CLAMP_ABOVE_MAX";
        std::env::set_var(key, "9999999");
        // default=10, range [1, 100] — 9999999 must clamp to 100.
        assert_eq!(parse_env_usize_clamped(key, 10, 1, 100), 100);
        std::env::remove_var(key);
    }

    /// Below-min value clamps to min.
    #[test]
    fn parse_env_usize_clamped_clamps_below_min() {
        let key = "CQS_TEST_CLAMP_BELOW_MIN";
        // Note: parse_env_usize_clamped requires `n > 0` to not fall back
        // to default — pick min=5 so 1 is parseable but below-floor.
        std::env::set_var(key, "1");
        assert_eq!(parse_env_usize_clamped(key, 10, 5, 100), 5);
        std::env::remove_var(key);
    }

    /// Garbage value triggers fallback to (clamped) default.
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

    /// NaN string for `parse_env_f32` falls back to default
    /// (`is_finite` filter rejects NaN before the unwrap_or).
    #[test]
    fn parse_env_f32_rejects_nan() {
        let key = "CQS_TEST_F32_NAN";
        std::env::set_var(key, "NaN");
        assert_eq!(parse_env_f32(key, 0.5), 0.5);
        std::env::remove_var(key);
    }

    /// `parse_env_f32` rejects negative and zero — the
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

    /// Pin that `parse_env_usize_clamped` zero-input falls back to
    /// `clamp(default)` rather than `0`. The helper's
    /// `n > 0` filter is load-bearing — a future refactor moving the
    /// check would silently produce 0, which would cause divide-by-zero
    /// in callers like embed_batch_size. Also covers the
    /// "default below min → use min" defensive contract.
    #[test]
    fn parse_env_usize_clamped_zero_input_uses_clamped_default() {
        let key = "CQS_TEST_USIZE_ZERO";
        std::env::set_var(key, "0");
        assert_eq!(
            parse_env_usize_clamped(key, 50, 1, 100),
            50,
            "explicit zero falls back to default (clamped)"
        );
        std::env::remove_var(key);
        // Missing var also falls through to clamped default.
        assert_eq!(parse_env_usize_clamped("CQS_TEST_USIZE_NX", 50, 1, 100), 50);
    }

    #[test]
    fn parse_env_usize_clamped_default_below_min_clamps_up() {
        // Misconfigured caller passes default=0, min=1 — must return min,
        // not 0. Without the clamp(default) we'd return 0 and crash on
        // division.
        std::env::remove_var("CQS_TEST_USIZE_BELOW_MIN_DEFAULT");
        assert_eq!(
            parse_env_usize_clamped("CQS_TEST_USIZE_BELOW_MIN_DEFAULT", 0, 1, 100),
            1
        );
    }

    // ===== candidate_count_for tests =====

    /// `candidate_count_for` must respect the floor when the linear ramp
    /// (`limit * 5`) is below it. Default floor is 500.
    #[test]
    fn candidate_count_floor_is_at_least_500() {
        // limit=20 (eval default) → linear=100, floor=500 → 500
        let pool = candidate_count_for(20);
        assert!(
            pool >= 500,
            "candidate_count for limit=20 must hit the >=500 floor, got {pool}"
        );
        // limit=5 (search default) → linear=25, floor=500 → 500
        let small = candidate_count_for(5);
        assert!(
            small >= 500,
            "candidate_count for limit=5 must hit the >=500 floor, got {small}"
        );
    }

    /// Above the floor the pool grows linearly with `limit * 5`.
    #[test]
    fn candidate_count_scales_linearly_above_floor() {
        // limit=200 → linear=1000, floor=500 → 1000 (linear wins)
        assert_eq!(candidate_count_for(200), 1000);
        // limit=500 → linear=2500
        assert_eq!(candidate_count_for(500), 2500);
    }

    /// Saturating-multiply guard: `limit >= usize::MAX/5` must NOT panic.
    #[test]
    fn candidate_count_saturating_does_not_panic_on_huge_limit() {
        let pool = candidate_count_for(usize::MAX / 4);
        assert_eq!(pool, usize::MAX, "saturating_mul must clamp at usize::MAX");
    }

    /// Pin the `dim_scaled_batch` formula at the dim values we actually ship
    /// presets for. Regressions surface here, not at runtime.
    #[test]
    fn dim_scaled_batch_at_baseline_dim_returns_baseline() {
        // dim == 1024 = baseline assumption → baseline unchanged.
        assert_eq!(dim_scaled_batch(10_000, 1024, 500, 50_000), 10_000);
        assert_eq!(dim_scaled_batch(5_000, 1024, 500, 50_000), 5_000);
    }

    #[test]
    fn dim_scaled_batch_halves_at_2048_dim() {
        // qwen3-embedding-4b is 2560-dim; pin the 2048 case as a reference.
        assert_eq!(dim_scaled_batch(10_000, 2048, 500, 50_000), 5_000);
    }

    #[test]
    fn dim_scaled_batch_quarters_at_4096_dim() {
        // qwen3-embedding-8b is 4096-dim.
        assert_eq!(dim_scaled_batch(10_000, 4096, 500, 50_000), 2_500);
    }

    #[test]
    fn dim_scaled_batch_grows_at_768_dim() {
        // E5-base / nomic-coderank / embeddinggemma-300m all 768-dim.
        // baseline * 1024 / 768 = 13_333 (integer).
        assert_eq!(dim_scaled_batch(10_000, 768, 500, 50_000), 13_333);
    }

    #[test]
    fn dim_scaled_batch_clamps_to_min_on_huge_dim() {
        // hypothetical 65536-dim → 156 → clamp to min 500.
        assert_eq!(dim_scaled_batch(10_000, 65_536, 500, 50_000), 500);
    }

    #[test]
    fn dim_scaled_batch_clamps_to_max_on_tiny_dim() {
        // hypothetical 64-dim → 160_000 → clamp to max 50_000.
        assert_eq!(dim_scaled_batch(10_000, 64, 500, 50_000), 50_000);
    }

    #[test]
    fn dim_scaled_batch_zero_dim_returns_clamped_baseline() {
        // Defensive: zero dim must not trigger div-by-zero panic.
        assert_eq!(dim_scaled_batch(10_000, 0, 500, 50_000), 10_000);
        assert_eq!(dim_scaled_batch(50, 0, 500, 50_000), 500); // clamp up
        assert_eq!(dim_scaled_batch(99_999, 0, 500, 50_000), 50_000); // clamp down
    }

    // ===== parser_stack_size tests =====

    /// Unset env → the 2 MiB default. The parse pool's worker stack must be at
    /// least the 1 MiB the depth rail is sized to fit; the default doubles that.
    #[test]
    #[serial]
    fn parser_stack_size_default_is_2mib() {
        std::env::remove_var("CQS_PARSER_STACK_SIZE");
        assert_eq!(parser_stack_size(), PARSER_STACK_SIZE);
        assert_eq!(parser_stack_size(), 2 * 1024 * 1024);
    }

    /// A valid override is honored verbatim (within the clamp range).
    #[test]
    #[serial]
    fn parser_stack_size_env_override_honored() {
        std::env::set_var("CQS_PARSER_STACK_SIZE", (8 * 1024 * 1024).to_string());
        assert_eq!(parser_stack_size(), 8 * 1024 * 1024);
        std::env::remove_var("CQS_PARSER_STACK_SIZE");
    }

    /// A below-floor override clamps UP to the depth-rail minimum (1 MiB), so an
    /// operator can never shrink the stack below what the 800-deep walk needs.
    #[test]
    #[serial]
    fn parser_stack_size_below_floor_clamps_to_min() {
        std::env::set_var("CQS_PARSER_STACK_SIZE", (256 * 1024).to_string());
        assert_eq!(parser_stack_size(), PARSER_STACK_SIZE_MIN);
        assert_eq!(parser_stack_size(), 1024 * 1024);
        std::env::remove_var("CQS_PARSER_STACK_SIZE");
    }

    /// Garbage / zero override falls back to the (clamped) default.
    #[test]
    #[serial]
    fn parser_stack_size_garbage_uses_default() {
        std::env::set_var("CQS_PARSER_STACK_SIZE", "not_a_number");
        assert_eq!(parser_stack_size(), PARSER_STACK_SIZE);
        std::env::set_var("CQS_PARSER_STACK_SIZE", "0");
        assert_eq!(parser_stack_size(), PARSER_STACK_SIZE);
        std::env::remove_var("CQS_PARSER_STACK_SIZE");
    }
}
