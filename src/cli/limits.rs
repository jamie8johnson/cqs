//! Shared clamp ceilings and env-overridable size limits for the CLI and
//! batch dispatchers.
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
//!
//! # P3 audit (post-v1.27.0)
//!
//! Items #100, #107, #109: a wave of magic-number caps scattered across
//! the CLI layer (rerank pool, stdin/diff caps, display/read file size
//! caps, daemon response cap) were extracted here so they share a single
//! env-override pattern. Library-level caps (parser, FTS, graph,
//! converter) live in `crate::limits` because their callers (`parser/`,
//! `store/`, `nl/`, `convert/`) cannot reach into the CLI module.

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

// ============ P3 #100: reranker pool sizing ============

/// Default over-retrieval multiplier for the cross-encoder reranker.
/// At `--rerank --limit N` we send `N * MULTIPLIER` candidates through
/// stage 1 so the reranker has enough recall headroom to surface the
/// right result. Honored by [`rerank_pool_size`].
pub(crate) const RERANK_OVER_RETRIEVAL_MULTIPLIER: usize = 4;

/// Default hard cap on the reranker pool, regardless of multiplier.
/// At `--limit 50 --rerank` the multiplier alone would yield 200 — the
/// cap keeps ORT memory and per-batch latency bounded on small GPUs.
///
/// The Reranker V2 post-mortem (2026-04-17) found that weak cross-encoders
/// degrade monotonically with pool size — at 80 candidates they're just
/// shuffling noise. "Drowning in Documents" (arXiv 2411.11767) reports
/// similar behavior for off-the-shelf cross-encoders; small pools
/// (~20) consistently beat large ones on recall@k. We cap here.
///
/// Honored by [`rerank_pool_size`].
pub(crate) const RERANK_POOL_MAX: usize = 20;

/// Resolve the over-retrieval multiplier honoring `CQS_RERANK_OVER_RETRIEVAL`.
/// Falls back to [`RERANK_OVER_RETRIEVAL_MULTIPLIER`] when the env var is
/// unset, empty, or unparseable. Zero is rejected so a misconfigured env
/// can't silently degrade reranking to single-candidate mode.
pub(crate) fn rerank_over_retrieval_multiplier() -> usize {
    parse_env_usize(
        "CQS_RERANK_OVER_RETRIEVAL",
        RERANK_OVER_RETRIEVAL_MULTIPLIER,
    )
}

/// Resolve the reranker pool cap honoring `CQS_RERANK_POOL_MAX`.
pub(crate) fn rerank_pool_max() -> usize {
    parse_env_usize("CQS_RERANK_POOL_MAX", RERANK_POOL_MAX)
}

/// Compute the over-retrieval pool size for a given user-facing limit.
/// Used by the four production rerank call sites (CLI query, CLI ref-only
/// query, batch search, batch ref search) so the policy lives in one
/// place. See P3 #100 in `docs/audit-findings.md`.
pub(crate) fn rerank_pool_size(user_limit: usize) -> usize {
    user_limit
        .saturating_mul(rerank_over_retrieval_multiplier())
        .min(rerank_pool_max())
}

// ============ P3 #107: stdin/diff/display/read file caps ============

/// Default 50 MiB cap on stdin diff input (`cqs impact --diff`,
/// `cqs review --stdin`) and on `git diff` subprocess stdout. See P3 #107.
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

// ============ P3 #109: daemon response cap ============

/// Default 16 MiB cap on the daemon-to-CLI response buffer. Larger
/// payloads force the CLI to fall back to direct (non-daemon) execution.
/// See P3 #109.
pub(crate) const MAX_DAEMON_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Resolve the daemon response cap honoring `CQS_DAEMON_MAX_RESPONSE_BYTES`.
pub(crate) fn max_daemon_response_bytes() -> u64 {
    parse_env_u64("CQS_DAEMON_MAX_RESPONSE_BYTES", MAX_DAEMON_RESPONSE_BYTES)
}

// ============ SHL-V1.29-2: batch stdin line cap ============

/// Default cap on a single batch stdin / daemon line. Matches
/// [`MAX_DIFF_BYTES`] (50 MiB) so a piped `--stdin` diff that passes the
/// CLI path is not silently rejected by the batch path. Historically this
/// was 1 MiB, which blocked realistic unified diffs of large PRs from
/// reaching the daemon.
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
