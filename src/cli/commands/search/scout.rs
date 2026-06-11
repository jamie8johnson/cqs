//! Scout command — pre-investigation dashboard for task planning
//!
//! The JSON builder lives here and is called by both the CLI (`cmd_scout`)
//! and the batch handler (`dispatch_scout` in
//! `src/cli/batch/handlers/misc.rs`) so the shape stays identical across
//! the two dispatch paths.

use anyhow::Result;
use colored::Colorize;

use cqs::store::{ReadOnly, Store};
use cqs::{scout_with_options, Embedder, ScoutOptions};

// ─── Args (surface-agnostic, MCP-ready) ─────────────────────────────────────

/// Input for [`scout_core`] — the scout knobs both the CLI and a future MCP
/// `scout` tool deserialize into. The core takes the store/embedder/root from
/// the adapter; these are the request-shaped settings.
///
/// `#[serde(default)]` so a wire caller can supply just `query` and inherit the
/// production defaults (limit mirrors clap's `LimitArg`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct ScoutArgs {
    /// Search query to investigate.
    pub query: String,
    /// Max file groups to return (clamped to `SCOUT_LIMIT_MAX`).
    pub limit: usize,
    /// Token budget — when set, packs chunk content into the budget.
    pub tokens: Option<usize>,
    /// Override the number of search results scout retrieves before grouping.
    /// `None` inherits [`ScoutOptions::default`] (`DEFAULT_SCOUT_SEARCH_LIMIT`).
    pub search_limit: Option<usize>,
    /// Override the minimum search score threshold. `None` inherits
    /// [`ScoutOptions::default`] (`DEFAULT_SCOUT_SEARCH_THRESHOLD`).
    pub search_threshold: Option<f32>,
    /// Override the min relative score gap that splits a ModifyTarget from a
    /// Dependency. Lower → more ModifyTargets. `None` inherits the default.
    pub min_gap_ratio: Option<f32>,
}

impl Default for ScoutArgs {
    fn default() -> Self {
        ScoutArgs {
            query: String::new(),
            // Mirrors clap `LimitArg` default (5).
            limit: 5,
            tokens: None,
            search_limit: None,
            search_threshold: None,
            min_gap_ratio: None,
        }
    }
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs scout` (JSON path). Runs the `scout` lib
/// primitive, optionally token-packs chunk content, and assembles the shared
/// JSON via [`build_scout_output`]. Returns the assembled `(value, token_info)`
/// so the CLI adapter can also drive its text rendering; the daemon takes only
/// the value. Reads no env (the limit clamp uses the `SCOUT_LIMIT_MAX` const).
pub(crate) fn scout_core(
    store: &Store<ReadOnly>,
    embedder: &Embedder,
    root: &std::path::Path,
    args: &ScoutArgs,
) -> Result<(serde_json::Value, Option<(usize, usize)>)> {
    let _span = tracing::info_span!("scout_core", query = %args.query).entered();
    let limit = args.limit.clamp(1, crate::cli::SCOUT_LIMIT_MAX);
    let opts = scout_options_from_args(args);
    let result = scout_with_options(store, embedder, &args.query, root, limit, &opts)?;

    let (content_map, token_info) = if let Some(budget) = args.tokens {
        let named_items = crate::cli::commands::scout_scored_names(&result);
        let (cmap, used) =
            crate::cli::commands::fetch_and_pack_content(store, embedder, &named_items, budget);
        (Some(cmap), Some((used, budget)))
    } else {
        (None, None)
    };

    let output = build_scout_output(&result, content_map.as_ref(), token_info)?;
    Ok((output, token_info))
}

/// Fold the optional `ScoutArgs` knob overrides onto [`ScoutOptions::default`].
/// Each `None` inherits the production default; each `Some` overrides it.
fn scout_options_from_args(args: &ScoutArgs) -> ScoutOptions {
    let mut opts = ScoutOptions::default();
    if let Some(v) = args.search_limit {
        opts.search_limit = v;
    }
    if let Some(v) = args.search_threshold {
        opts.search_threshold = v;
    }
    if let Some(v) = args.min_gap_ratio {
        opts.min_gap_ratio = v;
    }
    opts
}

/// Build the typed scout JSON object shared between CLI and batch.
///
/// Serializes `ScoutResult` once, then injects optional content-map and
/// token-budget fields in a fixed order so both dispatch paths produce
/// byte-identical JSON for the same inputs.
///
/// # Parameters
/// - `result`: scout analysis output
/// - `content_map`: `Some` when `--tokens` was supplied; injects a `content`
///   field into each matching chunk entry
/// - `token_info`: `Some((used, budget))` when token packing ran; adds the
///   `token_count` and `token_budget` fields to the top-level object
pub(crate) fn build_scout_output(
    result: &cqs::ScoutResult,
    content_map: Option<&std::collections::HashMap<String, String>>,
    token_info: Option<(usize, usize)>,
) -> Result<serde_json::Value> {
    let _span = tracing::debug_span!("build_scout_output").entered();
    let mut output = serde_json::to_value(result)?;
    if let Some(cmap) = content_map {
        crate::cli::commands::inject_content_into_scout_json(&mut output, cmap);
    }
    // Scout only queries the project store — every chunk is user-code.
    crate::cli::commands::tag_user_code_trust_level(&mut output);
    crate::cli::commands::inject_token_info(&mut output, token_info);
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_scout(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    task: &str,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
    search_limit: Option<usize>,
    search_threshold: Option<f32>,
    min_gap_ratio: Option<f32>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_scout", task, ?max_tokens).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder()?;

    // JSON path routes through the shared `scout_core` (same code the daemon
    // runs), so the wire shape is identical across surfaces.
    if json {
        let args = ScoutArgs {
            query: task.to_string(),
            limit,
            tokens: max_tokens,
            search_limit,
            search_threshold,
            min_gap_ratio,
        };
        let (output, _token_info) = scout_core(store, embedder, root, &args)?;
        crate::cli::json_envelope::emit_json(&output)?;
        return Ok(());
    }

    // Text path keeps the raw `ScoutResult` for rendering. Clamp via the shared
    // constant so it returns the same number of groups as the core.
    let limit = limit.clamp(1, crate::cli::SCOUT_LIMIT_MAX);

    let opts = scout_options_from_args(&ScoutArgs {
        search_limit,
        search_threshold,
        min_gap_ratio,
        ..Default::default()
    });
    let result = scout_with_options(store, embedder, task, root, limit, &opts)?;

    // Token-budgeted content: fetch chunk content and pack into budget
    let (content_map, token_info) = if let Some(budget) = max_tokens {
        let named_items = crate::cli::commands::scout_scored_names(&result);
        let (cmap, used) =
            crate::cli::commands::fetch_and_pack_content(store, embedder, &named_items, budget);
        (Some(cmap), Some((used, budget)))
    } else {
        (None, None)
    };

    {
        let token_label = match token_info {
            Some((used, budget)) => format!(" ({} of {} tokens)", used, budget),
            None => String::new(),
        };
        println!("{} {}{}", "Scout:".cyan(), task.bold(), token_label);

        if result.file_groups.is_empty() {
            println!();
            println!("{}", "No relevant code found.".dimmed());
        } else {
            for group in &result.file_groups {
                let rel = cqs::rel_display(&group.file, root);

                println!();
                print!(
                    "{} {}",
                    rel.bold(),
                    format!("({:.2})", group.relevance_score).dimmed()
                );
                if group.is_stale {
                    print!(" {}", "[STALE]".yellow().bold());
                }
                println!();

                for chunk in &group.chunks {
                    let role_indicator = match chunk.role {
                        cqs::ChunkRole::ModifyTarget => "",
                        cqs::ChunkRole::TestToUpdate => " [test]",
                        cqs::ChunkRole::Dependency => " [dep]",
                    };

                    let test_marker =
                        if chunk.test_count == 0 && chunk.role != cqs::ChunkRole::TestToUpdate {
                            " !!".red().bold().to_string()
                        } else {
                            String::new()
                        };

                    println!(
                        "  {}{}  {}",
                        chunk.signature.dimmed(),
                        role_indicator.dimmed(),
                        format!(
                            "[{} caller{}, {} test{}]{}",
                            chunk.caller_count,
                            if chunk.caller_count == 1 { "" } else { "s" },
                            chunk.test_count,
                            if chunk.test_count == 1 { "" } else { "s" },
                            test_marker
                        )
                        .dimmed()
                    );

                    // Print content if within token budget
                    if let Some(ref cmap) = content_map {
                        if let Some(content) = cmap.get(&chunk.name) {
                            println!("{}", "\u{2500}".repeat(50));
                            println!("{}", crate::cli::display::sanitize_for_terminal(content));
                            println!();
                        }
                    }
                }
            }

            // Notes
            if !result.relevant_notes.is_empty() {
                println!();
                println!("{}", "Notes:".cyan());
                for note in &result.relevant_notes {
                    let sentiment = if note.sentiment < 0.0 {
                        format!("[{:.1}]", note.sentiment).red().to_string()
                    } else if note.sentiment > 0.0 {
                        format!("[+{:.1}]", note.sentiment).green().to_string()
                    } else {
                        "[0.0]".dimmed().to_string()
                    };
                    // Truncate long notes
                    let text = if note.text.len() > 80 {
                        format!("{}...", &note.text[..note.text.floor_char_boundary(77)])
                    } else {
                        note.text.clone()
                    };
                    println!("  {} {}", sentiment, text.dimmed());
                }
            }

            // Summary
            println!();
            println!(
                "{} {} file{}, {} function{}, {} untested, {} stale",
                "Summary:".cyan(),
                result.summary.total_files,
                if result.summary.total_files == 1 {
                    ""
                } else {
                    "s"
                },
                result.summary.total_functions,
                if result.summary.total_functions == 1 {
                    ""
                } else {
                    "s"
                },
                result.summary.untested_count,
                result.summary.stale_count
            );
        }
    }

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// A wire/MCP caller can supply only `query` and inherit defaults.
    #[test]
    fn scout_args_deserialize_minimal() {
        let args: super::ScoutArgs = serde_json::from_str(r#"{"query": "auth flow"}"#).unwrap();
        assert_eq!(args.query, "auth flow");
        assert_eq!(args.limit, 5);
        assert!(args.tokens.is_none());
        assert!(args.search_limit.is_none());
        assert!(args.search_threshold.is_none());
        assert!(args.min_gap_ratio.is_none());
    }

    /// A wire caller can override the search knobs; omitted knobs inherit the
    /// `ScoutOptions::default` values via `scout_options_from_args`.
    #[test]
    fn scout_args_knobs_reach_scout_options() {
        let args: super::ScoutArgs = serde_json::from_str(
            r#"{"query": "x", "search_limit": 30, "search_threshold": 0.05, "min_gap_ratio": 0.25}"#,
        )
        .unwrap();
        assert_eq!(args.search_limit, Some(30));
        assert_eq!(args.search_threshold, Some(0.05));
        assert_eq!(args.min_gap_ratio, Some(0.25));

        let opts = super::scout_options_from_args(&args);
        assert_eq!(opts.search_limit, 30);
        assert_eq!(opts.search_threshold, 0.05);
        assert_eq!(opts.min_gap_ratio, 0.25);
    }

    /// Omitted knobs fall back to `ScoutOptions::default`.
    #[test]
    fn scout_args_omitted_knobs_use_defaults() {
        let args: super::ScoutArgs = serde_json::from_str(r#"{"query": "x"}"#).unwrap();
        let opts = super::scout_options_from_args(&args);
        let defaults = cqs::ScoutOptions::default();
        assert_eq!(opts.search_limit, defaults.search_limit);
        assert_eq!(opts.search_threshold, defaults.search_threshold);
        assert_eq!(opts.min_gap_ratio, defaults.min_gap_ratio);
    }

    /// `ScoutArgs::default` must match the clap `ScoutArgs` defaults exactly.
    /// Parses `cqs scout <query>` via a throwaway `clap::Parser` wrapper.
    #[test]
    fn scout_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::ScoutArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-scout", "q"]).unwrap().args;
        let core = super::ScoutArgs {
            query: clap_args.query.clone(),
            limit: clap_args.limit_arg.limit,
            tokens: clap_args.tokens,
            search_limit: clap_args.search_limit,
            search_threshold: clap_args.search_threshold,
            min_gap_ratio: clap_args.min_gap_ratio,
        };
        let expected = super::ScoutArgs {
            query: "q".to_string(),
            ..super::ScoutArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap scout defaults drifted from ScoutArgs::default — update both together"
        );
    }

    // ===== inject_content_into_scout_json tests =====

    #[test]
    fn tc15_inject_content_into_scout_json_known_shape() {
        // Verify inject_content_into_scout_json mutates content by name.
        let mut scout_json = json!({
            "file_groups": [
                {
                    "file": "src/lib.rs",
                    "chunks": [
                        { "name": "foo", "signature": "fn foo()" },
                        { "name": "bar", "signature": "fn bar()" }
                    ]
                }
            ]
        });
        let mut content_map = std::collections::HashMap::new();
        content_map.insert("foo".to_string(), "fn foo() { 42 }".to_string());

        crate::cli::commands::inject_content_into_scout_json(&mut scout_json, &content_map);

        let chunks = scout_json["file_groups"][0]["chunks"].as_array().unwrap();
        assert_eq!(
            chunks[0]["content"], "fn foo() { 42 }",
            "foo's content should be injected"
        );
        assert!(
            chunks[1].get("content").is_none(),
            "bar should have no content (not in content_map)"
        );
    }

    #[test]
    fn tc15_inject_content_empty_map_is_noop() {
        // Empty content_map should leave JSON unchanged.
        let original = json!({
            "file_groups": [
                {
                    "file": "src/lib.rs",
                    "chunks": [
                        { "name": "baz", "signature": "fn baz()" }
                    ]
                }
            ]
        });
        let mut json_val = original.clone();
        let empty_map = std::collections::HashMap::new();

        crate::cli::commands::inject_content_into_scout_json(&mut json_val, &empty_map);

        assert_eq!(
            json_val, original,
            "Empty content_map should leave JSON unchanged"
        );
    }

    #[test]
    fn tc15_inject_content_unrecognized_names_is_noop() {
        // content_map with names not in the JSON should not add fields.
        let original = json!({
            "file_groups": [
                {
                    "file": "src/lib.rs",
                    "chunks": [
                        { "name": "existing_fn", "signature": "fn existing_fn()" }
                    ]
                }
            ]
        });
        let mut json_val = original.clone();
        let mut content_map = std::collections::HashMap::new();
        content_map.insert("nonexistent_fn".to_string(), "content".to_string());

        crate::cli::commands::inject_content_into_scout_json(&mut json_val, &content_map);

        assert_eq!(
            json_val, original,
            "Unrecognized names should leave JSON unchanged"
        );
    }

    #[test]
    fn tc15_inject_content_no_file_groups_is_noop() {
        // JSON without file_groups key should be a safe no-op.
        let mut json_val = json!({ "summary": { "total_files": 0 } });
        let mut content_map = std::collections::HashMap::new();
        content_map.insert("foo".to_string(), "content".to_string());

        // Should not panic
        crate::cli::commands::inject_content_into_scout_json(&mut json_val, &content_map);
        assert!(json_val.get("file_groups").is_none());
    }

    // ===== inject_token_info tests =====

    #[test]
    fn tc15_inject_token_info_adds_fields() {
        let mut json_val = json!({ "results": [] });

        crate::cli::commands::inject_token_info(&mut json_val, Some((100, 500)));

        assert_eq!(json_val["token_count"], 100);
        assert_eq!(json_val["token_budget"], 500);
    }

    #[test]
    fn tc15_inject_token_info_none_is_noop() {
        let original = json!({ "results": [] });
        let mut json_val = original.clone();

        crate::cli::commands::inject_token_info(&mut json_val, None);

        assert_eq!(json_val, original, "None token_info should be a no-op");
        assert!(
            json_val.get("token_count").is_none(),
            "token_count should not be added when token_info is None"
        );
    }

    #[test]
    fn tc15_inject_token_info_zero_values() {
        let mut json_val = json!({});

        crate::cli::commands::inject_token_info(&mut json_val, Some((0, 0)));

        assert_eq!(json_val["token_count"], 0);
        assert_eq!(json_val["token_budget"], 0);
    }
}
