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

pub(crate) mod eval;
mod graph;
mod index;
mod infra;
mod io;
pub(crate) mod resolve;
pub(crate) mod review;
mod search;
#[cfg(feature = "serve")]
pub(crate) mod serve;
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
pub(crate) use search::build_gather_output;
pub(crate) use search::build_related_output;
pub(crate) use search::build_scout_output;
pub(crate) use search::build_where_output;
pub(crate) use search::cmd_gather;
pub(crate) use search::cmd_neighbors;
pub(crate) use search::cmd_onboard;
pub(crate) use search::cmd_query;
pub(crate) use search::cmd_related;
pub(crate) use search::cmd_scout;
pub(crate) use search::cmd_similar;
pub(crate) use search::cmd_where;
pub(crate) use search::GatherContext;

// -- graph --
pub(crate) use graph::build_callees;
pub(crate) use graph::build_callers;
pub(crate) use graph::build_deps_forward;
pub(crate) use graph::build_deps_reverse;
pub(crate) use graph::build_test_map;
pub(crate) use graph::build_test_map_output;
pub(crate) use graph::cmd_callees;
pub(crate) use graph::cmd_callers;
pub(crate) use graph::cmd_deps;
pub(crate) use graph::cmd_explain;
pub(crate) use graph::cmd_impact;
pub(crate) use graph::cmd_impact_diff;
pub(crate) use graph::cmd_test_map;
pub(crate) use graph::cmd_trace;

// -- review --
pub(crate) use review::build_dead_output;
pub(crate) use review::cmd_affected;
pub(crate) use review::cmd_ci;
pub(crate) use review::cmd_dead;
pub(crate) use review::cmd_health;
pub(crate) use review::cmd_review;
pub(crate) use review::cmd_suggest;

// -- index --
pub(crate) use index::build_hnsw_base_index;
pub(crate) use index::build_hnsw_index_owned;
pub(crate) use index::build_stale;
pub(crate) use index::build_stats;
pub(crate) use index::cmd_gc;
pub(crate) use index::cmd_index;
pub(crate) use index::cmd_stale;
pub(crate) use index::cmd_stats;

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
pub(crate) use infra::cmd_cache;
#[cfg(feature = "convert")]
pub(crate) use infra::cmd_convert;
pub(crate) use infra::cmd_doctor;
pub(crate) use infra::cmd_hook;
pub(crate) use infra::cmd_init;
pub(crate) use infra::cmd_model;
pub(crate) use infra::cmd_ping;
pub(crate) use infra::cmd_project;
pub(crate) use infra::cmd_ref;
pub(crate) use infra::cmd_slot;
pub(crate) use infra::cmd_status;
pub(crate) use infra::cmd_telemetry;
pub(crate) use infra::cmd_telemetry_reset;
pub(crate) use infra::CacheCommand;
pub(crate) use infra::HookCommand;
pub(crate) use infra::ModelCommand;
pub(crate) use infra::ProjectCommand;
pub(crate) use infra::RefCommand;
pub(crate) use infra::SlotCommand;

// -- train --
pub(crate) use train::cmd_export_model;
pub(crate) use train::cmd_plan;
pub(crate) use train::cmd_task;
pub(crate) use train::cmd_train_data;
pub(crate) use train::cmd_train_pairs;

// -- eval --
pub(crate) use eval::{cmd_eval, EvalCmdArgs};

// ---------------------------------------------------------------------------
// Shared token-packing utilities (used by both CLI commands and batch handlers)
// ---------------------------------------------------------------------------

/// Fetch content for named chunks from store, pack into token budget, return content map.
///
/// Shared by scout and onboard (both CLI and batch) -- the "fetch by name, build triples,
/// pack into budget" pattern. Returns `(content_map, tokens_used)`.
///
/// `named_items` is a list of `(name, score)` pairs. Content is fetched from `store` via
/// `get_chunks_by_names_batch`. Items without content in the store are silently dropped.
pub(crate) fn fetch_and_pack_content<Mode>(
    store: &cqs::Store<Mode>,
    embedder: &cqs::Embedder,
    named_items: &[(String, f32)],
    budget: usize,
) -> (std::collections::HashMap<String, String>, usize) {
    let _span =
        tracing::info_span!("fetch_and_pack_content", budget, items = named_items.len()).entered();

    let all_names: Vec<&str> = named_items.iter().map(|(n, _)| n.as_str()).collect();
    let chunks_by_name = match store.get_chunks_by_names_batch(&all_names) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for token packing");
            std::collections::HashMap::new()
        }
    };

    // Build (name, content, score) triples for items with available content
    let items: Vec<(String, String, f32)> = named_items
        .iter()
        .filter_map(|(name, score)| {
            let content = chunks_by_name.get(name.as_str())?.first()?.content.clone();
            Some((name.clone(), content, *score))
        })
        .collect();

    let texts: Vec<&str> = items
        .iter()
        .map(|(_, content, _)| content.as_str())
        .collect();
    let counts = count_tokens_batch(embedder, &texts);
    let (packed, used) = token_pack(items, &counts, budget, 0, |&(_, _, score)| score);

    let content_map: std::collections::HashMap<String, String> = packed
        .into_iter()
        .map(|(name, content, _)| (name, content))
        .collect();

    tracing::info!(
        packed = content_map.len(),
        tokens = used,
        budget,
        "Content packed"
    );
    (content_map, used)
}

/// Inject packed content into scout-style JSON (`file_groups[].chunks[].content`).
///
/// Mutates `json` in place, adding a `content` field to chunks whose names
/// appear in `content_map`.
pub(crate) fn inject_content_into_scout_json(
    json: &mut serde_json::Value,
    content_map: &std::collections::HashMap<String, String>,
) {
    if let Some(groups) = json.get_mut("file_groups").and_then(|v| v.as_array_mut()) {
        for group in groups.iter_mut() {
            if let Some(chunks) = group.get_mut("chunks").and_then(|v| v.as_array_mut()) {
                for chunk in chunks.iter_mut() {
                    if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
                        if let Some(content) = content_map.get(name) {
                            chunk["content"] = serde_json::json!(content);
                        }
                    }
                }
            }
        }
    }
}

/// Tag every chunk-shaped object in a scout-style JSON tree as user-code. (#1167)
///
/// Scout / onboard / where / plan only query the user's project store, so
/// every chunk is `trust_level: "user-code"`. Reference-aware commands
/// (search, gather) thread the origin through `to_json_with_origin` instead.
///
/// SEC-V1.30.1-4: recursive visitor — any object with the chunk-shape
/// signature (presence of `name` AND `file` AND a numeric `line_start`)
/// is tagged. Future scout / onboard surfaces that grow new chunk-bearing
/// keys (e.g. `dependents[]`, `examples[]`, top-level `chunks[]`) are
/// tagged automatically; the previous shape-coupled walker silently
/// no-oped on anything outside `entry_point` / `call_chain` / `callers`
/// / `file_groups[].chunks[]`.
pub(crate) fn tag_user_code_trust_level(json: &mut serde_json::Value) {
    let _span = tracing::info_span!("tag_user_code_trust_level").entered();

    fn looks_like_chunk(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
        obj.contains_key("name")
            && obj.contains_key("file")
            && obj.get("line_start").is_some_and(|v| v.is_number())
    }

    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if looks_like_chunk(map) {
                    map.insert(
                        "trust_level".to_string(),
                        serde_json::Value::String("user-code".to_string()),
                    );
                }
                for (_k, v) in map.iter_mut() {
                    walk(v);
                }
            }
            serde_json::Value::Array(arr) => {
                for v in arr.iter_mut() {
                    walk(v);
                }
            }
            _ => {}
        }
    }

    walk(json);
}

/// Inject packed content into onboard-style JSON (`entry_point`, `call_chain[]`, `callers[]`).
///
/// Replaces content fields for entries whose names appear in `content_map`.
pub(crate) fn inject_content_into_onboard_json(
    json: &mut serde_json::Value,
    content_map: &std::collections::HashMap<String, String>,
    result: &cqs::OnboardResult,
) {
    // entry_point
    if let Some(ep) = json.get_mut("entry_point") {
        if let Some(content) = content_map.get(&result.entry_point.name) {
            ep["content"] = serde_json::json!(content);
        }
    }
    // call_chain
    if let Some(chain) = json.get_mut("call_chain").and_then(|v| v.as_array_mut()) {
        for (i, entry) in chain.iter_mut().enumerate() {
            if let Some(c) = result.call_chain.get(i) {
                if let Some(content) = content_map.get(&c.name) {
                    entry["content"] = serde_json::json!(content);
                }
            }
        }
    }
    // callers
    if let Some(callers) = json.get_mut("callers").and_then(|v| v.as_array_mut()) {
        for (i, entry) in callers.iter_mut().enumerate() {
            if let Some(c) = result.callers.get(i) {
                if let Some(content) = content_map.get(&c.name) {
                    entry["content"] = serde_json::json!(content);
                }
            }
        }
    }
}

/// Build scored `(name, score)` pairs for onboard entries (entry_point + call_chain + callers).
///
/// Entry point gets score 1.0, call chain entries get `1/(depth+1)`, callers get 0.3.
pub(crate) fn onboard_scored_names(result: &cqs::OnboardResult) -> Vec<(String, f32)> {
    let mut items: Vec<(String, f32)> = Vec::new();
    items.push((result.entry_point.name.clone(), 1.0));
    for e in &result.call_chain {
        items.push((e.name.clone(), 1.0 / (e.depth as f32 + 1.0)));
    }
    for c in &result.callers {
        items.push((c.name.clone(), 0.3));
    }
    items
}

/// Build scored `(name, score)` pairs from scout file groups.
///
/// Score for each chunk is `relevance_score * search_score`.
pub(crate) fn scout_scored_names(result: &cqs::ScoutResult) -> Vec<(String, f32)> {
    result
        .file_groups
        .iter()
        .flat_map(|g| {
            g.chunks
                .iter()
                .map(move |c| (c.name.clone(), g.relevance_score * c.search_score))
        })
        .collect()
}

/// Token-pack gather chunks by score within a token budget.
///
/// Shared by CLI gather and batch gather. Returns `(packed_chunks, tokens_used)`.
pub(crate) fn pack_gather_chunks(
    chunks: Vec<cqs::GatheredChunk>,
    embedder: &cqs::Embedder,
    budget: usize,
    json_overhead: usize,
) -> (Vec<cqs::GatheredChunk>, usize) {
    let _span = tracing::info_span!("pack_gather_chunks", budget, count = chunks.len()).entered();
    let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let counts = count_tokens_batch(embedder, &texts);
    let (packed, used) = token_pack(chunks, &counts, budget, json_overhead, |c| c.score);
    tracing::info!(
        packed = packed.len(),
        tokens = used,
        budget,
        "Gather chunks packed"
    );
    (packed, used)
}

/// Token info for display: `(used, budget)`.
pub(crate) type TokenInfo = Option<(usize, usize)>;

/// Inject `token_count` and `token_budget` fields into a JSON value.
///
/// No-op when `token_info` is `None`. Used by scout, onboard, and batch
/// handlers that build JSON via lib functions and then append token metadata.
pub(crate) fn inject_token_info(json: &mut serde_json::Value, token_info: TokenInfo) {
    if let Some((used, budget)) = token_info {
        json["token_count"] = serde_json::json!(used);
        json["token_budget"] = serde_json::json!(budget);
    }
}

/// Pack results into a token budget, keeping highest-scoring results.
///
/// Generic over result type -- works for `UnifiedResult`, `TaggedResult`, etc.
/// Both CLI search and batch search use this.
pub(crate) fn token_pack_results<T>(
    results: Vec<T>,
    budget: usize,
    json_overhead: usize,
    embedder: &cqs::Embedder,
    text_fn: impl Fn(&T) -> &str,
    score_fn: impl Fn(&T) -> f32,
    label: &str,
) -> (Vec<T>, TokenInfo) {
    let _span = tracing::info_span!("token_pack_results", budget, label).entered();

    let texts: Vec<&str> = results.iter().map(&text_fn).collect();
    let token_counts = count_tokens_batch(embedder, &texts);
    let (packed, used) = token_pack(results, &token_counts, budget, json_overhead, score_fn);
    tracing::info!(
        chunks = packed.len(),
        tokens = used,
        budget,
        label,
        "Token-budgeted results"
    );
    (packed, Some((used, budget)))
}

// ---------------------------------------------------------------------------
// Core token utilities
// ---------------------------------------------------------------------------

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

    // Build index order sorted by score descending. Secondary sort on the
    // original index keeps equal-score items deterministically ordered, so
    // the subsequent packing picks the same items on every invocation.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        score_fn(&items[b])
            .total_cmp(&score_fn(&items[a]))
            .then(a.cmp(&b))
    });

    // Greedy pack in score order, tracking which indices to keep.
    //
    // P1.18: when an oversized item appears mid-stream we `continue` rather
    // than `break` so subsequent (smaller, lower-scored) items can still
    // fit into the remaining budget. Score-ordered packing already prefers
    // higher-relevance items; the greedy fall-through is the right rounding
    // when one mid-list item won't fit.
    let mut used: usize = 0;
    let mut kept_any = false;
    let mut keep: Vec<bool> = vec![false; items.len()];
    for idx in order {
        let tokens = token_counts[idx] + json_overhead_per_item;
        if used + tokens > budget && kept_any {
            // Skip this oversized item but keep probing — smaller items
            // later in score order may still fit.
            continue;
        }
        if !kept_any && tokens > budget {
            // Always include at least one result, but cap at 10x budget to avoid
            // pathological cases (e.g., 50K-token item with 300-token budget).
            // When budget == 0, skip the 10x guard (0 * 10 == 0, which would reject
            // every item) and include the first item unconditionally.
            if budget > 0 && tokens > budget * 10 {
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
    // Secondary sort on the original index keeps equal-score items
    // deterministically ordered across process invocations.
    order.sort_by(|&a, &b| score_fn(b).total_cmp(&score_fn(a)).then(a.cmp(&b)));

    let mut used = 0;
    let mut kept = Vec::new();
    for idx in order {
        let cost = token_counts[idx] + overhead_per_item;
        if used + cost > budget && !kept.is_empty() {
            break;
        }
        // Mirror token_pack's 10x guard: skip items that vastly exceed budget
        // to avoid pathological cases (e.g., 50K-token item with 300-token budget)
        if kept.is_empty() && cost > budget * 10 {
            tracing::debug!(cost, budget, idx, "First item exceeds 10x budget, skipping");
            continue;
        }
        used += cost;
        kept.push(idx);
    }
    kept.sort(); // preserve original order
    (kept, used)
}

/// Read diff text from stdin, capped at `CQS_MAX_DIFF_BYTES` (default 50 MiB).
/// P3 #107: shares the same env knob with `git diff` subprocess output.
pub(crate) fn read_stdin() -> anyhow::Result<String> {
    use std::io::Read;
    let max_stdin_size = crate::cli::limits::max_diff_bytes();
    let mut buf = String::new();
    std::io::stdin()
        .take(max_stdin_size as u64 + 1)
        .read_to_string(&mut buf)?;
    if buf.len() > max_stdin_size {
        anyhow::bail!(
            "stdin input exceeds {} MiB limit (CQS_MAX_DIFF_BYTES)",
            max_stdin_size / 1024 / 1024
        );
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

    // P3 #107: shared cap with stdin (CQS_MAX_DIFF_BYTES, default 50 MiB).
    let max_diff_size = crate::cli::limits::max_diff_bytes();
    if output.stdout.len() > max_diff_size {
        anyhow::bail!(
            "git diff output exceeds {} MiB limit (CQS_MAX_DIFF_BYTES; {} bytes seen)",
            max_diff_size / 1024 / 1024,
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
        // Scores: a=1, b=5, c=3, d=4, e=2 -> picks b,d,c (top 3 by score)
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

    // TC-6: token_pack zero budget includes first item unconditionally
    #[test]
    fn test_token_pack_zero_budget_includes_first() {
        let items = vec!["a", "b"];
        let counts = vec![10, 20];
        let (packed, used) = token_pack(items, &counts, 0, 0, |_| 1.0);
        // "Always includes at least one item" — even with budget=0
        assert_eq!(packed.len(), 1);
        assert_eq!(used, 10);
    }

    // TC-6: token_pack 10x guard still works when budget > 0
    #[test]
    fn test_token_pack_10x_guard_nonzero_budget() {
        let items = vec!["huge", "small"];
        let counts = vec![5000, 10];
        // Budget 30, first item by score is "huge" (5000 tokens > 30*10=300) — skip it
        let (packed, used) = token_pack(items, &counts, 30, 0, |item| match *item {
            "huge" => 2.0,
            "small" => 1.0,
            _ => 0.0,
        });
        assert_eq!(packed, vec!["small"]);
        assert_eq!(used, 10);
    }

    // AC-11: index_pack 10x guard skips pathologically large first item
    #[test]
    fn test_index_pack_10x_guard() {
        let counts = vec![5000, 10];
        // Budget 30: first by score is index 0 (5000 tokens > 30*10=300) — skip it
        let (indices, used) = index_pack(&counts, 30, 0, |i| if i == 0 { 2.0 } else { 1.0 });
        assert_eq!(indices, vec![1]);
        assert_eq!(used, 10);
    }

    // AC-11: index_pack still includes moderately-over-budget first item (< 10x)
    #[test]
    fn test_index_pack_includes_moderate_overbudget() {
        let counts = vec![100]; // 100 > budget 30, but 100 < 30*10=300
        let (indices, used) = index_pack(&counts, 30, 0, |_| 1.0);
        assert_eq!(indices, vec![0]);
        assert_eq!(used, 100);
    }

    // HP-2: inject_token_info adds fields when Some
    #[test]
    fn test_inject_token_info_some() {
        let mut json = serde_json::json!({"results": []});
        inject_token_info(&mut json, Some((150, 300)));
        assert_eq!(json["token_count"], 150);
        assert_eq!(json["token_budget"], 300);
    }

    // HP-2: inject_token_info is no-op when None
    #[test]
    fn test_inject_token_info_none() {
        let mut json = serde_json::json!({"results": []});
        inject_token_info(&mut json, None);
        assert!(json.get("token_count").is_none());
        assert!(json.get("token_budget").is_none());
    }

    // HP-2: inject_content_into_scout_json injects content by chunk name
    #[test]
    fn test_inject_content_into_scout_json() {
        let mut json = serde_json::json!({
            "file_groups": [{
                "file": "src/a.rs",
                "chunks": [
                    {"name": "foo", "signature": "fn foo()"},
                    {"name": "bar", "signature": "fn bar()"}
                ]
            }]
        });
        let mut content_map = std::collections::HashMap::new();
        content_map.insert("foo".to_string(), "fn foo() { 42 }".to_string());
        // "bar" deliberately not in map

        inject_content_into_scout_json(&mut json, &content_map);

        let chunks = json["file_groups"][0]["chunks"].as_array().unwrap();
        assert_eq!(chunks[0]["content"], "fn foo() { 42 }");
        assert!(chunks[1].get("content").is_none());
    }

    // HP-2: inject_content_into_scout_json no-op on missing file_groups
    #[test]
    fn test_inject_content_into_scout_json_no_groups() {
        let mut json = serde_json::json!({"other": 1});
        let content_map = std::collections::HashMap::new();
        inject_content_into_scout_json(&mut json, &content_map);
        // Should not panic, json unchanged
        assert_eq!(json, serde_json::json!({"other": 1}));
    }

    // HP-2: inject_content_into_onboard_json injects into entry_point, call_chain, callers
    #[test]
    fn test_inject_content_into_onboard_json() {
        use std::path::PathBuf;
        let result = cqs::OnboardResult {
            concept: "test".into(),
            entry_point: cqs::OnboardEntry {
                name: "entry".into(),
                file: PathBuf::from("a.rs"),
                line_start: 1,
                line_end: 10,
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                signature: "fn entry()".into(),
                content: String::new(),
                depth: 0,
            },
            call_chain: vec![cqs::OnboardEntry {
                name: "callee".into(),
                file: PathBuf::from("b.rs"),
                line_start: 1,
                line_end: 5,
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                signature: "fn callee()".into(),
                content: String::new(),
                depth: 1,
            }],
            callers: vec![cqs::OnboardEntry {
                name: "caller".into(),
                file: PathBuf::from("c.rs"),
                line_start: 1,
                line_end: 5,
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                signature: "fn caller()".into(),
                content: String::new(),
                depth: 0,
            }],
            key_types: vec![],
            tests: vec![],
            summary: cqs::OnboardSummary {
                total_items: 3,
                files_covered: 3,
                callee_depth: 1,
                tests_found: 0,
                callees_truncated: 0,
                callers_truncated: 0,
            },
        };

        let mut json = serde_json::json!({
            "entry_point": {"name": "entry"},
            "call_chain": [{"name": "callee"}],
            "callers": [{"name": "caller"}]
        });
        let mut content_map = std::collections::HashMap::new();
        content_map.insert("entry".to_string(), "fn entry() {}".to_string());
        content_map.insert("callee".to_string(), "fn callee() {}".to_string());
        // "caller" not in map

        inject_content_into_onboard_json(&mut json, &content_map, &result);

        assert_eq!(json["entry_point"]["content"], "fn entry() {}");
        assert_eq!(json["call_chain"][0]["content"], "fn callee() {}");
        assert!(json["callers"][0].get("content").is_none());
    }

    // HP-9: onboard_scored_names scoring logic
    #[test]
    fn test_onboard_scored_names() {
        use std::path::PathBuf;
        let result = cqs::OnboardResult {
            concept: "test".into(),
            entry_point: cqs::OnboardEntry {
                name: "entry".into(),
                file: PathBuf::from("a.rs"),
                line_start: 1,
                line_end: 10,
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                signature: String::new(),
                content: String::new(),
                depth: 0,
            },
            call_chain: vec![
                cqs::OnboardEntry {
                    name: "depth0".into(),
                    file: PathBuf::from("b.rs"),
                    line_start: 1,
                    line_end: 5,
                    language: cqs::language::Language::Rust,
                    chunk_type: cqs::language::ChunkType::Function,
                    signature: String::new(),
                    content: String::new(),
                    depth: 0,
                },
                cqs::OnboardEntry {
                    name: "depth1".into(),
                    file: PathBuf::from("c.rs"),
                    line_start: 1,
                    line_end: 5,
                    language: cqs::language::Language::Rust,
                    chunk_type: cqs::language::ChunkType::Function,
                    signature: String::new(),
                    content: String::new(),
                    depth: 1,
                },
                cqs::OnboardEntry {
                    name: "depth3".into(),
                    file: PathBuf::from("d.rs"),
                    line_start: 1,
                    line_end: 5,
                    language: cqs::language::Language::Rust,
                    chunk_type: cqs::language::ChunkType::Function,
                    signature: String::new(),
                    content: String::new(),
                    depth: 3,
                },
            ],
            callers: vec![cqs::OnboardEntry {
                name: "caller".into(),
                file: PathBuf::from("e.rs"),
                line_start: 1,
                line_end: 5,
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                signature: String::new(),
                content: String::new(),
                depth: 0,
            }],
            key_types: vec![],
            tests: vec![],
            summary: cqs::OnboardSummary {
                total_items: 5,
                files_covered: 5,
                callee_depth: 3,
                tests_found: 0,
                callees_truncated: 0,
                callers_truncated: 0,
            },
        };

        let scored = onboard_scored_names(&result);
        assert_eq!(scored.len(), 5);

        // Entry point: score 1.0
        assert_eq!(scored[0], ("entry".to_string(), 1.0));

        // Call chain: 1/(depth+1)
        assert_eq!(scored[1], ("depth0".to_string(), 1.0 / 1.0)); // depth 0 → 1.0
        assert_eq!(scored[2], ("depth1".to_string(), 1.0 / 2.0)); // depth 1 → 0.5
        assert_eq!(scored[3], ("depth3".to_string(), 1.0 / 4.0)); // depth 3 → 0.25

        // Callers: score 0.3
        assert_eq!(scored[4], ("caller".to_string(), 0.3));
    }

    // HP-9: scout_scored_names scoring logic
    #[test]
    fn test_scout_scored_names() {
        let result = cqs::ScoutResult {
            file_groups: vec![
                cqs::FileGroup {
                    file: std::path::PathBuf::from("a.rs"),
                    relevance_score: 0.8,
                    chunks: vec![
                        cqs::ScoutChunk {
                            name: "foo".into(),
                            chunk_type: cqs::language::ChunkType::Function,
                            signature: String::new(),
                            line_start: 1,
                            role: cqs::ChunkRole::ModifyTarget,
                            caller_count: 3,
                            test_count: 1,
                            search_score: 0.9,
                        },
                        cqs::ScoutChunk {
                            name: "bar".into(),
                            chunk_type: cqs::language::ChunkType::Function,
                            signature: String::new(),
                            line_start: 10,
                            role: cqs::ChunkRole::Dependency,
                            caller_count: 0,
                            test_count: 0,
                            search_score: 0.5,
                        },
                    ],
                    is_stale: false,
                },
                cqs::FileGroup {
                    file: std::path::PathBuf::from("b.rs"),
                    relevance_score: 0.4,
                    chunks: vec![cqs::ScoutChunk {
                        name: "baz".into(),
                        chunk_type: cqs::language::ChunkType::Function,
                        signature: String::new(),
                        line_start: 1,
                        role: cqs::ChunkRole::Dependency,
                        caller_count: 1,
                        test_count: 0,
                        search_score: 0.7,
                    }],
                    is_stale: true,
                },
            ],
            relevant_notes: vec![],
            summary: cqs::ScoutSummary {
                total_files: 2,
                total_functions: 3,
                untested_count: 1,
                stale_count: 1,
            },
        };

        let scored = scout_scored_names(&result);
        assert_eq!(scored.len(), 3);

        // Score = relevance_score * search_score
        assert_eq!(scored[0].0, "foo");
        assert!((scored[0].1 - 0.8 * 0.9).abs() < 1e-6); // 0.72

        assert_eq!(scored[1].0, "bar");
        assert!((scored[1].1 - 0.8 * 0.5).abs() < 1e-6); // 0.40

        assert_eq!(scored[2].0, "baz");
        assert!((scored[2].1 - 0.4 * 0.7).abs() < 1e-6); // 0.28
    }

    // HP-9: scout_scored_names with empty result
    #[test]
    fn test_scout_scored_names_empty() {
        let result = cqs::ScoutResult {
            file_groups: vec![],
            relevant_notes: vec![],
            summary: cqs::ScoutSummary {
                total_files: 0,
                total_functions: 0,
                untested_count: 0,
                stale_count: 0,
            },
        };
        let scored = scout_scored_names(&result);
        assert!(scored.is_empty());
    }

    // TC-12: token_pack with NaN scores — NaN sorts as highest via total_cmp,
    // so NaN-scored items are treated as top priority (picked first).
    // This documents the current behavior: NaN is NOT deprioritized.
    #[test]
    fn test_token_pack_nan_scores_sorted_first() {
        let items = vec!["nan_item", "good_item"];
        let counts = vec![10, 10];
        // Budget fits only 1 item; NaN sorts above 1.0 via total_cmp
        let (packed, used) = token_pack(items, &counts, 10, 0, |item| match *item {
            "nan_item" => f32::NAN,
            "good_item" => 1.0,
            _ => 0.0,
        });
        assert_eq!(packed.len(), 1);
        // NaN is picked first (total_cmp ranks NaN > all finite values)
        assert_eq!(packed[0], "nan_item");
        assert_eq!(used, 10);
    }

    // TC-12: token_pack with all NaN scores — at-least-one guarantee still holds
    #[test]
    fn test_token_pack_all_nan_includes_first() {
        let items = vec!["a", "b"];
        let counts = vec![10, 10];
        let (packed, used) = token_pack(items, &counts, 10, 0, |_| f32::NAN);
        // At-least-one guarantee: first item by sort order is included
        assert_eq!(packed.len(), 1);
        assert_eq!(used, 10);
    }

    // TC-12: token_pack with NaN and valid items when budget fits all — NaN items included
    #[test]
    fn test_token_pack_nan_mixed_all_fit() {
        let items = vec!["a", "b", "c"];
        let counts = vec![10, 10, 10];
        let (packed, used) = token_pack(items, &counts, 100, 0, |item| match *item {
            "a" => f32::NAN,
            "b" => 2.0,
            "c" => 1.0,
            _ => 0.0,
        });
        // All fit in budget — NaN items are included
        assert_eq!(packed.len(), 3);
        assert_eq!(used, 30);
    }

    // HP-3: run_git_diff rejects base refs starting with '-' (flag injection)
    #[test]
    fn test_run_git_diff_rejects_dash_prefix() {
        let result = run_git_diff(Some("--exec=whoami"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("must not start with '-'"),
            "Expected dash-prefix rejection, got: {}",
            err
        );
    }

    // HP-3: run_git_diff rejects base refs containing null bytes
    #[test]
    fn test_run_git_diff_rejects_null_bytes() {
        let result = run_git_diff(Some("main\0--exec=whoami"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("contain null bytes"),
            "Expected null-byte rejection, got: {}",
            err
        );
    }

    // SEC-V1.30.1-4: recursive visitor still tags the four legacy shapes
    // (entry_point, call_chain[], callers[], file_groups[].chunks[]).
    #[test]
    fn tag_user_code_visits_legacy_onboard_shapes() {
        let mut json = serde_json::json!({
            "entry_point": {"name": "ep", "file": "a.rs", "line_start": 1},
            "call_chain": [
                {"name": "c1", "file": "b.rs", "line_start": 2}
            ],
            "callers": [
                {"name": "caller", "file": "c.rs", "line_start": 3}
            ]
        });
        tag_user_code_trust_level(&mut json);
        assert_eq!(json["entry_point"]["trust_level"], "user-code");
        assert_eq!(json["call_chain"][0]["trust_level"], "user-code");
        assert_eq!(json["callers"][0]["trust_level"], "user-code");
    }

    // SEC-V1.30.1-4: recursive visitor still tags scout-shape nesting.
    #[test]
    fn tag_user_code_visits_legacy_scout_shape() {
        let mut json = serde_json::json!({
            "file_groups": [
                {
                    "file": "a.rs",
                    "chunks": [
                        {"name": "foo", "file": "a.rs", "line_start": 10}
                    ]
                }
            ]
        });
        tag_user_code_trust_level(&mut json);
        assert_eq!(
            json["file_groups"][0]["chunks"][0]["trust_level"],
            "user-code"
        );
    }

    // SEC-V1.30.1-4: chunks reachable through arbitrary new keys are
    // tagged — the contract is "every chunk-shaped object", not "every
    // chunk under one of these four keys."
    #[test]
    fn tag_user_code_visits_arbitrary_nested_chunks() {
        let mut json = serde_json::json!({
            "entry_point": {"name": "foo", "file": "a.rs", "line_start": 10},
            "future_field": {
                "examples": [
                    {"name": "bar", "file": "b.rs", "line_start": 20},
                ]
            }
        });
        tag_user_code_trust_level(&mut json);
        assert_eq!(json["entry_point"]["trust_level"], "user-code");
        assert_eq!(
            json["future_field"]["examples"][0]["trust_level"],
            "user-code"
        );
    }

    // SEC-V1.30.1-4: deep object nesting — visitor descends arbitrarily.
    #[test]
    fn tag_user_code_visits_deeply_nested_chunks() {
        let mut json = serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {
                        "name": "deep",
                        "file": "x.rs",
                        "line_start": 99
                    }
                }
            }
        });
        tag_user_code_trust_level(&mut json);
        assert_eq!(
            json["level1"]["level2"]["level3"]["trust_level"],
            "user-code"
        );
    }

    // SEC-V1.30.1-4: nested array of chunk-shaped objects — every
    // element gets tagged.
    #[test]
    fn tag_user_code_visits_nested_array_of_chunks() {
        let mut json = serde_json::json!({
            "wrapper": {
                "list": [
                    {"name": "a", "file": "a.rs", "line_start": 1},
                    {"name": "b", "file": "b.rs", "line_start": 2},
                    {"name": "c", "file": "c.rs", "line_start": 3}
                ]
            }
        });
        tag_user_code_trust_level(&mut json);
        let arr = json["wrapper"]["list"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        for entry in arr {
            assert_eq!(entry["trust_level"], "user-code");
        }
    }

    // SEC-V1.30.1-4: mixed shape — tag chunk-shaped objects, leave
    // metadata objects untouched.
    #[test]
    fn tag_user_code_mixed_shape_only_tags_chunks() {
        let mut json = serde_json::json!({
            "summary": {"total": 5, "stale": 1},
            "entry": {"name": "foo", "file": "a.rs", "line_start": 1},
            "siblings": [
                {"name": "x", "file": "x.rs", "line_start": 2},
                {"unrelated": true},
                {"name": "without-line", "file": "y.rs"},
                {"name": "string-line", "file": "z.rs", "line_start": "10"}
            ]
        });
        tag_user_code_trust_level(&mut json);
        // Chunk-shaped: tagged.
        assert_eq!(json["entry"]["trust_level"], "user-code");
        assert_eq!(json["siblings"][0]["trust_level"], "user-code");
        // Metadata + non-chunk shapes: untouched.
        assert!(json["summary"].get("trust_level").is_none());
        assert!(json["siblings"][1].get("trust_level").is_none());
        assert!(json["siblings"][2].get("trust_level").is_none()); // missing line_start
        assert!(json["siblings"][3].get("trust_level").is_none()); // string line_start
    }

    // SEC-V1.30.1-4: pure-scalar root JSON — visitor is a no-op
    // (no panic, no tag).
    #[test]
    fn tag_user_code_scalar_root_no_op() {
        let mut s = serde_json::Value::String("hello".into());
        tag_user_code_trust_level(&mut s);
        assert_eq!(s, serde_json::Value::String("hello".into()));

        let mut n = serde_json::Value::Number(42.into());
        tag_user_code_trust_level(&mut n);
        assert_eq!(n, serde_json::Value::Number(42.into()));

        let mut b = serde_json::Value::Bool(true);
        tag_user_code_trust_level(&mut b);
        assert_eq!(b, serde_json::Value::Bool(true));

        let mut nul = serde_json::Value::Null;
        tag_user_code_trust_level(&mut nul);
        assert_eq!(nul, serde_json::Value::Null);
    }

    // SEC-V1.30.1-4: top-level array of chunks — visitor descends in.
    #[test]
    fn tag_user_code_array_root() {
        let mut json = serde_json::json!([
            {"name": "a", "file": "a.rs", "line_start": 1},
            {"name": "b", "file": "b.rs", "line_start": 2}
        ]);
        tag_user_code_trust_level(&mut json);
        let arr = json.as_array().unwrap();
        assert_eq!(arr[0]["trust_level"], "user-code");
        assert_eq!(arr[1]["trust_level"], "user-code");
    }

    // SEC-V1.30.1-4: object that is NOT chunk-shaped — no tag added.
    #[test]
    fn tag_user_code_does_not_tag_non_chunk_objects() {
        let mut json = serde_json::json!({"meta": {"version": 1}});
        tag_user_code_trust_level(&mut json);
        assert!(json["meta"].get("trust_level").is_none());
        assert!(json.get("trust_level").is_none());
    }
}
