//! CLI command handlers
//!
//! Commands are organized into thematic subdirectories:
//! - `search/` — semantic search, context assembly, exploration
//! - `graph/` — call graph analysis, impact, tracing, type dependencies
//! - `review/` — diff review, CI analysis, dead code, health checks
//! - `index/` — indexing, stats, staleness, garbage collection
//! - `io/` — file reading, reconstruction, blame, context, notes, diffs
//! - `infra/` — init, doctor, audit mode, telemetry, projects, references
//! - `train/` — planning, task context, training data, model export

mod graph;
mod index;
mod infra;
mod io;
pub(crate) mod resolve;
pub(crate) mod review;
mod search;
mod train;

// Re-export inner modules accessed directly by batch handlers via
// crate::cli::commands::{module}::{function} paths.
pub(crate) use graph::explain;
pub(crate) use graph::trace;
pub(crate) use io::blame;
pub(crate) use io::context;
pub(crate) use io::read;
pub(crate) use review::ci;
pub(crate) use train::task;

// -- search --
pub(crate) use search::cmd_gather;
pub(crate) use search::cmd_neighbors;
pub(crate) use search::cmd_onboard;
pub(crate) use search::cmd_query;
pub(crate) use search::cmd_related;
pub(crate) use search::cmd_scout;
pub(crate) use search::cmd_similar;
pub(crate) use search::cmd_where;
pub(crate) use search::related_result_to_json;
pub(crate) use search::where_to_json;
pub(crate) use search::GatherContext;

// -- graph --
pub(crate) use graph::build_test_map;
pub(crate) use graph::callees_to_json;
pub(crate) use graph::callers_to_json;
pub(crate) use graph::cmd_callees;
pub(crate) use graph::cmd_callers;
pub(crate) use graph::cmd_deps;
pub(crate) use graph::cmd_explain;
pub(crate) use graph::cmd_impact;
pub(crate) use graph::cmd_impact_diff;
pub(crate) use graph::cmd_test_map;
pub(crate) use graph::cmd_trace;
pub(crate) use graph::deps_forward_to_json;
pub(crate) use graph::deps_reverse_to_json;
pub(crate) use graph::test_map_to_json;

// -- review --
pub(crate) use review::cmd_affected;
pub(crate) use review::cmd_ci;
pub(crate) use review::cmd_dead;
pub(crate) use review::cmd_health;
pub(crate) use review::cmd_review;
pub(crate) use review::cmd_suggest;
pub(crate) use review::dead_to_json;

// -- index --
pub(crate) use index::build_hnsw_index_owned;
pub(crate) use index::cmd_gc;
pub(crate) use index::cmd_index;
pub(crate) use index::cmd_stale;
pub(crate) use index::cmd_stats;
pub(crate) use index::stale_to_json;
pub(crate) use index::stats_to_json;

// -- io --
pub(crate) use io::cmd_blame;
pub(crate) use io::cmd_brief;
pub(crate) use io::cmd_context;
pub(crate) use io::cmd_diff;
pub(crate) use io::cmd_drift;
pub(crate) use io::cmd_notes;
pub(crate) use io::cmd_read;
pub(crate) use io::cmd_reconstruct;
pub(crate) use io::NotesCommand;

// -- infra --
pub(crate) use infra::cmd_audit_mode;
#[cfg(feature = "convert")]
pub(crate) use infra::cmd_convert;
pub(crate) use infra::cmd_doctor;
pub(crate) use infra::cmd_init;
pub(crate) use infra::cmd_project;
pub(crate) use infra::cmd_ref;
pub(crate) use infra::cmd_telemetry;
pub(crate) use infra::cmd_telemetry_reset;
pub(crate) use infra::ProjectCommand;
pub(crate) use infra::RefCommand;

// -- train --
pub(crate) use train::cmd_export_model;
pub(crate) use train::cmd_plan;
pub(crate) use train::cmd_task;
pub(crate) use train::cmd_train_data;
pub(crate) use train::cmd_train_pairs;

/// Count tokens for text, with fallback estimation on error.
///
/// Used by `--tokens` token-budgeted output across multiple commands.
pub(crate) fn count_tokens(embedder: &cqs::Embedder, text: &str, label: &str) -> usize {
    embedder.token_count(text).unwrap_or_else(|e| {
        tracing::warn!(error = %e, chunk = label, "Token count failed, estimating");
        text.len() / 4
    })
}

/// Batch-count tokens for multiple texts.
///
/// Uses `encode_batch` for better throughput than individual `count_tokens` calls.
/// Falls back to per-text estimation on error.
pub(crate) fn count_tokens_batch(embedder: &cqs::Embedder, texts: &[&str]) -> Vec<usize> {
    embedder.token_counts_batch(texts).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Batch token count failed, estimating per-text");
        texts.iter().map(|t| t.len() / 4).collect()
    })
}

/// Estimated per-result JSON envelope overhead in tokens (field names, paths, metadata).
pub(crate) const JSON_OVERHEAD_PER_RESULT: usize = 35;

/// Greedy knapsack token packing: sort items by score descending, include items
/// while the total token count stays within `budget`. Always includes at least one item.
///
/// `json_overhead_per_item` adds per-item overhead for JSON envelope tokens.
/// Pass `0` for text output, `JSON_OVERHEAD_PER_RESULT` for JSON.
///
/// Returns `(packed_items, tokens_used)` where `tokens_used` includes overhead.
///
/// Callers build a `texts` slice parallel to `items`, call `count_tokens_batch` to get
/// token counts, then pass those counts here. This two-step avoids borrow/move conflicts.
pub(crate) fn token_pack<T>(
    items: Vec<T>,
    token_counts: &[usize],
    budget: usize,
    json_overhead_per_item: usize,
    score_fn: impl Fn(&T) -> f32,
) -> (Vec<T>, usize) {
    debug_assert_eq!(items.len(), token_counts.len());

    // Build index order sorted by score descending
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| score_fn(&items[b]).total_cmp(&score_fn(&items[a])));

    // Greedy pack in score order, tracking which indices to keep
    let mut used: usize = 0;
    let mut kept_any = false;
    let mut keep: Vec<bool> = vec![false; items.len()];
    for idx in order {
        let tokens = token_counts[idx] + json_overhead_per_item;
        if used + tokens > budget && kept_any {
            break;
        }
        if !kept_any && tokens > budget {
            // Always include at least one result, but cap at 10x budget to avoid
            // pathological cases (e.g., 50K-token item with 300-token budget)
            if tokens > budget * 10 {
                tracing::debug!(tokens, budget, "First item exceeds 10x budget, skipping");
                continue;
            }
            tracing::debug!(
                tokens,
                budget,
                "First item exceeds token budget, including anyway"
            );
        }
        used += tokens;
        keep[idx] = true;
        kept_any = true;
    }

    // Preserve original ordering among kept items (stable extraction)
    let mut packed = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        if keep[i] {
            packed.push(item);
        }
    }
    (packed, used)
}

/// Greedy index-based packing: sort items by score descending, pack until budget.
///
/// Unlike [`token_pack`] which takes and returns owned items, this returns
/// kept indices (in original order) so callers can selectively extract from
/// multiple parallel collections. Used by waterfall budgeting in `task`.
///
/// **Difference from `token_pack`:** returns empty when `budget == 0`.
/// `token_pack` always includes at least one item (for user-facing search).
/// `index_pack` is for internal budgeting where zero-allocation sections are valid.
///
/// Returns `(kept_indices_in_original_order, tokens_used)`.
pub(crate) fn index_pack(
    token_counts: &[usize],
    budget: usize,
    overhead_per_item: usize,
    score_fn: impl Fn(usize) -> f32,
) -> (Vec<usize>, usize) {
    if token_counts.is_empty() || budget == 0 {
        return (Vec::new(), 0);
    }
    let mut order: Vec<usize> = (0..token_counts.len()).collect();
    order.sort_by(|&a, &b| score_fn(b).total_cmp(&score_fn(a)));

    let mut used = 0;
    let mut kept = Vec::new();
    for idx in order {
        let cost = token_counts[idx] + overhead_per_item;
        if used + cost > budget && !kept.is_empty() {
            break;
        }
        used += cost;
        kept.push(idx);
    }
    kept.sort(); // preserve original order
    (kept, used)
}

/// Read diff text from stdin, capped at 50 MB.
pub(crate) fn read_stdin() -> anyhow::Result<String> {
    use std::io::Read;
    const MAX_STDIN_SIZE: usize = 50 * 1024 * 1024; // 50 MB
    let mut buf = String::new();
    std::io::stdin()
        .take(MAX_STDIN_SIZE as u64 + 1)
        .read_to_string(&mut buf)?;
    if buf.len() > MAX_STDIN_SIZE {
        anyhow::bail!("stdin input exceeds 50 MB limit");
    }
    Ok(buf)
}

/// Run `git diff` and return the output. Validates `base` ref to prevent argument injection.
pub(crate) fn run_git_diff(base: Option<&str>) -> anyhow::Result<String> {
    let _span = tracing::info_span!("run_git_diff").entered();

    let mut cmd = std::process::Command::new("git");
    cmd.args(["--no-pager", "diff", "--no-color"]);
    if let Some(b) = base {
        if b.starts_with('-') || b.contains('\0') {
            anyhow::bail!(
                "Invalid base ref '{}': must not start with '-' or contain null bytes",
                b
            );
        }
        cmd.arg(b);
    }

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run 'git diff': {}. Is git installed?", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    const MAX_DIFF_SIZE: usize = 50 * 1024 * 1024; // 50 MB
    if output.stdout.len() > MAX_DIFF_SIZE {
        anyhow::bail!(
            "git diff output exceeds {} MB limit ({} bytes)",
            MAX_DIFF_SIZE / 1024 / 1024,
            output.stdout.len()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_pack_empty() {
        let items: Vec<i32> = vec![];
        let counts: Vec<usize> = vec![];
        let (packed, used) = token_pack(items, &counts, 100, 0, |_| 1.0);
        assert!(packed.is_empty());
        assert_eq!(used, 0);
    }

    #[test]
    fn test_token_pack_single_item() {
        let items = vec!["a"];
        let counts = vec![50];
        let (packed, used) = token_pack(items, &counts, 10, 0, |_| 1.0);
        // Always includes at least one item even if over budget
        assert_eq!(packed.len(), 1);
        assert_eq!(used, 50);
    }

    #[test]
    fn test_token_pack_all_fit() {
        let items = vec!["a", "b", "c"];
        let counts = vec![10, 20, 30];
        let (packed, used) = token_pack(items, &counts, 100, 0, |_| 1.0);
        assert_eq!(packed.len(), 3);
        assert_eq!(used, 60);
    }

    #[test]
    fn test_token_pack_budget_forces_selection() {
        // 5 items, budget fits 3: should pick highest-scored
        let items = vec!["a", "b", "c", "d", "e"];
        let counts = vec![10, 10, 10, 10, 10];
        // Scores: a=1, b=5, c=3, d=4, e=2 → picks b,d,c (top 3 by score)
        let (packed, used) = token_pack(items, &counts, 30, 0, |item| match *item {
            "a" => 1.0,
            "b" => 5.0,
            "c" => 3.0,
            "d" => 4.0,
            "e" => 2.0,
            _ => 0.0,
        });
        assert_eq!(packed.len(), 3);
        assert_eq!(used, 30);
        // Verify highest-scored items are kept
        assert!(packed.contains(&"b"));
        assert!(packed.contains(&"c"));
        assert!(packed.contains(&"d"));
    }

    #[test]
    fn test_token_pack_preserves_original_order() {
        // Items should be returned in input order, not score order
        let items = vec!["a", "b", "c"];
        let counts = vec![10, 10, 10];
        let (packed, _) = token_pack(items, &counts, 20, 0, |item| match *item {
            "a" => 1.0, // lowest score
            "b" => 3.0, // highest score
            "c" => 2.0,
            _ => 0.0,
        });
        // Should keep b and c (highest scores), in original order: b, c
        assert_eq!(packed, vec!["b", "c"]);
    }

    #[test]
    fn test_token_pack_json_overhead() {
        // With overhead=35, each item costs 10+35=45 tokens
        let items = vec!["a", "b", "c"];
        let counts = vec![10, 10, 10];
        // Budget 100: fits 2 items at 45 each (90), but not 3 (135)
        let (packed, used) = token_pack(items, &counts, 100, 35, |_| 1.0);
        assert_eq!(packed.len(), 2);
        assert_eq!(used, 90); // 2 * (10 + 35)
    }
}
