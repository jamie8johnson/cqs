//! Scout command — pre-investigation dashboard for task planning

use anyhow::Result;
use colored::Colorize;

use cqs::{scout, scout_to_json, Embedder};

pub(crate) fn cmd_scout(
    _cli: &crate::cli::Cli,
    task: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_scout", task).entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let embedder = Embedder::new()?;
    let limit = limit.clamp(1, 10);

    let result = scout(&store, &embedder, task, &root, limit)?;

    if json {
        let output = scout_to_json(&result, &root);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{} {}", "Scout:".cyan(), task.bold());

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
