//! Gather command — smart context assembly for a question
//!
//! Core JSON builders are shared between CLI and batch handlers.

use anyhow::Result;
use colored::Colorize;

use cqs::{
    gather, gather_cross_index_with_index, normalize_path, GatherDirection, GatherOptions,
    GatherResult,
};

use crate::cli::staleness;

// ─── Output types ──────────────────────────────────────────────────────────

/// Typed JSON output for the gather command.
#[derive(Debug, serde::Serialize)]
pub(crate) struct GatherOutput {
    pub query: String,
    pub chunks: Vec<serde_json::Value>,
    pub expansion_capped: bool,
    pub search_degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
}

// ─── Shared JSON builder ───────────────────────────────────────────────────

/// Build typed gather output from a `GatherResult` — shared between CLI and batch.
///
/// Serializes each chunk individually (logging warnings on failure) to match
/// the previous behavior of silently dropping un-serializable chunks.
pub(crate) fn build_gather_output(
    result: &GatherResult,
    query: &str,
    token_info: Option<(usize, usize)>,
) -> GatherOutput {
    let _span = tracing::info_span!("build_gather_output", query_len = query.len()).entered();

    let json_chunks: Vec<serde_json::Value> = result
        .chunks
        .iter()
        .filter_map(|c| match serde_json::to_value(c) {
            Ok(mut v) => {
                // #1167, #1169: derive trust_level + reference_name from the
                // existing `source: Option<String>`. Reference chunks → tagged
                // as "reference-code" with reference_name set; project chunks
                // → "user-code" with reference_name omitted (`source` is
                // already skipped on serialize when None).
                if let Some(obj) = v.as_object_mut() {
                    let (trust_level, ref_name) = match c.source.as_ref() {
                        Some(name) => ("reference-code", Some(name.clone())),
                        None => ("user-code", None),
                    };
                    obj.insert(
                        "trust_level".to_string(),
                        serde_json::Value::String(trust_level.to_string()),
                    );
                    if let Some(name) = ref_name {
                        obj.insert(
                            "reference_name".to_string(),
                            serde_json::Value::String(name),
                        );
                    }
                }
                Some(v)
            }
            Err(e) => {
                tracing::warn!(error = %e, chunk = %c.name, "Failed to serialize chunk");
                None
            }
        })
        .collect();

    GatherOutput {
        query: query.to_string(),
        chunks: json_chunks,
        expansion_capped: result.expansion_capped,
        search_degraded: result.search_degraded,
        token_count: token_info.map(|(used, _)| used),
        token_budget: token_info.map(|(_, budget)| budget),
    }
}

/// Infrastructure context for gather commands.
pub(crate) struct GatherContext<'a> {
    pub ctx: &'a crate::cli::CommandContext<'a, cqs::store::ReadOnly>,
    pub query: &'a str,
    pub expand: usize,
    pub direction: GatherDirection,
    pub limit: usize,
    pub max_tokens: Option<usize>,
    pub ref_name: Option<&'a str>,
    pub json: bool,
}

pub(crate) fn cmd_gather(gctx: &GatherContext<'_>) -> Result<()> {
    let ctx = gctx.ctx;
    let query = gctx.query;
    let expand = gctx.expand;
    let direction = gctx.direction;
    let limit = gctx.limit;
    let max_tokens = gctx.max_tokens;
    let ref_name = gctx.ref_name;
    let json = gctx.json;
    let _span = tracing::info_span!(
        "cmd_gather",
        query_len = query.len(),
        expand,
        limit,
        ?max_tokens,
        ?ref_name
    )
    .entered();

    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;
    let embedder = ctx.embedder()?;

    // When token-budgeted, fetch more chunks than limit so we have candidates to pack
    let fetch_limit = if max_tokens.is_some() {
        limit.max(50) // Fetch at least 50 candidates for token packing
    } else {
        limit
    };

    let opts = GatherOptions {
        expand_depth: expand.clamp(0, 5),
        direction,
        limit: fetch_limit,
        ..GatherOptions::default()
    };

    // Cross-index gather: seed from reference, bridge into project code
    let mut result = if let Some(rn) = ref_name {
        let query_embedding = embedder.embed_query(query)?;
        let ref_idx = crate::cli::commands::resolve::find_reference(root, rn)?;
        let index = crate::cli::build_vector_index(store, cqs_dir)?;
        gather_cross_index_with_index(
            store,
            &ref_idx,
            &query_embedding,
            query,
            &opts,
            root,
            index.as_deref(),
        )?
    } else {
        gather(store, embedder, query, &opts, root)?
    };

    // Token-budgeted packing: keep highest-scoring chunks within token budget
    let token_count_used = if let Some(budget) = max_tokens {
        let overhead = if json {
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT
        } else {
            0
        };
        let chunks = std::mem::take(&mut result.chunks);
        let (mut packed, used) =
            crate::cli::commands::pack_gather_chunks(chunks, embedder, budget, overhead);

        // Re-sort to reading order (ref first, then project, each in file/line order)
        packed.sort_by(|a, b| {
            let source_ord = match (&a.source, &b.source) {
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            };
            source_ord
                .then(a.file.cmp(&b.file))
                .then(a.line_start.cmp(&b.line_start))
                .then(a.name.cmp(&b.name))
        });
        result.chunks = packed;
        Some(used)
    } else {
        None
    };

    // Proactive staleness warning (only for project chunks)
    if !ctx.cli.quiet && !ctx.cli.no_stale_check && !result.chunks.is_empty() {
        let origins: Vec<&str> = result
            .chunks
            .iter()
            .filter(|c| c.source.is_none()) // only project chunks
            .filter_map(|c| c.file.to_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !origins.is_empty() {
            staleness::warn_stale_results(store, &origins, root);
        }
    }

    if json {
        let token_info = token_count_used.map(|used| (used, max_tokens.unwrap_or(0)));
        let output = build_gather_output(&result, query, token_info);
        crate::cli::json_envelope::emit_json(&output)?;
    } else if result.chunks.is_empty() {
        println!("No relevant code found for: {}", query);
    } else {
        let token_info = match (token_count_used, max_tokens) {
            (Some(used), Some(budget)) => format!(" ({} of {} tokens)", used, budget),
            _ => String::new(),
        };
        let ref_label = ref_name
            .map(|rn| format!(" (cross-index via '{}')", rn))
            .unwrap_or_default();
        println!(
            "Gathered {} chunk{}{}{} for: {}",
            result.chunks.len(),
            if result.chunks.len() == 1 { "" } else { "s" },
            ref_label,
            token_info,
            query.cyan(),
        );
        if result.expansion_capped {
            let cap = cqs::gather_max_nodes();
            println!(
                "{}",
                format!("Warning: expansion capped at {cap} nodes").yellow()
            );
        }
        if result.search_degraded {
            println!(
                "{}",
                "Warning: batch name search failed, results may be incomplete".yellow()
            );
        }
        println!();

        let is_cross_index = ref_name.is_some();
        let mut current_file = String::new();
        let mut current_source: Option<String> = None;
        for chunk in &result.chunks {
            // Show source headers only in cross-index mode
            if is_cross_index {
                let source_label = chunk.source.as_deref().unwrap_or("project").to_string();
                if Some(&source_label) != current_source.as_ref() {
                    if current_source.is_some() {
                        println!();
                    }
                    if chunk.source.is_some() {
                        println!("=== Reference: {} ===", source_label.yellow());
                    } else {
                        println!("=== Project ===");
                    }
                    current_source = Some(source_label);
                    current_file.clear();
                }
            }

            let file_str = normalize_path(&chunk.file);
            if file_str != current_file {
                if !current_file.is_empty() {
                    println!();
                }
                println!("--- {} ---", file_str.cyan());
                current_file = file_str;
            }
            let depth_label = if chunk.depth == 0 {
                if is_cross_index {
                    if chunk.source.is_some() {
                        "ref seed".to_string()
                    } else {
                        "bridge".to_string()
                    }
                } else {
                    "seed".to_string()
                }
            } else {
                format!("depth {}", chunk.depth)
            };
            println!(
                "  {} ({}:{}, {}, {:.3})",
                chunk.name.bold(),
                chunk.file.display(),
                chunk.line_start,
                depth_label,
                chunk.score,
            );
            println!("  {}", chunk.signature.dimmed());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(chunks: Vec<cqs::GatheredChunk>) -> GatherResult {
        GatherResult {
            chunks,
            expansion_capped: false,
            search_degraded: false,
        }
    }

    fn make_chunk(name: &str) -> cqs::GatheredChunk {
        cqs::GatheredChunk {
            name: name.to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            line_start: 1,
            line_end: 10,
            language: cqs::parser::Language::Rust,
            chunk_type: cqs::parser::ChunkType::Function,
            signature: format!("fn {}()", name),
            content: "// body".to_string(),
            score: 0.9,
            depth: 0,
            source: None,
        }
    }

    #[test]
    fn gather_output_empty() {
        let result = make_result(vec![]);
        let output = build_gather_output(&result, "test query", None);
        assert_eq!(output.query, "test query");
        assert!(output.chunks.is_empty());
        assert!(!output.expansion_capped);
        assert!(!output.search_degraded);
        assert!(output.token_count.is_none());
        assert!(output.token_budget.is_none());
    }

    #[test]
    fn gather_output_with_chunks() {
        let result = make_result(vec![make_chunk("foo"), make_chunk("bar")]);
        let output = build_gather_output(&result, "find code", None);
        assert_eq!(output.chunks.len(), 2);
        assert_eq!(output.chunks[0]["name"], "foo");
        assert_eq!(output.chunks[1]["name"], "bar");
    }

    #[test]
    fn gather_output_with_token_info() {
        let result = make_result(vec![make_chunk("baz")]);
        let output = build_gather_output(&result, "q", Some((150, 500)));
        assert_eq!(output.token_count, Some(150));
        assert_eq!(output.token_budget, Some(500));
    }

    #[test]
    fn gather_output_flags() {
        let result = GatherResult {
            chunks: vec![],
            expansion_capped: true,
            search_degraded: true,
        };
        let output = build_gather_output(&result, "q", None);
        assert!(output.expansion_capped);
        assert!(output.search_degraded);
    }

    #[test]
    fn gather_output_serializes() {
        let result = make_result(vec![make_chunk("x")]);
        let output = build_gather_output(&result, "q", Some((100, 300)));
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["query"], "q");
        assert_eq!(json["token_count"], 100);
        assert_eq!(json["token_budget"], 300);
        assert!(json["chunks"].is_array());
    }

    #[test]
    fn gather_output_omits_tokens_when_none() {
        let result = make_result(vec![]);
        let output = build_gather_output(&result, "q", None);
        let json = serde_json::to_value(&output).unwrap();
        assert!(json.get("token_count").is_none());
        assert!(json.get("token_budget").is_none());
    }
}
