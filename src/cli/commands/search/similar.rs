//! Similar command — find code similar to a given function
//!
//! Core retrieval logic lives in [`similar_core`] so the CLI (`cmd_similar`)
//! and the batch/daemon handler (`dispatch_similar` in
//! `src/cli/batch/handlers/info.rs`) share one surface-agnostic path. Both
//! adapters resolve the target, fetch its embedding, run the index-guided
//! search, drop the self-match, and serialize the canonical `SearchResult`
//! JSON shape — so CLI and daemon payloads stay byte-identical.

use anyhow::{Context, Result};

use cqs::store::{ReadOnly, SearchResult, Store};
use cqs::{SearchFilter, VectorIndex};

use crate::cli::display;

// ─── Args (surface-agnostic, MCP-ready) ─────────────────────────────────────

/// Input for [`similar_core`] — the knobs both the CLI and a future MCP
/// `similar` tool deserialize into. The store, vector index, and search
/// filter come from the adapter; these are the request-shaped settings.
///
/// `#[serde(default)]` so a wire caller can supply just `name` and inherit the
/// production defaults (limit mirrors clap's `LimitArg`, threshold mirrors the
/// `--threshold` default).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct SimilarArgs {
    /// Function name or `file:function`.
    pub name: String,
    /// Cap on results returned (clamped to `SIMILAR_LIMIT_MAX` in the core).
    pub limit: usize,
    /// Min similarity threshold.
    pub threshold: f32,
}

impl Default for SimilarArgs {
    fn default() -> Self {
        Self {
            name: String::new(),
            // Mirrors clap `LimitArg` default (5).
            limit: crate::cli::args::DEFAULT_LIMIT,
            // Mirrors clap `--threshold` default in `args::SimilarArgs`.
            threshold: 0.3,
        }
    }
}

// ─── Output ─────────────────────────────────────────────────────────────────

/// Typed JSON output for the similar command: `{target, results, total}`.
///
/// `results` carries the canonical per-result `SearchResult::to_json()` shape
/// (the 9-field code-result schema with `type` + `has_parent`) so this surface
/// can never drift from the rest of the CLI on the chunk schema.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SimilarOutput {
    /// Resolved name of the queried chunk.
    pub target: String,
    /// Matched chunks, nearest first, self-match excluded.
    pub results: Vec<serde_json::Value>,
    /// Number of results returned.
    pub total: usize,
}

/// Resolved similar-search result: the target name plus the filtered match
/// list, self-match already excluded. The [`similar_core`] return type.
///
/// Carries the raw [`SearchResult`]s rather than the projected JSON so the
/// text adapter keeps access to the full chunks for `UnifiedResult` rendering;
/// the JSON adapter wraps with [`build_similar_output`]. Both surfaces call
/// [`similar_core`] so the retrieve + self-exclude + truncate discipline can't
/// drift.
pub(crate) struct SimilarMatches {
    /// Resolved name of the queried chunk.
    pub target: String,
    /// Matched chunks, nearest first, self-match excluded, truncated to limit.
    pub results: Vec<SearchResult>,
}

// ─── Filter construction (shared by both surfaces) ──────────────────────────

/// Build the `similar` search filter from the `--lang` / `--path` scope flags.
///
/// Both surfaces call this so a daemon-routed `similar` builds the exact same
/// [`SearchFilter`] as the CLI-direct path. Parses `lang` to a concrete
/// [`cqs::parser::Language`] (surfacing the valid-name list on a typo) and
/// carries `path` through as the glob `path_pattern`. Notes stay excluded —
/// `similar` is a code-only neighborhood search.
pub(crate) fn build_similar_filter(lang: Option<&str>, path: Option<&str>) -> Result<SearchFilter> {
    let languages = match lang {
        Some(l) => Some(vec![l.parse().context(format!(
            "Invalid language. Valid: {}",
            cqs::parser::Language::valid_names_display()
        ))?]),
        None => None,
    };
    // SearchFilter is `#[non_exhaustive]`; external-crate construction goes
    // through `Default` + field assignment.
    let mut f = SearchFilter::default();
    f.languages = languages;
    f.path_pattern = path.map(|p| p.to_string());
    Ok(f)
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs similar <name>`.
///
/// Resolves the target via the shared [`cqs::resolve_target`] helper, loads its
/// embedding, runs the index-guided search (requesting one extra to exclude the
/// self-match), drops the source chunk, and truncates to the clamped limit. The
/// vector index and filter are passed in rather than built internally so each
/// adapter supplies its own (the CLI's concrete HNSW handle, the daemon's
/// cached `dyn VectorIndex`) without the core knowing which surface it runs on.
/// Both surfaces now build their `--lang`/`--path`-scoped filter through the
/// shared [`build_similar_filter`], so daemon-routed scoping matches CLI-direct.
pub(crate) fn similar_core(
    store: &Store<ReadOnly>,
    index: Option<&dyn VectorIndex>,
    filter: &SearchFilter,
    args: &SimilarArgs,
) -> Result<SimilarMatches> {
    let _span =
        tracing::info_span!("similar_core", name = %args.name, limit = args.limit).entered();
    crate::cli::validate_finite_f32(args.threshold, "threshold")?;

    // Clamp via shared constant so CLI and batch return the same number of
    // results. Without it, `-n 10000` cascades into unbounded allocation in
    // search_filtered_with_index.
    let limit = args.limit.clamp(1, crate::cli::SIMILAR_LIMIT_MAX);

    let resolved = cqs::resolve_target(store, &args.name)?;
    let chunk_id = resolved.chunk.id.clone();
    let chunk_name = resolved.chunk.name.clone();

    // Fetch embedding for the target chunk.
    let (source_chunk, embedding) =
        store
            .get_chunk_with_embedding(&chunk_id)?
            .with_context(|| {
                format!(
                    "Could not load embedding for '{}'. Index may be corrupt.",
                    chunk_name
                )
            })?;

    // Request one extra to exclude the self-match below.
    let results = store.search_filtered_with_index(
        &embedding,
        filter,
        limit.saturating_add(1),
        args.threshold,
        index,
    )?;

    let filtered: Vec<SearchResult> = results
        .into_iter()
        .filter(|r| r.chunk.id != source_chunk.id)
        .take(limit)
        .collect();

    Ok(SimilarMatches {
        target: chunk_name,
        results: filtered,
    })
}

/// Project [`SimilarMatches`] into the typed JSON output.
///
/// Emits the canonical per-result `SearchResult::to_json()` shape so daemon/CLI
/// parity holds — the same schema both surfaces previously built inline.
pub(crate) fn build_similar_output(matches: &SimilarMatches) -> SimilarOutput {
    let _span = tracing::info_span!(
        "build_similar_output",
        target = %matches.target,
        count = matches.results.len()
    )
    .entered();
    let json_results: Vec<serde_json::Value> =
        matches.results.iter().map(|r| r.to_json()).collect();
    let total = json_results.len();
    SimilarOutput {
        target: matches.target.clone(),
        results: json_results,
        total,
    }
}

// ─── CLI command (thin adapter over the core) ──────────────────────────────

pub(crate) fn cmd_similar(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    limit: usize,
    threshold: f32,
    lang: Option<&str>,
    path: Option<&str>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_similar", name).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    // Build the scoped search filter via the shared helper so the CLI-direct
    // and daemon-routed paths construct byte-identical filters from the same
    // `--lang` / `--path` inputs.
    let filter = build_similar_filter(lang, path)?;

    // Load the concrete HNSW index; the core takes a `dyn VectorIndex`.
    let index = cqs::HnswIndex::try_load_with_ef(cqs_dir, None, store.dim());

    let args = SimilarArgs {
        name: name.to_string(),
        limit,
        threshold,
    };
    let matches = similar_core(store, index.as_deref(), &filter, &args)?;

    if json {
        let output = build_similar_output(&matches);
        crate::cli::json_envelope::emit_json(&output)?;
        return Ok(());
    }

    if matches.results.is_empty() {
        println!("No similar functions found for '{}'.", matches.target);
        return Ok(());
    }

    if !ctx.cli.quiet {
        // The first result's source file isn't the target's — re-resolve for
        // the header (cheap indexed lookup). Text mode only; the JSON path
        // returns above without it.
        let resolved = cqs::resolve_target(store, &matches.target)?;
        println!(
            "Similar to '{}' ({}):",
            matches.target,
            resolved.chunk.file.display()
        );
        println!();
    }

    let unified: Vec<cqs::store::UnifiedResult> = matches
        .results
        .into_iter()
        .map(cqs::store::UnifiedResult::Code)
        .collect();
    display::display_unified_results(&unified, root, ctx.cli.no_content, ctx.cli.context, None)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `parse_target` is re-exported from the library via `cqs::parse_target`.
    // These coverage tests live in this file — no direct `parse_target` tests
    // exist in `src/search/mod.rs` (only `resolve_target` is covered there).
    use cqs::parse_target;

    #[test]
    fn test_parse_target_name_only() {
        let (file, name) = parse_target("search_filtered");
        assert_eq!(file, None);
        assert_eq!(name, "search_filtered");
    }

    #[test]
    fn test_parse_target_file_and_name() {
        let (file, name) = parse_target("src/search.rs:search_filtered");
        assert_eq!(file, Some("src/search.rs"));
        assert_eq!(name, "search_filtered");
    }

    #[test]
    fn test_parse_target_nested_path() {
        let (file, name) = parse_target("src/cli/commands/query.rs:cmd_query");
        assert_eq!(file, Some("src/cli/commands/query.rs"));
        assert_eq!(name, "cmd_query");
    }

    #[test]
    fn test_parse_target_empty_name_fallback() {
        // Trailing colon — stripped.
        let (file, name) = parse_target("something:");
        assert_eq!(file, None);
        assert_eq!(name, "something");
    }

    #[test]
    fn test_parse_target_leading_colon_fallback() {
        // Leading colon — treat entire string as name
        let (file, name) = parse_target(":name");
        assert_eq!(file, None);
        assert_eq!(name, ":name");
    }

    /// A wire caller can supply just `name` and inherit the defaults.
    #[test]
    fn similar_args_deserialize_minimal() {
        let args: SimilarArgs = serde_json::from_str(r#"{"name":"foo"}"#).unwrap();
        assert_eq!(args.name, "foo");
        assert_eq!(args.limit, crate::cli::args::DEFAULT_LIMIT);
        assert!((args.threshold - 0.3).abs() < 1e-6);
    }

    /// The output envelope pins the `{target, results, total}` shape.
    #[test]
    fn similar_output_serializes() {
        let output = SimilarOutput {
            target: "my_func".to_string(),
            results: vec![serde_json::json!({"name": "near_fn", "score": 0.9})],
            total: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["target"], "my_func");
        assert_eq!(json["total"], 1);
        assert_eq!(json["results"][0]["name"], "near_fn");
    }

    fn make_search_result(name: &str, score: f32, parent_id: Option<&str>) -> SearchResult {
        SearchResult::new(
            cqs::store::ChunkSummary {
                id: format!("id-{name}"),
                file: std::path::PathBuf::from(format!("src/{name}.rs")),
                language: cqs::parser::Language::Rust,
                chunk_type: cqs::parser::ChunkType::Function,
                name: name.to_string(),
                signature: format!("fn {name}()"),
                content: format!("fn {name}() {{}}"),
                doc: None,
                line_start: 10,
                line_end: 20,
                parent_id: parent_id.map(|s| s.to_string()),
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            score,
        )
    }

    /// `build_similar_output` projects the canonical per-result
    /// `SearchResult::to_json()` schema — the 9 base fields plus the envelope.
    #[test]
    fn build_similar_output_carries_canonical_schema() {
        let matches = SimilarMatches {
            target: "my_target".to_string(),
            results: vec![
                make_search_result("alpha", 0.95, None),
                make_search_result("beta", 0.80, Some("parent-1")),
            ],
        };
        let output = serde_json::to_value(build_similar_output(&matches)).unwrap();

        assert_eq!(output["target"], "my_target");
        assert_eq!(output["total"], 2);

        let arr = output["results"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        for (i, item) in arr.iter().enumerate() {
            let obj = item.as_object().unwrap();
            for field in [
                "file",
                "line_start",
                "line_end",
                "name",
                "signature",
                "language",
                "chunk_type",
                "score",
                "content",
            ] {
                assert!(
                    obj.contains_key(field),
                    "result[{i}] missing field '{field}'"
                );
            }
        }
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[1]["name"], "beta");
        assert_eq!(arr[0]["line_start"], 10);
        assert_eq!(arr[0]["line_end"], 20);
        assert_eq!(arr[0]["language"], "rust");
        assert_eq!(arr[0]["chunk_type"], "function");
        let s0 = arr[0]["score"].as_f64().unwrap();
        assert!((s0 - 0.95).abs() < 1e-4);
    }
}
