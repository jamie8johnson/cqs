//! `cqs cache` subcommands — stats, prune, compact for the project-scoped
//! embeddings cache.
//!
//! Spec §Cache: cache lives at `<project_cqs_dir>/embeddings_cache.db`,
//! survives `cqs slot remove` / `cqs slot create` cycles, and is keyed by
//! `(content_hash, model_id)` so an embedder swap only re-embeds chunks
//! whose hash hasn't been seen for that model_id before.
//!
//! Outside a project (no `.cqs/` dir found), commands fall back to the
//! legacy global cache at `~/.cache/cqs/embeddings.db` so `cqs cache stats`
//! invoked from a non-project shell keeps producing useful output.

use anyhow::{Context, Result};
use clap::Subcommand;

use cqs::cache::{EmbeddingCache, QueryCache};

use crate::cli::config::find_project_root;
use crate::cli::definitions::TextJsonArgs;
use crate::cli::Cli;

/// API-V1.22-2: subcommands flatten the shared `TextJsonArgs` instead of
/// declaring inline `json: bool` fields, so every `--json`-bearing subcommand
/// in the CLI uses one definition.
#[derive(Subcommand, Clone, Debug)]
pub(crate) enum CacheCommand {
    /// Show cache statistics (entries, size, models). Use `--per-model` for
    /// per-model_id rows so you know which model dominates the cache.
    Stats {
        /// Surface per-model_id entry counts and bytes (spec §Cache).
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
    /// Remove entries older than N days, OR every entry for a given model_id
    /// (per spec §Cache: prune supports `--model` AND `--older-than-days`).
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

    let cache_path = resolve_cache_path();
    let cache = EmbeddingCache::open(&cache_path)
        .with_context(|| format!("Failed to open embedding cache at {}", cache_path.display()))?;

    // Top-level `--json` always wins (mirrors `cmd_model` at
    // `src/cli/commands/infra/model.rs:113`). `cqs --json cache stats` must
    // emit envelope JSON even without `--json` after the subcommand.
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
    let _span = tracing::info_span!("cache_stats", per_model).entered();
    let stats = cache.stats().context("Failed to get cache stats")?;
    let per_model_rows = if per_model {
        cache
            .stats_per_model()
            .context("Failed to get per-model cache stats")?
    } else {
        Vec::new()
    };

    // P3 #124: surface persistent QueryCache size alongside the embedding
    // cache so `cqs cache stats --json` consumers can monitor both.
    // Open is best-effort — missing file is reported as 0 bytes.
    let query_cache_size_bytes: u64 = {
        let q_path = QueryCache::default_path();
        if q_path.exists() {
            match QueryCache::open(&q_path) {
                Ok(qc) => qc.size_bytes().unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Query cache size_bytes failed");
                    0
                }),
                Err(e) => {
                    tracing::warn!(error = %e, "Query cache open failed for stats");
                    0
                }
            }
        } else {
            0
        }
    };

    if json {
        // P1 #11: `total_size_mb` is a numeric field so consumers can do
        // arithmetic on it (e.g., `obj["total_size_mb"] + 1`). Earlier
        // `format!("{:.1}", ...)` made it a string and broke programmatic use.
        let obj = serde_json::json!({
            "cache_path": cache_path.display().to_string(),
            "total_entries": stats.total_entries,
            "total_size_bytes": stats.total_size_bytes,
            "total_size_mb": stats.total_size_bytes as f64 / 1_048_576.0,
            "unique_models": stats.unique_models,
            "oldest_timestamp": stats.oldest_timestamp,
            "newest_timestamp": stats.newest_timestamp,
            // P3 #124: parallel `query_cache_size_bytes` field. Always present;
            // 0 when the QueryCache file doesn't exist yet.
            "query_cache_size_bytes": query_cache_size_bytes,
            "per_model": per_model_rows,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!("Embedding cache: {}", cache_path.display());
        println!("  Entries:  {}", stats.total_entries);
        println!(
            "  Size:     {:.1} MB",
            stats.total_size_bytes as f64 / 1_048_576.0
        );
        println!("  Models:   {}", stats.unique_models);
        if let Some(oldest) = stats.oldest_timestamp {
            println!("  Oldest:   {}", format_timestamp(oldest));
        }
        if let Some(newest) = stats.newest_timestamp {
            println!("  Newest:   {}", format_timestamp(newest));
        }
        // P3 #124: query cache size (0 when file absent). Single line — full
        // QueryCache stats live behind `cqs cache prune` and the daemon log.
        println!(
            "Query cache size: {:.1} MB",
            query_cache_size_bytes as f64 / 1_048_576.0
        );
        if per_model && !per_model_rows.is_empty() {
            println!();
            println!("Per-model:");
            for row in &per_model_rows {
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
    let _span = tracing::info_span!("cache_clear", model = ?model).entered();
    let deleted = cache.clear(model).context("Failed to clear cache")?;

    if json {
        let obj = serde_json::json!({
            "deleted": deleted,
            "model": model,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else if let Some(fp) = model {
        println!("Cleared {} entries for model {}", deleted, fp);
    } else {
        println!("Cleared {} entries", deleted);
    }

    Ok(())
}

fn cache_prune(
    cache: &EmbeddingCache,
    days: Option<u32>,
    model: Option<&str>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cache_prune", days, model).entered();
    let (pruned, mode_label, mode_value): (usize, &'static str, String) = match (days, model) {
        (Some(d), None) => {
            let n = cache
                .prune_older_than(d)
                .context("Failed to prune cache by age")?;
            (n, "older_than_days", d.to_string())
        }
        (None, Some(m)) => {
            let n = cache
                .prune_by_model(m)
                .context("Failed to prune cache by model")?;
            (n, "model", m.to_string())
        }
        (None, None) => {
            anyhow::bail!(
                "cqs cache prune requires either DAYS positional or --model <id>; see --help"
            );
        }
        (Some(_), Some(_)) => {
            // clap conflicts_with should prevent this branch; defense-in-depth.
            anyhow::bail!("cqs cache prune: DAYS and --model are mutually exclusive");
        }
    };

    if json {
        let obj = match mode_label {
            "older_than_days" => serde_json::json!({
                "pruned": pruned,
                "older_than_days": mode_value.parse::<u32>().unwrap_or(0),
            }),
            "model" => serde_json::json!({
                "pruned": pruned,
                "model": mode_value,
            }),
            _ => unreachable!("mode_label must be one of the two arms"),
        };
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        match mode_label {
            "older_than_days" => {
                println!("Pruned {} entries older than {} days", pruned, mode_value)
            }
            "model" => println!("Pruned {} entries for model {}", pruned, mode_value),
            _ => unreachable!(),
        }
    }

    Ok(())
}

fn cache_compact(cache: &EmbeddingCache, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cache_compact").entered();
    let before = cache.stats().context("Failed to read pre-compact stats")?;
    cache.compact().context("Failed to VACUUM cache DB")?;
    let after = cache.stats().context("Failed to read post-compact stats")?;
    if json {
        let obj = serde_json::json!({
            "size_before_bytes": before.total_size_bytes,
            "size_after_bytes": after.total_size_bytes,
            "reclaimed_bytes": before.total_size_bytes.saturating_sub(after.total_size_bytes),
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!(
            "Compacted: {:.2} MB → {:.2} MB",
            before.total_size_bytes as f64 / 1_048_576.0,
            after.total_size_bytes as f64 / 1_048_576.0,
        );
    }
    Ok(())
}

fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "unknown".to_string();
    }
    use std::time::{Duration, UNIX_EPOCH};
    let dt = UNIX_EPOCH + Duration::from_secs(ts as u64);
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
