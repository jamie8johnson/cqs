//! CLI command handlers
//!
//! Each submodule handles one CLI subcommand.

mod audit_mode;
mod context;
#[cfg(feature = "convert")]
mod convert;
mod dead;
mod diff;
mod doctor;
mod explain;
mod gather;
mod gc;
mod graph;
mod impact;
mod impact_diff;
mod index;
mod init;
mod notes;
mod project;
mod query;
mod read;
mod reference;
mod related;
pub(crate) mod resolve;
mod review;
mod scout;
mod similar;
mod stale;
mod stats;
mod test_map;
mod trace;
mod where_cmd;

pub(crate) use audit_mode::cmd_audit_mode;
pub(crate) use context::cmd_context;
#[cfg(feature = "convert")]
pub(crate) use convert::cmd_convert;
pub(crate) use dead::cmd_dead;
pub(crate) use diff::cmd_diff;
pub(crate) use doctor::cmd_doctor;
pub(crate) use explain::cmd_explain;
pub(crate) use gather::cmd_gather;
pub(crate) use gc::cmd_gc;
pub(crate) use graph::{cmd_callees, cmd_callers};
pub(crate) use impact::cmd_impact;
pub(crate) use impact_diff::cmd_impact_diff;
pub(crate) use index::{build_hnsw_index, cmd_index};
pub(crate) use init::cmd_init;
pub(crate) use notes::{cmd_notes, NotesCommand};
pub(crate) use project::{cmd_project, ProjectCommand};
pub(crate) use query::cmd_query;
pub(crate) use read::cmd_read;
pub(crate) use reference::{cmd_ref, RefCommand};
pub(crate) use related::cmd_related;
pub(crate) use review::cmd_review;
pub(crate) use scout::cmd_scout;
pub(crate) use similar::cmd_similar;
pub(crate) use stale::cmd_stale;
pub(crate) use stats::cmd_stats;
pub(crate) use test_map::cmd_test_map;
pub(crate) use trace::cmd_trace;
pub(crate) use where_cmd::cmd_where;

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
    order.sort_by(|&a, &b| {
        score_fn(&items[b])
            .partial_cmp(&score_fn(&items[a]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Greedy pack in score order, tracking which indices to keep
    let mut used: usize = 0;
    let mut keep: Vec<bool> = vec![false; items.len()];
    for idx in order {
        let tokens = token_counts[idx] + json_overhead_per_item;
        if used + tokens > budget && keep.iter().any(|&k| k) {
            break;
        }
        used += tokens;
        keep[idx] = true;
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
        if b.starts_with('-') {
            anyhow::bail!("Invalid base ref '{}': must not start with '-'", b);
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
