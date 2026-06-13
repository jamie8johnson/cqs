//! Gather command — smart context assembly for a question
//!
//! Core JSON builders are shared between CLI and batch handlers.

use anyhow::Result;
use colored::Colorize;

use cqs::index::VectorIndex;
use cqs::reference::ReferenceIndex;
use cqs::store::{ReadOnly, Store};
use cqs::Embedder;
use cqs::{
    gather_cross_index_with_index, gather_with_overlay, normalize_path, GatherDirection,
    GatherOptions, GatherResult,
};

use crate::cli::staleness;

// ─── Args (surface-agnostic, MCP-ready) ─────────────────────────────────────

/// Input for [`gather_core`] — the gather knobs both the CLI and a future MCP
/// `gather` tool deserialize into. Store/embedder/root and the resolved
/// reference index come from the adapter (reference resolution differs by
/// surface); these are the request-shaped settings.
///
/// `#[serde(default)]` so a wire caller can supply just `query` and inherit the
/// production defaults (depth/direction/limit mirror clap).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct GatherArgs {
    /// Search query / question.
    pub query: String,
    /// Call-graph BFS depth for expansion (clamped 0..=5).
    pub depth: usize,
    /// Expansion direction: both / callers / callees.
    pub direction: GatherDirection,
    /// Max seed chunks before expansion (clamped 1..=100).
    pub limit: usize,
    /// Token budget — when set, packs chunks into the budget.
    pub tokens: Option<usize>,
    /// Per-result JSON overhead the token packer charges (the CLI sets this to
    /// the per-result envelope cost under `--json`, 0 for text; a wire caller
    /// that always serializes should set the constant). `#[serde(default)]` 0.
    pub json_overhead: usize,
}

impl Default for GatherArgs {
    fn default() -> Self {
        GatherArgs {
            query: String::new(),
            // Mirrors clap: DEFAULT_DEPTH_BLAST = 1, direction = both,
            // LimitArg default = 5.
            depth: crate::cli::args::DEFAULT_DEPTH_BLAST,
            direction: GatherDirection::Both,
            limit: 5,
            tokens: None,
            json_overhead: 0,
        }
    }
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs gather`. Runs the gather lib primitive
/// (project, or cross-index when `ref_idx` is `Some`), then applies the shared
/// token-budget packing + reading-order re-sort. Returns the assembled
/// [`GatherResult`] plus `(used, budget)` token accounting; the adapter renders
/// text/JSON and owns staleness warnings. Reads no env — the
/// `json_overhead` and reference resolution arrive via `args` / parameters.
///
/// Unifying the packing here gives the daemon the same reading-order re-sort
/// the CLI always had (previously the daemon left packed chunks in score order).
#[allow(clippy::too_many_arguments)]
pub(crate) fn gather_core(
    store: &Store<ReadOnly>,
    embedder: &Embedder,
    root: &std::path::Path,
    args: &GatherArgs,
    ref_idx: Option<&ReferenceIndex>,
    project_index: Option<&dyn VectorIndex>,
    overlay: Option<&cqs::worktree_overlay::WorktreeOverlay>,
) -> Result<(GatherResult, Option<(usize, usize)>)> {
    let _span = tracing::info_span!("gather_core", query_len = args.query.len()).entered();

    // When token-budgeted, fetch more seeds than the limit so the packer has
    // candidates to choose from.
    let fetch_limit = if args.tokens.is_some() {
        args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP).max(50)
    } else {
        args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP)
    };

    let opts = GatherOptions {
        expand_depth: args.depth.clamp(0, 5),
        direction: args.direction,
        limit: fetch_limit,
        ..GatherOptions::default()
    };

    let mut result = if let Some(ref_idx) = ref_idx {
        // Cross-index gather seeds from the REFERENCE, not the project store, so
        // the worktree overlay (a project-delta shadow) does not apply to the
        // seed — it stays parent-truth in Part A.
        let query_embedding = embedder.embed_query(&args.query)?;
        gather_cross_index_with_index(
            store,
            ref_idx,
            &query_embedding,
            &args.query,
            &opts,
            root,
            project_index,
        )?
    } else {
        // Project gather: overlay the seed search (Part A).
        gather_with_overlay(store, embedder, &args.query, &opts, root, overlay)?
    };

    let token_info = if let Some(budget) = args.tokens {
        let chunks = std::mem::take(&mut result.chunks);
        let (mut packed, used) =
            crate::cli::commands::pack_gather_chunks(chunks, embedder, budget, args.json_overhead);

        // Re-sort to reading order (ref first, then project, each in
        // file/line order) so chained reads are coherent.
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
        Some((used, budget))
    } else {
        None
    };

    Ok((result, token_info))
}

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
                // Derive trust_level + reference_name from the
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

    let json_overhead = if json {
        crate::cli::commands::JSON_OVERHEAD_PER_RESULT
    } else {
        0
    };
    let args = GatherArgs {
        query: query.to_string(),
        depth: expand,
        direction,
        limit,
        tokens: max_tokens,
        json_overhead,
    };

    // Resolve the reference index + project vector index for cross-index
    // gather (CLI-side resolution; the daemon resolves its own), then drive
    // the shared core.
    let (result, token_info) = if let Some(rn) = ref_name {
        let ref_idx = crate::cli::commands::resolve::find_reference(root, rn)?;
        let index = crate::cli::build_vector_index(store, cqs_dir)?;
        gather_core(
            store,
            embedder,
            root,
            &args,
            Some(&ref_idx),
            index.as_deref(),
            // CLI surface serves the parent index (overlay is daemon-only, ).
            None,
        )?
    } else {
        gather_core(store, embedder, root, &args, None, None, None)?
    };
    let token_count_used = token_info.map(|(used, _)| used);

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

    /// A wire/MCP caller can supply only `query` and inherit defaults.
    #[test]
    fn gather_args_deserialize_minimal() {
        let args: GatherArgs = serde_json::from_str(r#"{"query": "search path"}"#).unwrap();
        assert_eq!(args.query, "search path");
        assert_eq!(args.depth, 1);
        assert_eq!(args.direction, GatherDirection::Both);
        assert_eq!(args.limit, 5);
        assert!(args.tokens.is_none());
        assert_eq!(args.json_overhead, 0);
    }

    /// `GatherArgs::default` must match the clap `GatherArgs` defaults.
    /// Parses `cqs gather <query>` via a throwaway `clap::Parser` wrapper.
    #[test]
    fn gather_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::GatherArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-gather", "q"]).unwrap().args;
        let core = GatherArgs {
            query: clap_args.query.clone(),
            depth: clap_args.depth,
            direction: clap_args.direction,
            limit: clap_args.limit_arg.limit,
            tokens: clap_args.tokens,
            // json_overhead is an adapter-resolved field, not a clap flag.
            json_overhead: 0,
        };
        let expected = GatherArgs {
            query: "q".to_string(),
            ..GatherArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap gather defaults drifted from GatherArgs::default — update both together"
        );
    }

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
            rank_signals: vec![],
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
