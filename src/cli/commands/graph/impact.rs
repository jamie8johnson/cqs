//! Impact command — what breaks if you change a function
//!
//! ## Polymorphic routing (Phase 1, partial)
//!
//! `cqs impact <name>` historically required a function-or-method name and
//! returned an empty `Vec` for any other chunk kind (consts, types, etc.) —
//! the misrouted-to-empty failure mode the polymorphic-routing design
//! (`docs/polymorphic-routing.md`) targets. This module now consults
//! `cqs::kind::classify_hits` against an exact-name lookup before running
//! the call-graph analysis. For [`Kind::Const`] the response is a
//! kind-labeled definition list with a redirect note instead of empty.
//! Other non-Function kinds (Type, Module, Ambiguous, ...) still fall
//! through to the existing flow until their per-(command × kind) cells
//! land in follow-up PRs.

use anyhow::Result;

use cqs::kind::{classify_hits, Kind, KindHit};
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

        // P4-3 (#1463): exhaustive match — adding a new `OutputFormat`
        // variant fails to compile until every render site adds an arm.
        match format {
            OutputFormat::Mermaid => {
                println!("{}", impact_to_mermaid(&result));
            }
            OutputFormat::Json => {
                let json = impact_to_json_with_kind(&result, "function")?;
                crate::cli::json_envelope::emit_json(&json)?;
            }
            OutputFormat::Text => {
                let rel_file = "(cross-project)";
                display_impact_text(&result, root, rel_file);
            }
        }
        return Ok(());
    }

    // Polymorphic-routing kind detection (Phase 1, partial). Read once
    // up-front so the dispatcher branch happens before the resolve-+-
    // analyze flow that assumes a function. Hits double-duty as the
    // input to the Const fallback below — one SQL query covers both.
    let chunks = store.lookup_by_name(name)?;
    let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
    let kind = classify_hits(&hits);
    if matches!(kind, Kind::Const) {
        return cmd_impact_const_fallback(name, &chunks, format);
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

    match format {
        OutputFormat::Mermaid => {
            println!("{}", impact_to_mermaid(&result));
        }
        OutputFormat::Json => {
            let mut json = impact_to_json_with_kind(&result, "function")?;
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
        }
        OutputFormat::Text => {
            let rel_file = cqs::rel_display(&chunk.file, root);
            display_impact_text(&result, root, &rel_file);

            if do_suggest_tests && !suggestions.is_empty() {
                display_test_suggestions(&suggestions);
            }
        }
    }

    Ok(())
}

/// Wrap [`cqs::impact_to_json`] with a top-level `kind` field so agents
/// can detect whether the response came from the function-shaped happy
/// path or a kind-mismatch fallback. Polymorphic-routing Phase 1.
fn impact_to_json_with_kind(result: &cqs::ImpactResult, kind: &str) -> Result<serde_json::Value> {
    let mut json = impact_to_json(result)?;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("kind".into(), serde_json::json!(kind));
    }
    Ok(json)
}

/// Build the JSON shape for the const fallback. Pure function — printing
/// happens in [`cmd_impact_const_fallback`]. Factored out so tests can
/// pin the response shape without stdout capture.
///
/// **Response shape:**
/// - `kind`: `"const"`
/// - `fallback_from`: `"impact"` so a downstream agent can detect that
///   the original command's contract didn't apply (and the response is
///   reroute-shaped, not the call-graph shape).
/// - `name`: the exact-matched name as queried.
/// - `definitions`: one entry per matched chunk (file/line/language/chunk_type/
///   signature/content). For multi-language consts the full list is returned.
/// - `note`: human-readable redirect to `cqs <name>` / `cqs search <name>`
///   for finding usage references.
fn build_impact_const_fallback_json(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
) -> serde_json::Value {
    let definitions: Vec<serde_json::Value> = chunks
        .iter()
        .map(|c| {
            serde_json::json!({
                "file": cqs::normalize_path(&c.file),
                "line_start": c.line_start,
                "line_end": c.line_end,
                "language": c.language.to_string(),
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "content": c.content,
            })
        })
        .collect();
    serde_json::json!({
        "kind": "const",
        "fallback_from": "impact",
        "name": name,
        "definitions": definitions,
        "note": "consts don't have call-graph impact; here are the definition sites. \
                 Use `cqs <name>` or `cqs search <name>` to find references.",
    })
}

/// Polymorphic-routing fallback: `cqs impact <const>` returns a kind-
/// labeled definition list + redirect note instead of empty. Pre-fix,
/// `analyze_impact` against a const name would resolve a chunk that
/// the call-graph layer doesn't track and silently return zero
/// callers. The agent saw `[]`, fell through to grep, and the const
/// became another datapoint in the 79% → 6% search-rate decline.
///
/// Text / Mermaid surfaces print a plain-text equivalent.
fn cmd_impact_const_fallback(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
    format: &OutputFormat,
) -> Result<()> {
    debug_assert!(
        !chunks.is_empty(),
        "Const fallback called with no hits — caller must check Kind::Const before dispatching"
    );
    match format {
        OutputFormat::Json => {
            let json = build_impact_const_fallback_json(name, chunks);
            crate::cli::json_envelope::emit_json(&json)?;
        }
        OutputFormat::Text | OutputFormat::Mermaid => {
            println!(
                "(impact) `{}` is a const, not a function — call-graph impact analysis doesn't apply.",
                name
            );
            println!();
            println!("Definitions:");
            for c in chunks {
                println!(
                    "  {}:{}-{} ({} {})",
                    cqs::normalize_path(&c.file),
                    c.line_start,
                    c.line_end,
                    c.language,
                    c.chunk_type
                );
                if !c.signature.is_empty() {
                    println!("    {}", c.signature);
                }
            }
            println!();
            println!("Use `cqs <name>` or `cqs search <name>` to find references.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ChunkSummary;
    use std::path::PathBuf;

    fn make_const_chunk(name: &str, file: &str, line: u32) -> ChunkSummary {
        ChunkSummary {
            id: format!("{}:{}:{}", file, line, "abcd1234"),
            file: PathBuf::from(file),
            language: cqs::parser::Language::Rust,
            chunk_type: cqs::parser::ChunkType::Constant,
            name: name.to_string(),
            signature: format!("pub const {}: &str = \"...\";", name),
            content: format!("pub const {}: &str = \"...\";", name),
            doc: None,
            line_start: line,
            line_end: line,
            content_hash: "abcd1234".to_string(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }

    #[test]
    fn impact_const_fallback_emits_kind_and_fallback_from() {
        let chunk = make_const_chunk("HANDLING_ADVICE", "src/json.rs", 73);
        let json = build_impact_const_fallback_json("HANDLING_ADVICE", &[chunk]);

        assert_eq!(json["kind"], "const");
        assert_eq!(json["fallback_from"], "impact");
        assert_eq!(json["name"], "HANDLING_ADVICE");
        assert_eq!(
            json["definitions"].as_array().unwrap().len(),
            1,
            "single chunk → single definition"
        );
        assert!(
            json["note"].as_str().unwrap().contains("cqs search"),
            "note should redirect to search"
        );
    }

    #[test]
    fn impact_const_fallback_definitions_carry_file_and_content() {
        let chunk = make_const_chunk("X", "src/foo.rs", 42);
        let json = build_impact_const_fallback_json("X", &[chunk]);
        let def = &json["definitions"][0];

        assert_eq!(def["file"], "src/foo.rs");
        assert_eq!(def["line_start"], 42);
        assert_eq!(def["line_end"], 42);
        assert_eq!(def["chunk_type"], "constant");
        assert_eq!(def["language"], "rust");
        assert!(def["content"].as_str().unwrap().contains("pub const X"));
    }

    #[test]
    fn impact_const_fallback_returns_all_definitions_when_multi_language() {
        // A const defined in multiple languages (or files) should surface
        // every definition. The Const fallback is the place to disclose
        // ambiguity, not silently pick one.
        let c1 = make_const_chunk("VERSION", "src/lib.rs", 5);
        let mut c2 = make_const_chunk("VERSION", "include/version.h", 10);
        c2.language = cqs::parser::Language::C;
        let json = build_impact_const_fallback_json("VERSION", &[c1, c2]);

        let defs = json["definitions"].as_array().unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0]["language"], "rust");
        assert_eq!(defs[1]["language"], "c");
    }

    #[test]
    fn impact_to_json_with_kind_injects_function_label() {
        // Pin the function-path label so a future schema audit catches
        // a regression that drops the `kind` field on the happy path.
        let result = cqs::ImpactResult {
            function_name: "foo".to_string(),
            callers: Vec::new(),
            transitive_callers: Vec::new(),
            tests: Vec::new(),
            type_impacted: Vec::new(),
            degraded: false,
        };
        let json = impact_to_json_with_kind(&result, "function").unwrap();
        assert_eq!(json["kind"], "function");
    }
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
