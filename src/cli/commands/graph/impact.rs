//! Impact command — what breaks if you change a function

use anyhow::Result;

use cqs::{
    analyze_impact, format_test_suggestions, impact_to_json, impact_to_mermaid, suggest_tests,
    ImpactOptions,
};

use crate::cli::commands::resolve::resolve_target;
use crate::cli::OutputFormat;

// Task A3 added `limit` as the 8th parameter; the CLI dispatcher inflates a
// shared arg struct rather than calling this directly, so we accept the lint
// here instead of forcing every call site through a wrapper.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_impact(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    depth: usize,
    limit: usize,
    format: &OutputFormat,
    do_suggest_tests: bool,
    include_types: bool,
    cross_project: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_impact", name, limit, cross_project).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let depth = depth.clamp(1, 10);
    // Task A3: per-section truncation cap. Default 5 from `LimitArg`. The
    // analyzer returns the full result; we apply the cap at render time so
    // the underlying graph data is unaffected (other consumers — mermaid,
    // suggest_tests — still see the full set).
    let limit = limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(root)?;
        let mut result = cqs::cross_project::analyze_impact_cross(
            &mut cross_ctx,
            name,
            depth,
            do_suggest_tests,
            include_types,
        )?;
        truncate_impact_sections(&mut result, limit);

        if matches!(format, OutputFormat::Mermaid) {
            println!("{}", impact_to_mermaid(&result));
            return Ok(());
        }
        if matches!(format, OutputFormat::Json) {
            let json = impact_to_json(&result);
            crate::cli::json_envelope::emit_json(&json)?;
        } else {
            let rel_file = "(cross-project)";
            display_impact_text(&result, root, rel_file);
        }
        return Ok(());
    }

    // Resolve target
    let resolved = resolve_target(store, name)?;
    let chunk = resolved.chunk;

    // Run shared impact analysis
    let mut result = analyze_impact(
        store,
        &chunk.name,
        root,
        &ImpactOptions {
            depth,
            include_types,
        },
    )?;

    // Compute test suggestions if requested (BEFORE truncation so the
    // suggestion engine sees every untested caller, not just the first N).
    let suggestions = if do_suggest_tests {
        suggest_tests(store, &result, root)
    } else {
        Vec::new()
    };

    truncate_impact_sections(&mut result, limit);

    if matches!(format, OutputFormat::Mermaid) {
        println!("{}", impact_to_mermaid(&result));
        return Ok(());
    }

    if matches!(format, OutputFormat::Json) {
        let mut json = impact_to_json(&result);
        if do_suggest_tests {
            let suggestions_json = format_test_suggestions(&suggestions);
            if let Some(obj) = json.as_object_mut() {
                obj.insert(
                    "test_suggestions".into(),
                    serde_json::json!(suggestions_json),
                );
            }
        }
        crate::cli::json_envelope::emit_json(&json)?;
    } else {
        let rel_file = cqs::rel_display(&chunk.file, root);
        display_impact_text(&result, root, &rel_file);

        if do_suggest_tests && !suggestions.is_empty() {
            display_test_suggestions(&suggestions);
        }
    }

    Ok(())
}

/// Task A3: truncate each list inside `ImpactResult` to `limit`. Operates
/// in-place — used by both the local and cross-project paths in `cmd_impact`.
/// Direct callers, transitive callers, affected tests, and type-impacted
/// callers each get the same cap (a single `--limit` controls all four
/// sections; no per-section knob today).
fn truncate_impact_sections(result: &mut cqs::ImpactResult, limit: usize) {
    result.callers.truncate(limit);
    result.transitive_callers.truncate(limit);
    result.tests.truncate(limit);
    result.type_impacted.truncate(limit);
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
        println!(
            "    {}",
            format!("in {}", s.suggested_file.display()).dimmed()
        );
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
