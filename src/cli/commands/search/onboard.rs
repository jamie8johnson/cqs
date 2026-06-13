//! Onboard command — guided codebase tour for understanding a concept
//!
//! ## Command-core split (Phase 2b)
//!
//! [`onboard_core`] owns the surface-agnostic JSON assembly (run the `onboard`
//! lib primitive, truncate to the limit, serialize, token-pack, trust-tag).
//! Both the CLI ([`cmd_onboard`] JSON path) and the daemon (`dispatch_onboard`)
//! drive it, so the wire shape is identical. The CLI text path keeps the raw
//! [`cqs::OnboardResult`] for rendering.

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::onboard;
use cqs::store::{ReadOnly, Store};
use cqs::{Embedder, GatherDirection};

// ─── Args (surface-agnostic, MCP-ready) ─────────────────────────────────────

/// Input for [`onboard_core`] — the onboard knobs both the CLI and a future
/// MCP `onboard` tool deserialize into. Store/embedder/root come from the
/// adapter.
///
/// `#[serde(default)]` so a wire caller can supply just `query` and inherit the
/// production defaults (depth/direction/limit mirror clap).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct OnboardArgs {
    /// Concept or query to explore.
    pub query: String,
    /// Call-chain expansion depth (clamped 1..=5).
    pub depth: usize,
    /// Expansion direction: both / callers / callees.
    pub direction: GatherDirection,
    /// Cap on call_chain + callers + tests entries (entry_point always kept).
    pub limit: usize,
    /// Token budget — when set, packs chunk content into the budget.
    pub tokens: Option<usize>,
}

impl Default for OnboardArgs {
    fn default() -> Self {
        OnboardArgs {
            query: String::new(),
            // Mirrors clap: DEFAULT_DEPTH_WALK = 3, direction = callees,
            // LimitArg default = 5.
            depth: crate::cli::args::DEFAULT_DEPTH_WALK,
            direction: GatherDirection::Callees,
            limit: 5,
            tokens: None,
        }
    }
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs onboard` (JSON path). Runs the `onboard`
/// lib primitive, truncates each section to the limit, then assembles the
/// shared JSON (serialize → optional token-pack → trust-tag). Reads no env.
/// Returns the assembled value; the CLI adapter renders text from the raw
/// result instead.
pub(crate) fn onboard_core(
    store: &Store<ReadOnly>,
    embedder: &Embedder,
    root: &std::path::Path,
    args: &OnboardArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("onboard_core", query = %args.query).entered();
    let depth = args.depth.clamp(1, crate::cli::ONBOARD_DEPTH_CAP);
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    let mut result = onboard(store, embedder, &args.query, root, depth, args.direction)?;
    result.call_chain.truncate(limit);
    result.callers.truncate(limit);
    result.tests.truncate(limit);

    let mut output = serde_json::to_value(&result).context("Failed to serialize onboard result")?;

    if let Some(budget) = args.tokens {
        let named_items = crate::cli::commands::onboard_scored_names(&result);
        let (content_map, used) =
            crate::cli::commands::fetch_and_pack_content(store, embedder, &named_items, budget);
        crate::cli::commands::inject_content_into_onboard_json(&mut output, &content_map, &result);
        crate::cli::commands::inject_token_info(&mut output, Some((used, budget)));
    }

    // Onboard only queries the project store — every chunk is user-code.
    crate::cli::commands::tag_user_code_trust_level(&mut output);
    Ok(output)
}

pub(crate) fn cmd_onboard(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    concept: &str,
    depth: usize,
    direction: cqs::GatherDirection,
    limit: usize,
    json: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!(
        "cmd_onboard",
        concept,
        depth,
        ?direction,
        limit,
        ?max_tokens
    )
    .entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let embedder = ctx.embedder()?;

    // JSON path routes through the shared `onboard_core` (same code the daemon
    // runs), so the wire shape is identical across surfaces.
    if json {
        let args = OnboardArgs {
            query: concept.to_string(),
            depth,
            direction,
            limit,
            tokens: max_tokens,
        };
        let output = onboard_core(store, embedder, root, &args)?;
        crate::cli::json_envelope::emit_json(&output)?;
        return Ok(());
    }

    let depth = depth.clamp(1, crate::cli::ONBOARD_DEPTH_CAP);
    // Cap on call_chain + callers + tests entries. Applied AFTER the
    // BFS+search so the entry_point is always preserved and so the order
    // (relevance → depth) is respected before truncation.
    let limit = limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    let mut result = onboard(store, embedder, concept, root, depth, direction)?;
    result.call_chain.truncate(limit);
    result.callers.truncate(limit);
    result.tests.truncate(limit);

    {
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
            if result.summary.key_types_truncated > 0 {
                let total = result.key_types.len() + result.summary.key_types_truncated;
                println!(
                    "{}",
                    format!(
                        "── Key Types (showing {} of {}, raise CQS_ONBOARD_KEY_TYPES to see more) ──",
                        result.key_types.len(),
                        total
                    )
                    .cyan()
                    .bold()
                );
            } else {
                println!("{}", "── Key Types ──".cyan().bold());
            }
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

#[cfg(test)]
mod tests {
    use super::OnboardArgs;

    /// A wire/MCP caller can supply only `query` and inherit defaults.
    #[test]
    fn onboard_args_deserialize_minimal() {
        let args: OnboardArgs = serde_json::from_str(r#"{"query": "indexing"}"#).unwrap();
        assert_eq!(args.query, "indexing");
        assert_eq!(args.depth, 3);
        assert_eq!(args.direction, cqs::GatherDirection::Callees);
        assert_eq!(args.limit, 5);
        assert!(args.tokens.is_none());
    }

    /// `OnboardArgs::default` must match the clap `OnboardArgs` defaults.
    /// Parses `cqs onboard <query>` via a throwaway `clap::Parser` wrapper.
    #[test]
    fn onboard_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::OnboardArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-onboard", "q"]).unwrap().args;
        let core = OnboardArgs {
            query: clap_args.query.clone(),
            depth: clap_args.depth,
            direction: clap_args.direction,
            limit: clap_args.limit_arg.limit,
            tokens: clap_args.tokens,
        };
        let expected = OnboardArgs {
            query: "q".to_string(),
            ..OnboardArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap onboard defaults drifted from OnboardArgs::default — update both together"
        );
    }
}
