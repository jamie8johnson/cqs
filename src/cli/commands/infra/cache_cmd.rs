//! `cqs cache` subcommands — stats, clear, prune for the global embedding cache.

use anyhow::{Context, Result};
use clap::Subcommand;

use cqs::cache::EmbeddingCache;

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum CacheCommand {
    /// Show cache statistics (entries, size, models)
    Stats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete all cached embeddings (or only for a model fingerprint)
    Clear {
        /// Only clear entries for this model fingerprint
        #[arg(long)]
        model: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove entries older than N days
    Prune {
        /// Days to keep (entries older than this are removed)
        days: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn cmd_cache(subcmd: &CacheCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_cache").entered();

    let cache_path = EmbeddingCache::default_path();
    let cache = EmbeddingCache::open(&cache_path)
        .with_context(|| format!("Failed to open embedding cache at {}", cache_path.display()))?;

    match subcmd {
        CacheCommand::Stats { json } => cache_stats(&cache, &cache_path, *json),
        CacheCommand::Clear { model, json } => cache_clear(&cache, model.as_deref(), *json),
        CacheCommand::Prune { days, json } => cache_prune(&cache, *days, *json),
    }
}

fn cache_stats(cache: &EmbeddingCache, cache_path: &std::path::Path, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cache_stats").entered();
    let stats = cache.stats().context("Failed to get cache stats")?;

    if json {
        let obj = serde_json::json!({
            "total_entries": stats.total_entries,
            "total_size_bytes": stats.total_size_bytes,
            "total_size_mb": format!("{:.1}", stats.total_size_bytes as f64 / 1_048_576.0),
            "unique_models": stats.unique_models,
            "oldest_timestamp": stats.oldest_timestamp,
            "newest_timestamp": stats.newest_timestamp,
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

fn cache_prune(cache: &EmbeddingCache, days: u32, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cache_prune", days).entered();
    let pruned = cache
        .prune_older_than(days)
        .context("Failed to prune cache")?;

    if json {
        let obj = serde_json::json!({
            "pruned": pruned,
            "older_than_days": days,
        });
        crate::cli::json_envelope::emit_json(&obj)?;
    } else {
        println!("Pruned {} entries older than {} days", pruned, days);
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
