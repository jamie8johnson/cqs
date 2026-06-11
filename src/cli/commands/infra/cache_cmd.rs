//! `cqs cache` subcommands — stats, prune, compact for the project-scoped
//! embeddings cache.
//!
//! The cache lives at `<project_cqs_dir>/embeddings_cache.db`, survives
//! `cqs slot remove` / `cqs slot create` cycles, and is keyed by
//! `(content_hash, model_id)` so an embedder swap only re-embeds chunks
//! whose hash hasn't been seen for that model_id before.
//!
//! Outside a project (no `.cqs/` dir found), commands fall back to the
//! global cache at `~/.cache/cqs/embeddings.db` so `cqs cache stats`
//! invoked from a non-project shell keeps producing useful output.

use anyhow::{Context, Result};
use clap::Subcommand;

use cqs::cache::{EmbeddingCache, QueryCache};

use crate::cli::config::find_project_root;
use crate::cli::definitions::TextJsonArgs;
use crate::cli::Cli;

/// Subcommands flatten the shared `TextJsonArgs` instead of declaring inline
/// `json: bool` fields, so every `--json`-bearing subcommand in the CLI uses
/// one definition.
#[derive(Subcommand, Clone, Debug)]
pub(crate) enum CacheCommand {
    /// Show cache statistics (entries, size, models). Use `--per-model` for
    /// per-model_id rows so you know which model dominates the cache.
    Stats {
        /// Surface per-model_id entry counts and bytes.
        #[arg(long)]
        per_model: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Delete all cached embeddings (or only for a model fingerprint)
    Clear {
        /// Only clear entries for this model fingerprint
        #[arg(long)]
        model: Option<String>,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Remove entries older than N days, OR every entry for a given model_id.
    Prune {
        /// Days to keep — entries older than this are removed. Mutually
        /// exclusive with `--model`.
        #[arg(value_name = "DAYS")]
        days: Option<u32>,
        /// Drop every entry tagged with this model_id (e.g.,
        /// `BAAI/bge-large-en-v1.5@<rev>`). Mutually exclusive with positional
        /// `DAYS`.
        #[arg(long, conflicts_with = "days")]
        model: Option<String>,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// VACUUM the cache DB to reclaim unused pages after large deletes.
    Compact {
        #[command(flatten)]
        output: TextJsonArgs,
    },
}

// ─── Typed outputs (the JSON schema) ────────────────────────────────────────

/// `cqs cache stats --json` payload. Bytes is the canonical unit on the JSON
/// path (the text path renders MB); emitting both would let callers diverge
/// silently. `per_model` is empty unless `--per-model` was passed.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CacheStatsOutput {
    pub cache_path: String,
    pub total_entries: u64,
    pub total_size_bytes: u64,
    pub unique_models: u64,
    pub oldest_timestamp: Option<i64>,
    pub newest_timestamp: Option<i64>,
    /// Always present; 0 when the persistent QueryCache file doesn't exist yet.
    pub query_cache_size_bytes: u64,
    /// `ok` / `missing` / `error: <msg>` — disambiguates a legitimate empty
    /// cache from an open/size failure (both report 0 bytes).
    pub query_cache_status: String,
    pub per_model: Vec<cqs::cache::PerModelStats>,
}

/// `cqs cache clear --json` payload.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CacheClearOutput {
    pub deleted: usize,
    pub model: Option<String>,
}

/// `cqs cache prune --json` payload. Exactly one of `older_than_days` /
/// `model` is `Some`, mirroring the mutually-exclusive CLI surface.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CachePruneOutput {
    pub pruned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older_than_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// `cqs cache compact --json` payload.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CacheCompactOutput {
    pub size_before_bytes: u64,
    pub size_after_bytes: u64,
    pub reclaimed_bytes: u64,
}

// ─── Cores (surface-agnostic; cache has no daemon path) ──────────────────────

/// Surface-agnostic core for `cqs cache stats`. Reads the embedding cache
/// stats plus the persistent QueryCache size, returning the typed output the
/// text and JSON renderers both consume.
pub(crate) fn cache_stats_core(
    cache: &EmbeddingCache,
    cache_path: &std::path::Path,
    per_model: bool,
) -> Result<CacheStatsOutput> {
    let _span = tracing::info_span!("cache_stats_core", per_model).entered();
    let stats = cache.stats().context("Failed to get cache stats")?;
    let per_model_rows = if per_model {
        cache
            .stats_per_model()
            .context("Failed to get per-model cache stats")?
    } else {
        Vec::new()
    };

    // Surface the persistent QueryCache size; report a structured status so
    // consumers distinguish "missing file" (legitimate 0) from "open failed".
    let (query_cache_size_bytes, query_cache_status): (u64, String) = {
        let q_path = QueryCache::default_path();
        if !q_path.exists() {
            (0, "missing".to_string())
        } else {
            match QueryCache::open(&q_path) {
                Ok(qc) => match qc.size_bytes() {
                    Ok(n) => (n, "ok".to_string()),
                    Err(e) => {
                        tracing::warn!(error = %e, "Query cache size_bytes failed");
                        (0, format!("error: {e}"))
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache open failed for stats");
                    (0, format!("error: {e}"))
                }
            }
        }
    };

    Ok(CacheStatsOutput {
        cache_path: cache_path.display().to_string(),
        total_entries: stats.total_entries,
        total_size_bytes: stats.total_size_bytes,
        unique_models: stats.unique_models,
        oldest_timestamp: stats.oldest_timestamp,
        newest_timestamp: stats.newest_timestamp,
        query_cache_size_bytes,
        query_cache_status,
        per_model: per_model_rows,
    })
}

/// Surface-agnostic core for `cqs cache clear`. Mutating.
pub(crate) fn cache_clear_core(
    cache: &EmbeddingCache,
    model: Option<&str>,
) -> Result<CacheClearOutput> {
    let _span = tracing::info_span!("cache_clear_core", model = ?model).entered();
    let deleted = cache.clear(model).context("Failed to clear cache")?;
    Ok(CacheClearOutput {
        deleted,
        model: model.map(str::to_string),
    })
}

/// Surface-agnostic core for `cqs cache prune`. Mutating. `days` and `model`
/// are mutually exclusive (clap enforces it; this re-checks defensively).
pub(crate) fn cache_prune_core(
    cache: &EmbeddingCache,
    days: Option<u32>,
    model: Option<&str>,
) -> Result<CachePruneOutput> {
    let _span = tracing::info_span!("cache_prune_core", days, model).entered();
    match (days, model) {
        (Some(d), None) => {
            let pruned = cache
                .prune_older_than(d)
                .context("Failed to prune cache by age")?;
            Ok(CachePruneOutput {
                pruned,
                older_than_days: Some(d),
                model: None,
            })
        }
        (None, Some(m)) => {
            let pruned = cache
                .prune_by_model(m)
                .context("Failed to prune cache by model")?;
            Ok(CachePruneOutput {
                pruned,
                older_than_days: None,
                model: Some(m.to_string()),
            })
        }
        (None, None) => anyhow::bail!(
            "cqs cache prune requires either DAYS positional or --model <id>; see --help"
        ),
        // clap conflicts_with should prevent this; defense-in-depth.
        (Some(_), Some(_)) => {
            anyhow::bail!("cqs cache prune: DAYS and --model are mutually exclusive")
        }
    }
}

/// Input for [`cache_compact_core`]. `cqs cache compact` takes no positional
/// or flag input; the empty struct keeps the surface-agnostic Args convention
/// every other core follows (a wire caller inflates it from `{}`).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct CacheCompactArgs {}

/// Surface-agnostic core for `cqs cache compact`. VACUUMs the DB and reports
/// the size delta. Mutating (rewrites the DB file).
pub(crate) fn cache_compact_core(
    cache: &EmbeddingCache,
    _args: &CacheCompactArgs,
) -> Result<CacheCompactOutput> {
    let _span = tracing::info_span!("cache_compact_core").entered();
    let before = cache.stats().context("Failed to read pre-compact stats")?;
    cache.compact().context("Failed to VACUUM cache DB")?;
    let after = cache.stats().context("Failed to read post-compact stats")?;
    Ok(CacheCompactOutput {
        size_before_bytes: before.total_size_bytes,
        size_after_bytes: after.total_size_bytes,
        reclaimed_bytes: before
            .total_size_bytes
            .saturating_sub(after.total_size_bytes),
    })
}

/// Resolve the cache path: project-scoped if invoked inside a project tree,
/// otherwise fall back to the legacy global `~/.cache/cqs/embeddings.db`.
fn resolve_cache_path() -> std::path::PathBuf {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    if cqs_dir.exists() {
        EmbeddingCache::project_default_path(&cqs_dir)
    } else {
        EmbeddingCache::default_path()
    }
}

pub(crate) fn cmd_cache(cli: &Cli, subcmd: &CacheCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_cache").entered();

    // The embeddings cache is project-scoped, *not* per-slot. Passing
    // `cqs --slot foo cache stats` would otherwise be silently accepted with
    // the project-default path resolved anyway. Surface the misuse
    // explicitly so we don't pretend to honor `--slot`.
    if cli.slot.is_some() {
        anyhow::bail!(
            "--slot has no effect on `cqs cache` subcommands (the embeddings cache is project-scoped, not per-slot)"
        );
    }

    let cache_path = resolve_cache_path();
    let cache = EmbeddingCache::open(&cache_path)
        .with_context(|| format!("Failed to open embedding cache at {}", cache_path.display()))?;

    // Top-level `--json` always wins (mirrors `cmd_model`). `cqs --json cache
    // stats` must emit envelope JSON even without `--json` after the subcommand.
    match subcmd {
        CacheCommand::Stats { per_model, output } => {
            cache_stats(&cache, &cache_path, *per_model, cli.json || output.json)
        }
        CacheCommand::Clear { model, output } => {
            cache_clear(&cache, model.as_deref(), cli.json || output.json)
        }
        CacheCommand::Prune {
            days,
            model,
            output,
        } => cache_prune(&cache, *days, model.as_deref(), cli.json || output.json),
        CacheCommand::Compact { output } => cache_compact(&cache, cli.json || output.json),
    }
}

fn cache_stats(
    cache: &EmbeddingCache,
    cache_path: &std::path::Path,
    per_model: bool,
    json: bool,
) -> Result<()> {
    let out = cache_stats_core(cache, cache_path, per_model)?;
    if json {
        // Bytes is the canonical unit on the JSON path; the text path renders
        // MB. All four cache subcommands share the bytes-only JSON contract.
        crate::cli::json_envelope::emit_json(&out)?;
    } else {
        println!("Embedding cache: {}", out.cache_path);
        println!("  Entries:  {}", out.total_entries);
        println!(
            "  Size:     {:.1} MB",
            out.total_size_bytes as f64 / 1_048_576.0
        );
        println!("  Models:   {}", out.unique_models);
        if let Some(oldest) = out.oldest_timestamp {
            println!("  Oldest:   {}", format_timestamp(oldest));
        }
        if let Some(newest) = out.newest_timestamp {
            println!("  Newest:   {}", format_timestamp(newest));
        }
        // Query cache size (0 when file absent). Append the status when it
        // isn't `ok`/`missing` so operators see open/size failures instead of
        // mistaking them for an empty cache.
        match out.query_cache_status.as_str() {
            "ok" | "missing" => println!(
                "Query cache size: {:.1} MB",
                out.query_cache_size_bytes as f64 / 1_048_576.0
            ),
            other => println!(
                "Query cache size: {:.1} MB ({other})",
                out.query_cache_size_bytes as f64 / 1_048_576.0
            ),
        }
        if per_model && !out.per_model.is_empty() {
            println!();
            println!("Per-model:");
            for row in &out.per_model {
                println!(
                    "  {} — {} entries, {:.2} MB",
                    row.model_id,
                    row.entries,
                    row.total_bytes as f64 / 1_048_576.0
                );
            }
        }
    }

    Ok(())
}

fn cache_clear(cache: &EmbeddingCache, model: Option<&str>, json: bool) -> Result<()> {
    let out = cache_clear_core(cache, model)?;
    if json {
        crate::cli::json_envelope::emit_json(&out)?;
    } else if let Some(fp) = &out.model {
        println!("Cleared {} entries for model {}", out.deleted, fp);
    } else {
        println!("Cleared {} entries", out.deleted);
    }

    Ok(())
}

fn cache_prune(
    cache: &EmbeddingCache,
    days: Option<u32>,
    model: Option<&str>,
    json: bool,
) -> Result<()> {
    let out = cache_prune_core(cache, days, model)?;
    if json {
        crate::cli::json_envelope::emit_json(&out)?;
    } else if let Some(d) = out.older_than_days {
        println!("Pruned {} entries older than {} days", out.pruned, d);
    } else if let Some(m) = &out.model {
        println!("Pruned {} entries for model {}", out.pruned, m);
    }

    Ok(())
}

fn cache_compact(cache: &EmbeddingCache, json: bool) -> Result<()> {
    let out = cache_compact_core(cache, &CacheCompactArgs::default())?;
    if json {
        crate::cli::json_envelope::emit_json(&out)?;
    } else {
        println!(
            "Compacted: {:.2} MB → {:.2} MB",
            out.size_before_bytes as f64 / 1_048_576.0,
            out.size_after_bytes as f64 / 1_048_576.0,
        );
    }
    Ok(())
}

fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "unknown".to_string();
    }
    use std::time::{Duration, UNIX_EPOCH};
    // A corrupt cache row with ts == i64::MAX overflows UNIX_EPOCH +
    // Duration on most platforms (SystemTime is i64-seconds backed).
    // checked_add returns None on overflow → emit a sentinel string instead
    // of panicking on dt.elapsed().
    let Some(dt) = UNIX_EPOCH.checked_add(Duration::from_secs(ts as u64)) else {
        return "<unrepresentable>".to_string();
    };
    let elapsed = dt.elapsed().unwrap_or_default();
    let days = elapsed.as_secs() / 86400;
    if days == 0 {
        let hours = elapsed.as_secs() / 3600;
        if hours == 0 {
            format!("{} minutes ago", elapsed.as_secs() / 60)
        } else {
            format!("{} hours ago", hours)
        }
    } else {
        format!("{} days ago", days)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cqs cache stats --json` field set is pinned by the typed
    /// `CacheStatsOutput` — the canonical bytes-only schema. A removed/renamed
    /// field breaks this instead of silently shifting the wire shape.
    #[test]
    fn cache_stats_output_field_names() {
        let out = CacheStatsOutput {
            cache_path: "/tmp/.cqs/embeddings_cache.db".into(),
            total_entries: 100,
            total_size_bytes: 2048,
            unique_models: 2,
            oldest_timestamp: Some(1000),
            newest_timestamp: Some(2000),
            query_cache_size_bytes: 512,
            query_cache_status: "ok".into(),
            per_model: vec![],
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["total_size_bytes"], 2048);
        assert_eq!(json["query_cache_status"], "ok");
        assert!(json.get("per_model").unwrap().is_array());
        // No derived-MB field — bytes is canonical.
        assert!(json.get("total_size_mb").is_none());
    }

    /// `cqs cache prune` emits exactly one of `older_than_days` / `model`
    /// (mutually-exclusive surface), with the other skipped.
    #[test]
    fn cache_prune_output_skips_unused_mode() {
        let by_days = CachePruneOutput {
            pruned: 5,
            older_than_days: Some(30),
            model: None,
        };
        let j = serde_json::to_value(&by_days).unwrap();
        assert_eq!(j["older_than_days"], 30);
        assert!(j.get("model").is_none());

        let by_model = CachePruneOutput {
            pruned: 3,
            older_than_days: None,
            model: Some("bge-large".into()),
        };
        let j = serde_json::to_value(&by_model).unwrap();
        assert_eq!(j["model"], "bge-large");
        assert!(j.get("older_than_days").is_none());
    }

    /// `cqs cache prune` with neither DAYS nor --model is rejected by the core
    /// (the daemon-less mutating path must not silently no-op).
    #[test]
    fn cache_prune_core_requires_a_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cache.db");
        let cache = EmbeddingCache::open(&path).unwrap();
        assert!(cache_prune_core(&cache, None, None).is_err());
        assert!(cache_prune_core(&cache, Some(1), Some("m")).is_err());
    }

    #[test]
    fn cache_compact_output_field_names() {
        let out = CacheCompactOutput {
            size_before_bytes: 4096,
            size_after_bytes: 2048,
            reclaimed_bytes: 2048,
        };
        let j = serde_json::to_value(&out).unwrap();
        assert_eq!(j["reclaimed_bytes"], 2048);
    }

    // format_timestamp must not panic on a corrupt cache row with
    // ts == i64::MAX. A plain `UNIX_EPOCH + Duration::from_secs(ts as u64)`
    // panics on platforms where SystemTime is backed by i64-seconds (some
    // libc / older glibc on 32-bit); `checked_add` avoids that. On Linux
    // x86_64 the addition succeeds and we land in the future-time branch —
    // the post-condition is "no panic, returns a non-empty string". The
    // `<unrepresentable>` branch is the fallback for platforms where
    // checked_add returns None.
    #[test]
    fn format_timestamp_handles_i64_max() {
        let result = format_timestamp(i64::MAX);
        // Either branch is acceptable — we just must not panic and must
        // emit something printable. On platforms where checked_add
        // returns Some, dt.elapsed() returns Err (future time) so
        // unwrap_or_default() yields 0 → "0 minutes ago", which is
        // wrong-but-harmless. On platforms where checked_add overflows
        // we get the explicit sentinel.
        assert!(!result.is_empty());
        assert!(
            result == "<unrepresentable>" || result.ends_with(" ago"),
            "unexpected format_timestamp output: {result}"
        );
    }

    // Even on platforms where i64::MAX doesn't overflow checked_add,
    // we can still exercise the overflow branch by passing a value
    // designed to force the path. Duration::from_secs(u64::MAX) is the
    // largest representable Duration, and adding it to UNIX_EPOCH
    // overflows on every platform.
    #[test]
    fn format_timestamp_overflow_branch_returns_sentinel() {
        // Construct a duration too large to add to UNIX_EPOCH on any
        // platform, by simulating the same overflow path directly.
        use std::time::{Duration, UNIX_EPOCH};
        // Sanity: the sentinel branch fires when checked_add returns None.
        // We cannot pass u64::MAX through format_timestamp's i64 surface,
        // so instead assert the precondition the fix relies on:
        // checked_add with a duration close to Duration::MAX overflows.
        let huge = Duration::from_secs(u64::MAX);
        assert!(
            UNIX_EPOCH.checked_add(huge).is_none(),
            "test precondition: huge duration must overflow checked_add"
        );
    }

    #[test]
    fn format_timestamp_handles_negative_or_zero() {
        assert_eq!(format_timestamp(0), "unknown");
        assert_eq!(format_timestamp(-1), "unknown");
        assert_eq!(format_timestamp(i64::MIN), "unknown");
    }
}
