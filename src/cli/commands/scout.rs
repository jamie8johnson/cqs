//! Scout command — pre-investigation dashboard for task planning

use anyhow::Result;
use colored::Colorize;

use cqs::{scout, scout_to_json, Embedder};

pub(crate) fn cmd_scout(
    _cli: &crate::cli::Cli,
    task: &str,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_scout", task, ?max_tokens).entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let embedder = Embedder::new()?;
    let limit = limit.clamp(1, 10);

    let result = scout(&store, &embedder, task, &root, limit)?;

    // Token-budgeted content: fetch chunk content and pack into budget
    let (content_map, token_info) = if let Some(budget) = max_tokens {
        let _pack_span = tracing::info_span!("token_pack_scout", budget).entered();

        // Collect all chunk names from all groups
        let all_names: Vec<&str> = result
            .file_groups
            .iter()
            .flat_map(|g| g.chunks.iter().map(|c| c.name.as_str()))
            .collect();

        // Batch-fetch content from store
        let chunks_by_name = match store.get_chunks_by_names_batch(&all_names) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to batch-fetch chunk content for token packing");
                std::collections::HashMap::new()
            }
        };

        // Walk groups by relevance, chunks by search_score, pack content
        let mut used: usize = 0;
        let mut included: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        // Flatten to (name, search_score) sorted by relevance_score * search_score
        let mut scored: Vec<(&str, f32)> = result
            .file_groups
            .iter()
            .flat_map(|g| {
                g.chunks
                    .iter()
                    .map(move |c| (c.name.as_str(), g.relevance_score * c.search_score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        for (name, _score) in &scored {
            if let Some(summaries) = chunks_by_name.get(*name) {
                if let Some(summary) = summaries.first() {
                    let tokens = super::count_tokens(&embedder, &summary.content, name);
                    if used + tokens > budget && !included.is_empty() {
                        break;
                    }
                    used += tokens;
                    included.insert(name.to_string(), summary.content.clone());
                }
            }
        }

        tracing::info!(
            chunks_with_content = included.len(),
            tokens = used,
            budget,
            "Token-budgeted scout"
        );
        (Some(included), Some((used, budget)))
    } else {
        (None, None)
    };

    if json {
        let mut output = scout_to_json(&result, &root);
        // Inject content into chunks that fit in the token budget
        if let Some(ref cmap) = content_map {
            if let Some(groups) = output.get_mut("file_groups").and_then(|v| v.as_array_mut()) {
                for group in groups.iter_mut() {
                    if let Some(chunks) = group.get_mut("chunks").and_then(|v| v.as_array_mut()) {
                        for chunk in chunks.iter_mut() {
                            if let Some(name) = chunk.get("name").and_then(|v| v.as_str()) {
                                if let Some(content) = cmap.get(name) {
                                    chunk["content"] = serde_json::json!(content);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some((used, budget)) = token_info {
            output["token_count"] = serde_json::json!(used);
            output["token_budget"] = serde_json::json!(budget);
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
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
                let rel = cqs::rel_display(&group.file, &root);

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
                            println!("{}", "─".repeat(50));
                            println!("{}", content);
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
