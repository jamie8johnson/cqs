//! Scout command — pre-investigation dashboard for task planning
//!
//! CQ-V1.25-7: the JSON builder lives here and is called by both the CLI
//! (`cmd_scout`) and the batch handler (`dispatch_scout` in
//! `src/cli/batch/handlers/misc.rs`) so the shape stays identical across
//! the two dispatch paths. Mirrors the `GcOutput` pattern in
//! `src/cli/commands/index/gc.rs`.

use anyhow::Result;
use colored::Colorize;

use cqs::scout;

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
    // #1167: scout only queries the project store — every chunk is user-code.
    crate::cli::commands::tag_user_code_trust_level(&mut output);
    crate::cli::commands::inject_token_info(&mut output, token_info);
    Ok(output)
}

pub(crate) fn cmd_scout(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    task: &str,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_scout", task, ?max_tokens).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder()?;
    // CQ-V1.25-2: clamp via shared constant so CLI and batch return the
    // same number of groups. Previously capped at 10 here vs 50 in batch.
    let limit = limit.clamp(1, crate::cli::SCOUT_LIMIT_MAX);

    let result = scout(store, embedder, task, root, limit)?;

    // Token-budgeted content: fetch chunk content and pack into budget
    let (content_map, token_info) = if let Some(budget) = max_tokens {
        let named_items = crate::cli::commands::scout_scored_names(&result);
        let (cmap, used) =
            crate::cli::commands::fetch_and_pack_content(store, embedder, &named_items, budget);
        (Some(cmap), Some((used, budget)))
    } else {
        (None, None)
    };

    if json {
        let output = build_scout_output(&result, content_map.as_ref(), token_info)?;
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
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

    // ===== TC-15: inject_content_into_scout_json tests =====

    #[test]
    fn tc15_inject_content_into_scout_json_known_shape() {
        // TC-15: verify inject_content_into_scout_json mutates content by name
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
        // TC-15: empty content_map should leave JSON unchanged
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
        // TC-15: content_map with names not in the JSON should not add fields
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
        // TC-15: JSON without file_groups key should be a safe no-op
        let mut json_val = json!({ "summary": { "total_files": 0 } });
        let mut content_map = std::collections::HashMap::new();
        content_map.insert("foo".to_string(), "content".to_string());

        // Should not panic
        crate::cli::commands::inject_content_into_scout_json(&mut json_val, &content_map);
        assert!(json_val.get("file_groups").is_none());
    }

    // ===== TC-15: inject_token_info tests =====

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
