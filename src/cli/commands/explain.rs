//! Explain command — generate a function card
//!
//! Core logic is in `build_explain_data()` so batch mode can reuse it
//! without duplicating ~130 lines.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use cqs::index::VectorIndex;
use cqs::store::{CallerInfo, ChunkSummary, SearchResult, Store};
use cqs::{compute_hints, normalize_path, FunctionHints, HnswIndex, SearchFilter};

use crate::cli::staleness;

// ─── Shared core ────────────────────────────────────────────────────────────

/// All data needed to render an explain card (JSON or terminal).
pub(crate) struct ExplainData {
    pub chunk: ChunkSummary,
    pub callers: Vec<CallerInfo>,
    pub callees: Vec<(String, u32)>,
    pub similar: Vec<SearchResult>,
    pub hints: Option<FunctionHints>,
    /// When true, the target chunk's content should be included in output.
    pub include_target_content: bool,
    /// IDs of similar chunks whose content fits within the token budget.
    pub similar_content_ids: Option<HashSet<String>>,
    /// (tokens_used, budget) if `--tokens` was requested.
    pub token_info: Option<(usize, usize)>,
}

/// Build explain data: resolve target, fetch callers/callees/similar, compute hints,
/// and optionally pack content within a token budget.
///
/// Shared between CLI `cmd_explain` and batch `dispatch_explain`.
///
/// * `index` — pre-loaded vector index (batch passes its cached one, CLI passes `None`
///   to load fresh).
/// * `embedder` — required only when `max_tokens` is `Some`. Batch passes its cached one;
///   CLI passes `None` to create a fresh one internally.
pub(crate) fn build_explain_data(
    store: &Store,
    cqs_dir: &Path,
    target: &str,
    max_tokens: Option<usize>,
    index: Option<Option<&dyn VectorIndex>>,
    embedder: Option<&cqs::Embedder>,
) -> Result<ExplainData> {
    // Resolve target
    let resolved = cqs::resolve_target(store, target)?;
    let chunk = resolved.chunk;

    // Get callers
    let callers = match store.get_callers_full(&chunk.name) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callers in explain");
            Vec::new()
        }
    };

    // Get callees — scope to the resolved chunk's file to avoid ambiguity
    let chunk_file = chunk.file.to_string_lossy();
    let callees = match store.get_callees_full(&chunk.name, Some(&chunk_file)) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, name = chunk.name, "Failed to get callees in explain");
            Vec::new()
        }
    };

    // Get similar (top 3) using embedding
    let similar = match store.get_chunk_with_embedding(&chunk.id)? {
        Some((_, embedding)) => {
            let filter = SearchFilter {
                note_weight: 0.0,
                ..Default::default()
            };
            // Use caller-provided index or load fresh
            let owned_index;
            let idx: Option<&dyn VectorIndex> = match index {
                Some(idx) => idx,
                None => {
                    owned_index = HnswIndex::try_load(cqs_dir);
                    owned_index.as_deref()
                }
            };
            let sim_results = store.search_filtered_with_index(
                &embedding, &filter, 4, // +1 to exclude self
                0.3, idx,
            )?;
            sim_results
                .into_iter()
                .filter(|r| r.chunk.id != chunk.id)
                .take(3)
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    // Compute hints (only for function/method chunk types)
    let hints = if chunk.chunk_type.is_callable() {
        match compute_hints(store, &chunk.name, Some(callers.len())) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(function = %chunk.name, error = %e, "Failed to compute hints");
                None
            }
        }
    } else {
        None
    };

    // Token budget: compute which content fits
    let (include_target_content, similar_content_ids, token_info) = if let Some(budget) = max_tokens
    {
        // Need an embedder for token counting
        let owned_embedder;
        let emb = match embedder {
            Some(e) => e,
            None => {
                owned_embedder = cqs::Embedder::new()?;
                &owned_embedder
            }
        };
        let _pack_span = tracing::info_span!("token_pack_explain", budget).entered();

        // Priority 1: target chunk content (always included)
        let target_tokens = super::count_tokens(emb, &chunk.content, &chunk.name);

        // Priority 2: similar chunks' content — pack remaining budget
        let remaining = budget.saturating_sub(target_tokens);
        let indexed: Vec<(usize, f32)> = similar
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.score))
            .collect();
        let texts: Vec<&str> = indexed
            .iter()
            .map(|&(i, _)| similar[i].chunk.content.as_str())
            .collect();
        let token_counts = super::count_tokens_batch(emb, &texts);
        let (packed, sim_used) =
            super::token_pack(indexed, &token_counts, remaining, 0, |&(_, score)| score);
        let sim_included: HashSet<String> = packed
            .into_iter()
            .map(|(i, _)| similar[i].chunk.id.clone())
            .collect();

        let used = target_tokens + sim_used;
        tracing::info!(
            tokens = used,
            budget,
            similar_with_content = sim_included.len(),
            "Token-budgeted explain"
        );
        (true, Some(sim_included), Some((used, budget)))
    } else {
        (false, None, None)
    };

    Ok(ExplainData {
        chunk,
        callers,
        callees,
        similar,
        hints,
        include_target_content,
        similar_content_ids,
        token_info,
    })
}

/// Build JSON output from explain data.
///
/// Shared between CLI `cmd_explain --json` and batch `dispatch_explain`.
pub(crate) fn explain_to_json(data: &ExplainData, root: &Path) -> serde_json::Value {
    let chunk = &data.chunk;

    let callers_json: Vec<_> = data
        .callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": normalize_path(&c.file),
                "line": c.line,
            })
        })
        .collect();

    let callees_json: Vec<_> = data
        .callees
        .iter()
        .map(|(name, line)| {
            serde_json::json!({
                "name": name,
                "line": line,
            })
        })
        .collect();

    let similar_json: Vec<_> = data
        .similar
        .iter()
        .map(|r| {
            let mut obj = serde_json::json!({
                "name": r.chunk.name,
                "file": normalize_path(&r.chunk.file),
                "score": r.score,
            });
            if let Some(ref set) = data.similar_content_ids {
                if set.contains(&r.chunk.id) {
                    obj["content"] = serde_json::json!(r.chunk.content);
                }
            }
            obj
        })
        .collect();

    let rel_file = cqs::rel_display(&chunk.file, root);

    let mut output = serde_json::json!({
        "name": chunk.name,
        "file": rel_file,
        "language": chunk.language.to_string(),
        "chunk_type": chunk.chunk_type.to_string(),
        "lines": [chunk.line_start, chunk.line_end],
        "signature": chunk.signature,
        "doc": chunk.doc,
        "callers": callers_json,
        "callees": callees_json,
        "similar": similar_json,
    });

    if data.include_target_content {
        output["content"] = serde_json::json!(chunk.content);
    }

    if let Some(ref h) = data.hints {
        output["hints"] = serde_json::json!({
            "caller_count": h.caller_count,
            "test_count": h.test_count,
            "no_callers": h.caller_count == 0,
            "no_tests": h.test_count == 0,
        });
    }

    if let Some((used, budget)) = data.token_info {
        output["token_count"] = serde_json::json!(used);
        output["token_budget"] = serde_json::json!(budget);
    }

    output
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_explain(
    cli: &crate::cli::Cli,
    target: &str,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_explain", target).entered();
    let (store, root, cqs_dir) = crate::cli::open_project_store()?;

    let data = build_explain_data(&store, &cqs_dir, target, max_tokens, None, None)?;

    // Proactive staleness warning
    if !cli.quiet && !cli.no_stale_check {
        if let Some(file_str) = data.chunk.file.to_str() {
            staleness::warn_stale_results(&store, &[file_str], &root);
        }
    }

    if json {
        let output = explain_to_json(&data, &root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_explain_terminal(&data, &root);
    }

    Ok(())
}

fn print_explain_terminal(data: &ExplainData, root: &Path) {
    use colored::Colorize;

    let chunk = &data.chunk;
    let rel_file = cqs::rel_display(&chunk.file, root);

    let token_label = match data.token_info {
        Some((used, budget)) => format!(" ({} of {} tokens)", used, budget),
        None => String::new(),
    };
    println!(
        "{} ({} {}){}",
        chunk.name.bold(),
        chunk.chunk_type,
        chunk.language,
        token_label,
    );
    println!("{}:{}-{}", rel_file, chunk.line_start, chunk.line_end);

    if let Some(ref h) = data.hints {
        if h.caller_count == 0 || h.test_count == 0 {
            let caller_part = if h.caller_count == 0 {
                format!("{}", "0 callers".yellow())
            } else {
                format!("{} callers", h.caller_count)
            };
            let test_part = if h.test_count == 0 {
                format!("{}", "0 tests".yellow())
            } else {
                format!("{} tests", h.test_count)
            };
            println!("{} | {}", caller_part, test_part);
        } else {
            println!("{} callers | {} tests", h.caller_count, h.test_count);
        }
    }

    if !chunk.signature.is_empty() {
        println!();
        println!("{}", chunk.signature.dimmed());
    }

    if let Some(ref doc) = chunk.doc {
        println!();
        println!("{}", doc.green());
    }

    // Print target content if --tokens is set
    if data.include_target_content {
        println!();
        println!("{}", "\u{2500}".repeat(50));
        println!("{}", chunk.content);
    }

    if !data.callers.is_empty() {
        println!();
        println!("{}", "Callers:".cyan());
        for c in &data.callers {
            let rel = cqs::rel_display(&c.file, root);
            println!("  {} ({}:{})", c.name, rel, c.line);
        }
    }

    if !data.callees.is_empty() {
        println!();
        println!("{}", "Callees:".cyan());
        for (name, _) in &data.callees {
            println!("  {}", name);
        }
    }

    if !data.similar.is_empty() {
        println!();
        println!("{}", "Similar:".cyan());
        for r in &data.similar {
            let rel = cqs::rel_display(&r.chunk.file, root);
            println!(
                "  {} ({}:{}) [{:.2}]",
                r.chunk.name, rel, r.chunk.line_start, r.score
            );
            // Print similar content if within token budget
            if let Some(ref set) = data.similar_content_ids {
                if set.contains(&r.chunk.id) {
                    println!("{}", "\u{2500}".repeat(40));
                    println!("{}", r.chunk.content);
                }
            }
        }
    }
}
