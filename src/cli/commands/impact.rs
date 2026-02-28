//! Impact command — what breaks if you change a function

use anyhow::Result;

use cqs::{analyze_impact, impact_to_json, impact_to_mermaid, suggest_tests};

use super::resolve::resolve_target;
use crate::cli::OutputFormat;

pub(crate) fn cmd_impact(
    name: &str,
    depth: usize,
    format: &OutputFormat,
    do_suggest_tests: bool,
    include_types: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_impact", name).entered();
    let (store, root, _) = crate::cli::open_project_store()?;
    let depth = depth.clamp(1, 10);

    // Resolve target
    let resolved = resolve_target(&store, name)?;
    let chunk = resolved.chunk;

    // Run shared impact analysis
    let result = analyze_impact(&store, &chunk.name, depth, include_types)?;

    // Compute test suggestions if requested
    let suggestions = if do_suggest_tests {
        suggest_tests(&store, &result)
    } else {
        Vec::new()
    };

    if matches!(format, OutputFormat::Mermaid) {
        println!("{}", impact_to_mermaid(&result, &root));
        return Ok(());
    }

    if matches!(format, OutputFormat::Json) {
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
        let rel_file = cqs::rel_display(&chunk.file, &root);
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
            let rel = cqs::rel_display(&c.file, root);
            println!(
                "  {} ({}:{}, call at line {})",
                c.name, rel, c.line, c.call_line
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
            let rel = cqs::rel_display(&c.file, root);
            println!("  {} ({}:{}) [depth {}]", c.name, rel, c.line, c.depth);
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
            let rel = cqs::rel_display(&t.file, root);
            println!("  {} ({}:{}) [depth {}]", t.name, rel, t.line, t.call_depth);
        }
    }

    // Type-impacted functions
    if !result.type_impacted.is_empty() {
        println!();
        println!(
            "{} ({}):",
            "Type-Impacted".magenta(),
            result.type_impacted.len()
        );
        for ti in &result.type_impacted {
            let rel = cqs::rel_display(&ti.file, root);
            println!(
                "  {} ({}:{}) via {}",
                ti.name,
                rel,
                ti.line,
                ti.shared_types.join(", ")
            );
        }
    }
}
