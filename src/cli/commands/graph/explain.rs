//! Explain command — generate a function card
//!
//! Core logic is in `build_explain_data()` so batch mode can reuse it
//! without duplicating ~130 lines.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;

use cqs::index::VectorIndex;
use cqs::store::{CallerInfo, ChunkSummary, SearchResult, Store};
use cqs::{compute_hints, rel_display, FunctionHints, HnswIndex, SearchFilter};

use super::callers::{CalleeEntry, CallerEntry};
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
/// Shared between CLI `cmd_explain` and batch `dispatch_explain`.
/// * `index` — pre-loaded vector index (batch passes its cached one, CLI passes `None`
///   to load fresh).
/// * `embedder` — required only when `max_tokens` is `Some`. Batch passes its cached one;
///   CLI passes `None` to create a fresh one internally.
pub(crate) fn build_explain_data<Mode>(
    store: &Store<Mode>,
    cqs_dir: &Path,
    target: &str,
    max_tokens: Option<usize>,
    index: Option<Option<&dyn VectorIndex>>,
    embedder: Option<&cqs::Embedder>,
    model_config: &cqs::embedder::ModelConfig,
) -> Result<ExplainData> {
    let _span = tracing::info_span!("build_explain_data", target).entered();
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
            let filter = SearchFilter::default();
            // Use caller-provided index or load fresh
            let owned_index;
            let idx: Option<&dyn VectorIndex> = match index {
                Some(idx) => idx,
                None => {
                    owned_index = HnswIndex::try_load_with_ef(cqs_dir, None, store.dim());
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
                owned_embedder = cqs::Embedder::new(model_config.clone())?;
                &owned_embedder
            }
        };
        let _pack_span = tracing::info_span!("token_pack_explain", budget).entered();

        // Priority 1: target chunk content (always included)
        let target_tokens = crate::cli::commands::count_tokens(emb, &chunk.content, &chunk.name);

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
        let token_counts = crate::cli::commands::count_tokens_batch(emb, &texts);
        let (packed, sim_used) = crate::cli::commands::token_pack(
            indexed,
            &token_counts,
            remaining,
            0,
            |&(_, score)| score,
        );
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

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct SimilarEntry {
    pub name: String,
    pub file: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct HintsOutput {
    pub caller_count: usize,
    pub test_count: usize,
    pub no_callers: bool,
    pub no_tests: bool,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ExplainOutput {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub chunk_type: String,
    pub language: String,
    pub signature: String,
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub callers: Vec<CallerEntry>,
    pub callees: Vec<CalleeEntry>,
    pub similar: Vec<SimilarEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<HintsOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}

/// Build typed explain output from explain data.
/// Shared between CLI `cmd_explain --json` and batch `dispatch_explain`.
pub(crate) fn build_explain_output(data: &ExplainData, root: &Path) -> ExplainOutput {
    let _span = tracing::info_span!("build_explain_output", name = %data.chunk.name).entered();
    let chunk = &data.chunk;

    let callers: Vec<CallerEntry> = data
        .callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: rel_display(&c.file, root),
            line_start: c.line,
        })
        .collect();

    let callees: Vec<CalleeEntry> = data
        .callees
        .iter()
        .map(|(name, line)| CalleeEntry {
            name: name.clone(),
            line_start: *line,
        })
        .collect();

    let similar: Vec<SimilarEntry> = data
        .similar
        .iter()
        .map(|r| {
            let content = data.similar_content_ids.as_ref().and_then(|set| {
                if set.contains(&r.chunk.id) {
                    Some(r.chunk.content.clone())
                } else {
                    None
                }
            });
            SimilarEntry {
                name: r.chunk.name.clone(),
                file: rel_display(&r.chunk.file, root),
                score: r.score,
                content,
            }
        })
        .collect();

    let hints = data.hints.as_ref().map(|h| HintsOutput {
        caller_count: h.caller_count,
        test_count: h.test_count,
        no_callers: h.caller_count == 0,
        no_tests: h.test_count == 0,
    });

    let (token_count, token_budget) = match data.token_info {
        Some((used, budget)) => (Some(used), Some(budget)),
        None => (None, None),
    };

    ExplainOutput {
        name: chunk.name.clone(),
        file: cqs::rel_display(&chunk.file, root).to_string(),
        line_start: chunk.line_start,
        line_end: chunk.line_end,
        chunk_type: chunk.chunk_type.to_string(),
        language: chunk.language.to_string(),
        signature: chunk.signature.clone(),
        doc: chunk.doc.clone(),
        content: if data.include_target_content {
            Some(chunk.content.clone())
        } else {
            None
        },
        callers,
        callees,
        similar,
        hints,
        token_count,
        token_budget,
    }
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_explain(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    target: &str,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_explain", target).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    let embedder = if max_tokens.is_some() {
        Some(ctx.embedder()?)
    } else {
        None
    };

    let data = build_explain_data(
        store,
        cqs_dir,
        target,
        max_tokens,
        None,
        embedder,
        ctx.cli.try_model_config()?,
    )?;

    // Proactive staleness warning
    if !ctx.cli.quiet && !ctx.cli.no_stale_check {
        if let Some(file_str) = data.chunk.file.to_str() {
            staleness::warn_stale_results(store, &[file_str], root);
        }
    }

    if json {
        let output = build_explain_output(&data, root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_explain_terminal(&data, root);
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

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn test_explain_output_field_names() {
        let output = ExplainOutput {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 10,
            line_end: 20,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn foo()".into(),
            doc: None,
            content: None,
            callers: vec![],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line_end").is_some());
        assert!(json.get("line").is_none());
        // None fields should be omitted
        assert!(json.get("content").is_none());
        assert!(json.get("hints").is_none());
        assert!(json.get("token_count").is_none());
        assert!(json.get("token_budget").is_none());
    }

    #[test]
    fn test_explain_output_with_hints() {
        let output = ExplainOutput {
            name: "bar".into(),
            file: "src/bar.rs".into(),
            line_start: 1,
            line_end: 5,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn bar()".into(),
            doc: Some("A doc comment".into()),
            content: Some("fn bar() {}".into()),
            callers: vec![CallerEntry {
                name: "caller_a".into(),
                file: "src/a.rs".into(),
                line_start: 42,
            }],
            callees: vec![CalleeEntry {
                name: "callee_b".into(),
                line_start: 3,
            }],
            similar: vec![SimilarEntry {
                name: "baz".into(),
                file: "src/baz.rs".into(),
                score: 0.85,
                content: None,
            }],
            hints: Some(HintsOutput {
                caller_count: 1,
                test_count: 0,
                no_callers: false,
                no_tests: true,
            }),
            token_count: Some(100),
            token_budget: Some(500),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "bar");
        assert_eq!(json["callers"][0]["line_start"], 42);
        assert!(json["callers"][0].get("line").is_none());
        assert_eq!(json["callees"][0]["line_start"], 3);
        assert!(json["callees"][0].get("line").is_none());
        assert_eq!(json["hints"]["no_tests"], true);
        assert_eq!(json["token_count"], 100);
        assert_eq!(json["token_budget"], 500);
        assert!(json.get("content").is_some());
        // similar entry should not have content (None)
        assert!(json["similar"][0].get("content").is_none());
    }

    // TC-14: special characters in name/signature/doc survive serde round-trip
    #[test]
    fn test_explain_output_special_characters() {
        let output = ExplainOutput {
            name: "parse<T>".into(),
            file: "src/parser.rs".into(),
            line_start: 1,
            line_end: 10,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn foo<T: Debug>(x: &T) -> Vec<T>".into(),
            doc: Some("returns \"best\" result with <html> & entities".into()),
            content: Some("fn foo<T>() { let x = \"hello\\nworld\"; }".into()),
            callers: vec![CallerEntry {
                name: "call<U>".into(),
                file: "src/a.rs".into(),
                line_start: 5,
            }],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
        };

        // Serialize to JSON Value
        let json = serde_json::to_value(&output).unwrap();

        // Verify special characters survive
        assert_eq!(json["name"], "parse<T>");
        assert_eq!(json["signature"], "fn foo<T: Debug>(x: &T) -> Vec<T>");
        assert_eq!(
            json["doc"],
            "returns \"best\" result with <html> & entities"
        );
        assert_eq!(json["callers"][0]["name"], "call<U>");

        // Verify round-trip through string serialization
        let json_str = serde_json::to_string(&output).unwrap();
        let roundtrip: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(roundtrip["name"], "parse<T>");
        assert_eq!(
            roundtrip["doc"],
            "returns \"best\" result with <html> & entities"
        );
    }

    // TC-14: unicode in name and doc fields
    #[test]
    fn test_explain_output_unicode() {
        let output = ExplainOutput {
            name: "calculate_\u{03b1}\u{03b2}".into(),
            file: "src/math.rs".into(),
            line_start: 1,
            line_end: 5,
            chunk_type: "function".into(),
            language: "rust".into(),
            signature: "fn calculate_\u{03b1}\u{03b2}()".into(),
            doc: Some("Computes \u{03b1} + \u{03b2} coefficient".into()),
            content: None,
            callers: vec![],
            callees: vec![],
            similar: vec![],
            hints: None,
            token_count: None,
            token_budget: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "calculate_\u{03b1}\u{03b2}");
        assert_eq!(json["doc"], "Computes \u{03b1} + \u{03b2} coefficient");
    }
}
