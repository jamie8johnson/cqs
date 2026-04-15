//! Onboard command — guided codebase tour for understanding a concept

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::onboard;

pub(crate) fn cmd_onboard(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    concept: &str,
    depth: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_onboard", concept, depth, ?max_tokens).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder()?;
    let depth = depth.clamp(1, 5);

    let result = onboard(store, embedder, concept, root, depth)?;

    if json {
        let mut output =
            serde_json::to_value(&result).context("Failed to serialize onboard result")?;

        // Token budgeting via shared helpers (same path as batch dispatch)
        if let Some(budget) = max_tokens {
            let named_items = crate::cli::commands::onboard_scored_names(&result);
            let (content_map, used) =
                crate::cli::commands::fetch_and_pack_content(store, embedder, &named_items, budget);
            crate::cli::commands::inject_content_into_onboard_json(
                &mut output,
                &content_map,
                &result,
            );
            crate::cli::commands::inject_token_info(&mut output, Some((used, budget)));
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
        print_entry(&result.entry_point, root);

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
                        print_entry(entry, root);
                    }
                }
            }
        }

        // Callers
        if !result.callers.is_empty() {
            println!();
            println!("{}", "── Callers ──".cyan().bold());
            for entry in &result.callers {
                let rel = cqs::rel_display(&entry.file, root);
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
                let rel = cqs::rel_display(&test.file, root);
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
