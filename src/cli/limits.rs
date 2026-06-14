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
//!
//! The env-knob parse/warn/default contract lives once in `cqs::limits`
//! (warns loudly on a malformed-but-set value so a typoed
//! `CQS_RERANK_POOL_MAX=abc` shows the silent fall-through). These CLI
//! resolvers route through those helpers rather than carrying a private copy.

use cqs::limits::{parse_env_u64, parse_env_usize};

/// Hard cap on `--limit` for search, applied identically by the CLI
/// dispatcher (`cli::dispatch`, top-level `cli.limit` clamp) and the daemon
/// batch search handler (`batch::handlers::search`). One constant for both
/// surfaces is a parity requirement: `cqs <query> -n 500` must return the
/// same result count whether or not a daemon happens to serve it.
pub(crate) const SEARCH_LIMIT_CAP: usize = 100;

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

/// Shared `--limit` ceiling for the graph-surface commands and the
/// `explain`/`onboard`/`gather` result lists: `callers`, `callees`,
/// `deps`, `impact`, `test-map`, `explain` (CLI + batch), plus the
/// `onboard` and `gather` seed-fetch limits. All truncate a result list
/// (or per-section list) to at most this many entries before rendering;
/// the underlying store query is unbounded, so callers paginate by
/// re-querying. One constant keeps the truncation ceiling identical
/// across every surface that shares the semantic.
pub(crate) const GRAPH_LIMIT_CAP: usize = 100;

/// `--limit` ceiling for the placement-suggestion commands: `where`,
/// `task`, and the batch `where` / `task` handlers. These rank candidate
/// insertion sites; a short list is the useful output, so the ceiling is
/// deliberately tighter than [`GRAPH_LIMIT_CAP`].
pub(crate) const PLACEMENT_LIMIT_CAP: usize = 10;

/// `--depth` ceiling for `cqs impact` (and the cross-project impact core).
/// Bounds the reverse-call-graph BFS depth so an adversarial `--depth`
/// can't fan the traversal out unbounded. Distinct from a result-count
/// limit — it caps traversal hops, not rendered rows.
pub(crate) const IMPACT_DEPTH_CAP: usize = 10;

/// `--depth` ceiling for `cqs onboard`. Bounds the callee-expansion BFS
/// depth in the guided-tour walk. Smaller than [`IMPACT_DEPTH_CAP`]
/// because onboard's tour stays shallow by design — a deep tour is noise.
pub(crate) const ONBOARD_DEPTH_CAP: usize = 5;

/// `--depth` ceiling for `cqs gather`. Bounds the call-graph BFS-expansion
/// depth around the seed set. Holds the same value as [`ONBOARD_DEPTH_CAP`]
/// today but is an independent knob — gather and onboard tune separately,
/// so a future onboard retune must not silently move gather's ceiling.
pub(crate) const GATHER_DEPTH_CAP: usize = 5;

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

    /// Pin the shared clamp ceilings. These values are duplicated at ~19
    /// call sites via the constants; if a refactor changes one, this test
    /// catches the drift before it ships a surface that clamps differently
    /// from its siblings.
    #[test]
    fn limit_caps_have_expected_values() {
        assert_eq!(GRAPH_LIMIT_CAP, 100);
        assert_eq!(PLACEMENT_LIMIT_CAP, 10);
        assert_eq!(IMPACT_DEPTH_CAP, 10);
        assert_eq!(ONBOARD_DEPTH_CAP, 5);
        assert_eq!(GATHER_DEPTH_CAP, 5);
    }

    /// Structural guard against a half-completed depth-cap sweep: every BFS
    /// depth clamp in the command layer must take its ceiling from a named
    /// `*_DEPTH_CAP` constant, not a raw literal. Asserted on source text on
    /// purpose — gather's literal `5` equals [`ONBOARD_DEPTH_CAP`], so a
    /// value-only check would stay green while the binding silently drifts.
    #[test]
    fn every_bfs_depth_clamp_uses_a_named_cap() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sites: &[(&str, &str)] = &[
            ("src/cli/commands/graph/impact.rs", "args.depth.clamp"),
            ("src/cli/commands/search/onboard.rs", "depth.clamp"),
            (
                "src/cli/commands/search/gather.rs",
                "expand_depth: args.depth.clamp",
            ),
        ];
        let mut offenders: Vec<String> = Vec::new();
        for (rel, marker) in sites {
            let path = std::path::Path::new(manifest_dir).join(rel);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let mut found_any = false;
            for (lineno, line) in src.lines().enumerate() {
                if !line.contains(marker) || !line.contains(".clamp(") {
                    continue;
                }
                found_any = true;
                let after = line.split(".clamp(").nth(1).unwrap_or("");
                let inner = after.split(')').next().unwrap_or("");
                let ceiling = inner.split(',').nth(1).map(str::trim).unwrap_or("");
                if !ceiling.contains("DEPTH_CAP") {
                    offenders.push(format!(
                        "{rel}:{}  depth ceiling `{ceiling}` is a raw literal, not a named *_DEPTH_CAP constant",
                        lineno + 1
                    ));
                }
            }
            assert!(
                found_any,
                "no depth-clamp line matched marker `{marker}` in {rel} — the site moved; update this guard's marker so the family stays enumerated"
            );
        }
        assert!(
            offenders.is_empty(),
            "incomplete depth-cap sweep — these BFS depth clamps still ship a raw literal:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Structural guard against a half-completed *limit*-cap sweep, the
    /// `--limit` sibling of [`every_bfs_depth_clamp_uses_a_named_cap`].
    ///
    /// Every command-layer site that bounds a user-facing `--limit` result
    /// count must take its ceiling from a named `*_LIMIT_*`/`*_CAP` constant
    /// in this module, not ship an unclamped limit. The named-cap consolidation
    /// collected scout/similar/related into `cli::limits` and was later
    /// extended across graph/impact/explain/onboard/where/task — but
    /// `cqs neighbors` (the brute-force cosine sibling of `similar`) predated
    /// the consolidation and was the last to migrate: before the fix
    /// `find_neighbors` did `scored.truncate(limit)` with a raw, unclamped
    /// `limit`, so `cqs neighbors foo -n 1000000` truncated to the full corpus
    /// and fanned out one DB lookup per chunk — exactly the unbounded-result
    /// amplification `similar`'s `clamp(1, SIMILAR_LIMIT_MAX)` exists to prevent.
    ///
    /// Asserted on source text (not on a runtime result count) on purpose:
    /// a member whose clamp drifts to a *raw literal that happens to equal a
    /// sibling cap* would stay green under a value-only check while the named
    /// binding silently rotted — the same trap the depth guard documents.
    ///
    /// `neighbors` now clamps `limit` to a named cap (`SIMILAR_LIMIT_MAX` —
    /// neighbors *is* the brute-force `similar`), so every member of the
    /// family is uniform and this guard is GREEN. It stays to catch the next
    /// command-layer site that bounds a `--limit` without a named cap.
    #[test]
    fn every_limit_clamp_uses_a_named_cap() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");

        // The family: every command-layer source file that owns a user
        // `--limit` result-count, paired with a marker locating the line that
        // *consumes* that limit to bound a result list. A member passes when
        // the limit reaching the result-bounding op was clamped to a named
        // cap somewhere in the file (`.clamp(1, …_CAP)` / `…_MAX`).
        //
        // `marker` is the "site moved" tripwire: if it stops matching, the
        // limit-handling moved and this guard must be re-pointed rather than
        // silently dropping the member from the enumerated family.
        let sites: &[(&str, &str)] = &[
            ("src/cli/commands/search/similar.rs", "args.limit.clamp"),
            ("src/cli/commands/search/scout.rs", "args.limit.clamp"),
            ("src/cli/commands/search/related.rs", "limit.clamp"),
            ("src/cli/commands/search/onboard.rs", "args.limit.clamp"),
            ("src/cli/commands/search/gather.rs", "args.limit.clamp"),
            ("src/cli/commands/search/where_cmd.rs", "limit.clamp"),
            ("src/cli/commands/graph/impact.rs", "args.limit.clamp"),
            ("src/cli/commands/graph/callers.rs", "args.limit.clamp"),
            ("src/cli/commands/graph/deps.rs", "args.limit.clamp"),
            ("src/cli/commands/graph/explain.rs", "limit.clamp"),
            ("src/cli/commands/graph/test_map.rs", "args.limit.clamp"),
            ("src/cli/commands/train/task.rs", "limit.clamp"),
            // The (now-fixed) straggler. `find_neighbors` bounds its result
            // list with `scored.truncate(limit)`. The marker locates the
            // truncation so a future refactor that renames it trips the
            // "site moved" guard.
            (
                "src/cli/commands/search/neighbors.rs",
                "scored.truncate(limit)",
            ),
        ];

        let mut offenders: Vec<String> = Vec::new();
        for (rel, marker) in sites {
            let path = std::path::Path::new(manifest_dir).join(rel);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert!(
                src.contains(marker),
                "no limit-handling line matched marker `{marker}` in {rel} — the site moved; update this guard's marker so the family stays enumerated"
            );
            // A member is uniform iff its limit is clamped to a NAMED cap
            // (`*_CAP`/`*_MAX`). We accept the clamp anywhere in the file: the
            // result-bounding op (`.truncate(limit)` / `.take(limit)`) consumes
            // a `limit` that was clamped upstream in the same `*_core`.
            let clamps_to_named_cap = src.lines().any(|line| {
                if !line.contains(".clamp(1,") {
                    return false;
                }
                let after = line.split(".clamp(1,").nth(1).unwrap_or("");
                let ceiling = after.split(')').next().unwrap_or("");
                ceiling.contains("_CAP") || ceiling.contains("_MAX")
            });
            if !clamps_to_named_cap {
                offenders.push(format!(
                    "{rel}  bounds a `--limit` result list (matched `{marker}`) but no `.clamp(1, *_CAP/*_MAX)` clamps the limit to a named cap — unmigrated limit-cap straggler"
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "incomplete limit-cap sweep — these command-layer sites bound a user `--limit` without a named cap:\n  {}",
            offenders.join("\n  ")
        );
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
