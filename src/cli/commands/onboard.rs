//! Onboard command — guided codebase tour for understanding a concept

use anyhow::Result;
use colored::Colorize;

use cqs::{onboard, onboard_to_json, Embedder};

pub(crate) fn cmd_onboard(
    _cli: &crate::cli::Cli,
    concept: &str,
    depth: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_onboard", concept, depth, ?max_tokens).entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let embedder = Embedder::new()?;
    let depth = depth.clamp(1, 5);

    let result = onboard(&store, &embedder, concept, &root, depth)?;

    if json {
        let mut output = onboard_to_json(&result);

        // Token budgeting: pack entry content into budget
        if let Some(budget) = max_tokens {
            let all_content: Vec<(&str, &str, f32)> = std::iter::once((
                result.entry_point.name.as_str(),
                result.entry_point.content.as_str(),
                1.0_f32,
            ))
            .chain(result.call_chain.iter().map(|e| {
                (
                    e.name.as_str(),
                    e.content.as_str(),
                    1.0 / (e.depth as f32 + 1.0),
                )
            }))
            .chain(
                result
                    .callers
                    .iter()
                    .map(|e| (e.name.as_str(), e.content.as_str(), 0.3_f32)),
            )
            .collect();

            let texts: Vec<&str> = all_content.iter().map(|(_, c, _)| *c).collect();
            let token_counts = super::count_tokens_batch(&embedder, &texts);
            let total_tokens: usize = token_counts.iter().sum();

            if total_tokens > budget {
                // Pack greedily — higher score = higher priority
                let items: Vec<(String, f32)> = all_content
                    .iter()
                    .map(|(name, _, score)| (name.to_string(), *score))
                    .collect();
                let (packed, used) =
                    super::token_pack(items, &token_counts, budget, 0, |&(_, score)| score);
                let included: std::collections::HashSet<String> =
                    packed.into_iter().map(|(name, _)| name).collect();

                // Remove content from entries not in budget
                if let Some(ep) = output.get_mut("entry_point") {
                    if !included.contains(result.entry_point.name.as_str()) {
                        ep["content"] = serde_json::json!("");
                    }
                }
                if let Some(chain) = output.get_mut("call_chain").and_then(|v| v.as_array_mut()) {
                    for entry in chain.iter_mut() {
                        if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                            if !included.contains(name) {
                                entry["content"] = serde_json::json!("");
                            }
                        }
                    }
                }
                if let Some(callers) = output.get_mut("callers").and_then(|v| v.as_array_mut()) {
                    for entry in callers.iter_mut() {
                        if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                            if !included.contains(name) {
                                entry["content"] = serde_json::json!("");
                            }
                        }
                    }
                }

                output["token_count"] = serde_json::json!(used);
                output["token_budget"] = serde_json::json!(budget);
            } else {
                output["token_count"] = serde_json::json!(total_tokens);
                output["token_budget"] = serde_json::json!(budget);
            }
        }

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Text output
        println!(
            "{} {}",
            "Onboard:".cyan(),
            format!("\"{}\"", concept).bold()
        );

        // Entry point
        println!();
        println!("{}", "── Entry Point ──".cyan().bold());
        print_entry(&result.entry_point, &root);

        // Call chain by depth
        if !result.call_chain.is_empty() {
            let max_depth = result.call_chain.iter().map(|e| e.depth).max().unwrap_or(0);
            for d in 1..=max_depth {
                let at_depth: Vec<&cqs::OnboardEntry> =
                    result.call_chain.iter().filter(|e| e.depth == d).collect();
                if !at_depth.is_empty() {
                    println!();
                    println!("{}", format!("── Call Chain (depth {d}) ──").cyan().bold());
                    for entry in at_depth {
                        print_entry(entry, &root);
                    }
                }
            }
        }

        // Callers
        if !result.callers.is_empty() {
            println!();
            println!("{}", "── Callers ──".cyan().bold());
            for entry in &result.callers {
                let rel = cqs::rel_display(&entry.file, &root);
                println!(
                    "  {}:{}  {}",
                    rel,
                    entry.line_start,
                    entry.signature.dimmed()
                );
            }
        }

        // Key types
        if !result.key_types.is_empty() {
            println!();
            println!("{}", "── Key Types ──".cyan().bold());
            let type_strs: Vec<String> = result
                .key_types
                .iter()
                .map(|t| format!("{} ({})", t.type_name, t.edge_kind))
                .collect();
            println!("  {}", type_strs.join("  ·  ").dimmed());
        }

        // Tests
        if !result.tests.is_empty() {
            println!();
            println!("{}", "── Tests ──".cyan().bold());
            for test in &result.tests {
                let rel = cqs::rel_display(&test.file, &root);
                println!(
                    "  {}:{}  {} {}",
                    rel,
                    test.line,
                    test.name,
                    format!("(depth {})", test.call_depth).dimmed()
                );
            }
        }

        // Summary
        println!();
        println!(
            "{} {} item{} across {} file{}, {} callee depth, {} test{}",
            "Summary:".cyan(),
            result.summary.total_items,
            if result.summary.total_items == 1 {
                ""
            } else {
                "s"
            },
            result.summary.files_covered,
            if result.summary.files_covered == 1 {
                ""
            } else {
                "s"
            },
            result.summary.callee_depth,
            result.summary.tests_found,
            if result.summary.tests_found == 1 {
                ""
            } else {
                "s"
            },
        );
    }

    Ok(())
}

fn print_entry(entry: &cqs::OnboardEntry, root: &std::path::Path) {
    let rel = cqs::rel_display(&entry.file, root);
    println!(
        "  {}:{}  {}",
        rel.bold(),
        entry.line_start,
        entry.name.bold()
    );
    println!("  {}", entry.signature.dimmed());
    if !entry.content.is_empty() {
        // Show first 20 lines of content
        let lines: Vec<&str> = entry.content.lines().take(20).collect();
        println!("{}", "─".repeat(50));
        for line in &lines {
            println!("{}", line);
        }
        let total_lines = entry.content.lines().count();
        if total_lines > 20 {
            println!(
                "{}",
                format!("  ... ({} more lines)", total_lines - 20).dimmed()
            );
        }
        println!();
    }
}
