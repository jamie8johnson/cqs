//! Shared clamp ceilings and env-overridable size limits for the CLI and
//! batch dispatchers.
//!
//! # Why this file exists
//!
//! The CLI path and the batch/daemon path both clamp `--limit` on several
//! commands; without a shared source the two sides drift (e.g. `cqs scout`
//! clamped to 10 on the CLI but 50 in the batch handler returns a different
//! result count depending on whether the daemon is up). All callers clamp via
//! these constants, so updating one value updates both paths atomically.
//!
//! Magic-number caps scattered across the CLI layer (rerank pool, stdin/diff
//! caps, display/read file size caps, daemon response cap) are collected here
//! so they share a single env-override pattern. Library-level caps (parser,
//! FTS, graph, converter) live in `crate::limits` because their callers
//! (`parser/`, `store/`, `nl/`, `convert/`) cannot reach into the CLI module.

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

// ============ reranker pool sizing ============

/// Default over-retrieval multiplier for the cross-encoder reranker.
/// At `--rerank --limit N` we send `N * MULTIPLIER` candidates through
/// stage 1 so the reranker has enough recall headroom to surface the
/// right result. Honored by [`rerank_pool_size`].
pub(crate) const RERANK_OVER_RETRIEVAL_MULTIPLIER: usize = 4;

/// Default hard cap on the reranker pool, regardless of multiplier.
/// At `--limit 50 --rerank` the multiplier alone would yield 200 — the
/// cap keeps ORT memory and per-batch latency bounded on small GPUs.
///
/// Weak cross-encoders degrade monotonically with pool size — at 80
/// candidates they're just shuffling noise. "Drowning in Documents" (arXiv
/// 2411.11767) reports the same for off-the-shelf cross-encoders; small pools
/// (~20) consistently beat large ones on recall@k.
///
/// Honored by [`rerank_pool_size`].
pub(crate) const RERANK_POOL_MAX: usize = 20;

/// Process-global cache of the resolved `[reranker] pool_max` and
/// `over_retrieval` TOML overrides. Set once at dispatch entry via
/// [`install_reranker_pool_overrides`]; consulted by the resolvers below as a
/// fallback between env and the compiled default.
static RERANKER_POOL_OVERRIDES: std::sync::OnceLock<RerankerPoolOverrides> =
    std::sync::OnceLock::new();

#[derive(Default, Debug, Clone, Copy)]
struct RerankerPoolOverrides {
    pool_max: Option<usize>,
    over_retrieval: Option<usize>,
}

/// Install the `[reranker]` pool-max + over-retrieval overrides (read
/// once from `.cqs.toml`) into the process-global cache. Called from
/// `cli::dispatch` after `Config::load`. Idempotent — first writer wins,
/// second silently no-ops (matches the OnceLock contract used by the
/// search/router overlay installers).
pub(crate) fn install_reranker_pool_overrides(
    pool_max: Option<usize>,
    over_retrieval: Option<usize>,
) {
    let _ = RERANKER_POOL_OVERRIDES.set(RerankerPoolOverrides {
        pool_max,
        over_retrieval,
    });
}

fn reranker_pool_overrides() -> RerankerPoolOverrides {
    RERANKER_POOL_OVERRIDES.get().copied().unwrap_or_default()
}

/// Resolve the over-retrieval multiplier honoring (in priority):
///   1. `CQS_RERANK_OVER_RETRIEVAL` env var (operator override).
///   2. `[reranker] over_retrieval = N` in `.cqs.toml` (durable config).
///   3. [`RERANK_OVER_RETRIEVAL_MULTIPLIER`] compile-time default.
///
/// Zero is rejected at every layer so a misconfigured value can't silently
/// degrade reranking to single-candidate mode.
pub(crate) fn rerank_over_retrieval_multiplier() -> usize {
    if std::env::var_os("CQS_RERANK_OVER_RETRIEVAL").is_some() {
        return parse_env_usize(
            "CQS_RERANK_OVER_RETRIEVAL",
            RERANK_OVER_RETRIEVAL_MULTIPLIER,
        );
    }
    reranker_pool_overrides()
        .over_retrieval
        .filter(|n| *n > 0)
        .unwrap_or(RERANK_OVER_RETRIEVAL_MULTIPLIER)
}

/// Resolve the reranker pool cap honoring (in priority):
///   1. `CQS_RERANK_POOL_MAX` env var (operator override).
///   2. `[reranker] pool_max = N` in `.cqs.toml` (durable config).
///   3. [`RERANK_POOL_MAX`] compile-time default.
pub(crate) fn rerank_pool_max() -> usize {
    if std::env::var_os("CQS_RERANK_POOL_MAX").is_some() {
        return parse_env_usize("CQS_RERANK_POOL_MAX", RERANK_POOL_MAX);
    }
    reranker_pool_overrides()
        .pool_max
        .filter(|n| *n > 0)
        .unwrap_or(RERANK_POOL_MAX)
}

/// Compute the over-retrieval pool size for a given user-facing limit.
/// Used by the four production rerank call sites (CLI query, CLI ref-only
/// query, batch search, batch ref search) so the policy lives in one place.
pub(crate) fn rerank_pool_size(user_limit: usize) -> usize {
    user_limit
        .saturating_mul(rerank_over_retrieval_multiplier())
        .min(rerank_pool_max())
}

// ============ stdin/diff/display/read file caps ============

/// Default 50 MiB cap on stdin diff input (`cqs impact --diff`,
/// `cqs review --stdin`) and on `git diff` subprocess stdout.
pub(crate) const MAX_DIFF_BYTES: usize = 50 * 1024 * 1024;

/// Default 10 MiB cap on file size for `display::read_context_lines`
/// (snippet extraction for search results).
pub(crate) const MAX_DISPLAY_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Default 10 MiB cap on file size for `cqs read` (full-file reads with
/// note injection). Distinct from the display cap because `cqs read`
/// emits the entire file body, not just a snippet.
pub(crate) const READ_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Resolve the diff input cap honoring `CQS_MAX_DIFF_BYTES`.
pub(crate) fn max_diff_bytes() -> usize {
    parse_env_usize("CQS_MAX_DIFF_BYTES", MAX_DIFF_BYTES)
}

/// Resolve the display file-size cap honoring `CQS_MAX_DISPLAY_FILE_SIZE`.
pub(crate) fn max_display_file_size() -> u64 {
    parse_env_u64("CQS_MAX_DISPLAY_FILE_SIZE", MAX_DISPLAY_FILE_SIZE)
}

/// Resolve the `cqs read` file-size cap honoring `CQS_READ_MAX_FILE_SIZE`.
pub(crate) fn read_max_file_size() -> u64 {
    parse_env_u64("CQS_READ_MAX_FILE_SIZE", READ_MAX_FILE_SIZE)
}

// ============ daemon response cap ============

/// Default 16 MiB cap on the daemon-to-CLI response buffer. Larger payloads
/// force the CLI to fall back to direct (non-daemon) execution.
pub(crate) const MAX_DAEMON_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve the daemon response cap honoring `CQS_DAEMON_MAX_RESPONSE_BYTES`.
pub(crate) fn max_daemon_response_bytes() -> u64 {
    parse_env_u64("CQS_DAEMON_MAX_RESPONSE_BYTES", MAX_DAEMON_RESPONSE_BYTES)
}

// ============ daemon periodic GC cadence ============

/// Default idle-time periodic GC interval (seconds). The daemon checks
/// for a tick every loop iteration; the actual prune only fires once
/// this many seconds have passed since the last periodic sweep.
pub(crate) const DAEMON_PERIODIC_GC_INTERVAL_SECS_DEFAULT: u64 = 1800; // 30 minutes

/// Default minimum idle gap (seconds) between the last file event and a
/// periodic GC tick. Prevents GC mid-burst during a long run of file
/// changes.
pub(crate) const DAEMON_PERIODIC_GC_IDLE_SECS_DEFAULT: u64 = 60;

/// Resolve the periodic-GC interval honoring `CQS_DAEMON_PERIODIC_GC_INTERVAL_SECS`.
/// Falls back to [`DAEMON_PERIODIC_GC_INTERVAL_SECS_DEFAULT`] when unset,
/// empty, unparseable, or zero.
pub(crate) fn daemon_periodic_gc_interval_secs() -> u64 {
    parse_env_u64(
        "CQS_DAEMON_PERIODIC_GC_INTERVAL_SECS",
        DAEMON_PERIODIC_GC_INTERVAL_SECS_DEFAULT,
    )
}

/// Resolve the periodic-GC idle gap honoring `CQS_DAEMON_PERIODIC_GC_IDLE_SECS`.
/// Falls back to [`DAEMON_PERIODIC_GC_IDLE_SECS_DEFAULT`] when unset,
/// empty, unparseable, or zero.
pub(crate) fn daemon_periodic_gc_idle_secs() -> u64 {
    parse_env_u64(
        "CQS_DAEMON_PERIODIC_GC_IDLE_SECS",
        DAEMON_PERIODIC_GC_IDLE_SECS_DEFAULT,
    )
}

// ============ periodic full-tree reconciliation ============

/// Default periodic reconciliation interval (seconds). Every this many
/// seconds the watch loop walks the working tree, compares stored mtime
/// against current FS mtime via [`Store::list_stale_files`], and queues
/// any divergent files for reindex. Catches missed inotify events from
/// bulk git operations, WSL 9P drops, and external writers.
///
/// 30 s default targets "user can't tell the freshness gap exists." On a
/// 17k-chunk corpus the walk is sub-second on Linux and ~1 s on WSL — both
/// well under the human perceptibility threshold for an idle-time tick.
pub(crate) const DAEMON_RECONCILE_INTERVAL_SECS_DEFAULT: u64 = 30;

/// Resolve the periodic-reconcile interval honoring `CQS_WATCH_RECONCILE_SECS`.
/// Falls back to [`DAEMON_RECONCILE_INTERVAL_SECS_DEFAULT`] when unset,
/// empty, unparseable, or zero.
///
/// Set `CQS_WATCH_RECONCILE_SECS=0` via env unset (parser falls back to
/// default) — to actually disable, use `CQS_WATCH_RECONCILE=0`. The
/// disable knob is checked in the watch loop, not here.
pub(crate) fn daemon_reconcile_interval_secs() -> u64 {
    parse_env_u64(
        "CQS_WATCH_RECONCILE_SECS",
        DAEMON_RECONCILE_INTERVAL_SECS_DEFAULT,
    )
}

// ============ batch stdin line cap ============

/// Default cap on a single batch stdin / daemon line. Matches
/// [`MAX_DIFF_BYTES`] (50 MiB) so a piped `--stdin` diff that passes the CLI
/// path is not silently rejected by the batch path.
pub(crate) const DEFAULT_MAX_BATCH_LINE_LEN: usize = MAX_DIFF_BYTES;

/// Resolve the batch-line cap honoring `CQS_BATCH_MAX_LINE_LEN`.
pub(crate) fn batch_max_line_len() -> usize {
    parse_env_usize("CQS_BATCH_MAX_LINE_LEN", DEFAULT_MAX_BATCH_LINE_LEN)
}

// ============ shared parsing helpers ============

/// Parse a `usize`-shaped env var. Empty / unparseable / zero values fall
/// back to the supplied default. Zero is rejected because every caller
/// here treats the value as a non-zero size limit; a caller that wants to
/// disable a check should remove the call, not set it to zero.
///
/// Warns loudly on a malformed-but-set value so an operator who typoed
/// `CQS_RERANK_POOL_MAX=128 ` (trailing space) or `=abc` sees the
/// silent-default fall-through instead of debugging "why isn't my env var
/// doing anything." Mirrors the warn pattern in the other env-knob helpers.
fn parse_env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    env = key,
                    value = %v,
                    "Invalid env var (must be positive usize), using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Same as [`parse_env_usize`] but for `u64`-shaped byte limits.
/// Same warn-on-malformed contract.
fn parse_env_u64(key: &str, default: u64) -> u64 {
    match std::env::var(key) {
        Ok(v) => match v.parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    env = key,
                    value = %v,
                    "Invalid env var (must be positive u64), using default {default}"
                );
                default
            }
        },
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each env-override helper must return its default when the env var
    /// is unset, garbage, or zero. Setting a positive value must take.
    #[test]
    fn rerank_pool_size_uses_defaults_and_env() {
        // SAFETY: env mutation is ordered by Rust's per-test single-thread
        // default. We unset before/after to keep tests independent.
        std::env::remove_var("CQS_RERANK_OVER_RETRIEVAL");
        std::env::remove_var("CQS_RERANK_POOL_MAX");
        // limit=5, default mult=4, default cap=20 → min(20, 20) = 20
        assert_eq!(rerank_pool_size(5), 20);
        // limit=10 would be 40, but cap=20 holds
        assert_eq!(rerank_pool_size(10), 20);
        // limit=50 hits the cap
        assert_eq!(rerank_pool_size(50), 20);

        std::env::set_var("CQS_RERANK_OVER_RETRIEVAL", "8");
        std::env::set_var("CQS_RERANK_POOL_MAX", "200");
        assert_eq!(rerank_pool_size(5), 40);
        assert_eq!(rerank_pool_size(50), 200);

        // Garbage falls back to default
        std::env::set_var("CQS_RERANK_OVER_RETRIEVAL", "not-a-number");
        std::env::set_var("CQS_RERANK_POOL_MAX", "0");
        assert_eq!(rerank_pool_size(5), 20);

        std::env::remove_var("CQS_RERANK_OVER_RETRIEVAL");
        std::env::remove_var("CQS_RERANK_POOL_MAX");
    }

    #[test]
    fn parse_env_usize_rejects_zero_and_garbage() {
        std::env::set_var("CQS_TEST_LIMIT_PARSE", "0");
        assert_eq!(parse_env_usize("CQS_TEST_LIMIT_PARSE", 42), 42);
        std::env::set_var("CQS_TEST_LIMIT_PARSE", "junk");
        assert_eq!(parse_env_usize("CQS_TEST_LIMIT_PARSE", 42), 42);
        std::env::set_var("CQS_TEST_LIMIT_PARSE", "7");
        assert_eq!(parse_env_usize("CQS_TEST_LIMIT_PARSE", 42), 7);
        std::env::remove_var("CQS_TEST_LIMIT_PARSE");
    }
}
