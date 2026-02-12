//! Impact command — what breaks if you change a function

use anyhow::Result;

use cqs::{analyze_impact, impact_to_json, impact_to_mermaid, suggest_tests, Store};

use crate::cli::find_project_root;

use super::resolve::resolve_target;

pub(crate) fn cmd_impact(
    _cli: &crate::cli::Cli,
    name: &str,
    depth: usize,
    format: &str,
    do_suggest_tests: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_impact", name).entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    let depth = depth.clamp(1, 10);

    // Resolve target
    let (chunk, _) = resolve_target(&store, name)?;

    // Run shared impact analysis
    let result = analyze_impact(&store, &chunk.name, depth)?;

    // Compute test suggestions if requested
    let suggestions = if do_suggest_tests {
        suggest_tests(&store, &result)
    } else {
        Vec::new()
    };

    if format == "mermaid" {
        println!("{}", impact_to_mermaid(&result, &root));
        return Ok(());
    }

    if format == "json" {
        let mut json = impact_to_json(&result, &root);
        if do_suggest_tests {
            let suggestions_json: Vec<_> = suggestions
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "test_name": s.test_name,
                        "suggested_file": s.suggested_file,
                        "for_function": s.for_function,
                        "pattern_source": s.pattern_source,
                        "inline": s.inline,
                    })
                })
                .collect();
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    "test_suggestions".into(),
                    serde_json::json!(suggestions_json),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        let rel_file = chunk
            .file
            .strip_prefix(&root)
            .unwrap_or(&chunk.file)
            .to_string_lossy()
            .replace('\\', "/");
        display_impact_text(&result, &root, &rel_file);

        if do_suggest_tests && !suggestions.is_empty() {
            display_test_suggestions(&suggestions);
        }
    }

    Ok(())
}

/// Display test suggestions with colored output
fn display_test_suggestions(suggestions: &[cqs::TestSuggestion]) {
    use colored::Colorize;

    println!();
    println!(
        "{} ({} untested {}):",
        "Suggested Tests".yellow(),
        suggestions.len(),
        if suggestions.len() == 1 {
            "caller"
        } else {
            "callers"
        }
    );
    for s in suggestions {
        let location = if s.inline { "inline" } else { "new file" };
        println!(
            "  {} {} {} ({})",
            s.for_function.bold(),
            "→".dimmed(),
            s.test_name,
            location.dimmed()
        );
        println!("    {}", format!("in {}", s.suggested_file).dimmed());
        if !s.pattern_source.is_empty() {
            println!(
                "    {}",
                format!("pattern from: {}", s.pattern_source).dimmed()
            );
        }
    }
}

/// Terminal display with colored output (CLI-only)
fn display_impact_text(result: &cqs::ImpactResult, root: &std::path::Path, target_file: &str) {
    use colored::Colorize;

    println!("{} ({})", result.function_name.bold(), target_file);

    // Direct callers
    if result.callers.is_empty() {
        println!();
        println!("{}", "No callers found.".dimmed());
    } else {
        println!();
        println!("{} ({}):", "Callers".cyan(), result.callers.len());
        for c in &result.callers {
            let rel = c.file.strip_prefix(root).unwrap_or(&c.file);
            println!(
                "  {} ({}:{}, call at line {})",
                c.name,
                rel.display(),
                c.line,
                c.call_line
            );
            if let Some(ref snippet) = c.snippet {
                for line in snippet.lines() {
                    println!("    {}", line.dimmed());
                }
            }
        }
    }

    // Transitive callers
    if !result.transitive_callers.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Transitive Callers".cyan(),
            result.transitive_callers.len()
        );
        for c in &result.transitive_callers {
            let rel = c.file.strip_prefix(root).unwrap_or(&c.file);
            println!(
                "  {} ({}:{}) [depth {}]",
                c.name,
                rel.display(),
                c.line,
                c.depth
            );
        }
    }

    // Tests
    if result.tests.is_empty() {
        println!();
        println!("{}", "No affected tests found.".dimmed());
    } else {
        println!();
        println!("{} ({}):", "Affected Tests".yellow(), result.tests.len());
        for t in &result.tests {
            let rel = t.file.strip_prefix(root).unwrap_or(&t.file);
            println!(
                "  {} ({}:{}) [depth {}]",
                t.name,
                rel.display(),
                t.line,
                t.call_depth
            );
        }
    }
}
