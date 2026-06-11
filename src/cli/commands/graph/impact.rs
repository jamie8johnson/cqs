//! Impact command — what breaks if you change a function
//!
//! ## Polymorphic routing
//!
//! `cqs impact <name>` consults `cqs::kind::classify_hits` against an
//! exact-name lookup before running the call-graph analysis. For
//! `Resolved(Kind::Const)`, `Resolved(Kind::Type)`, `Resolved(Kind::Module)`,
//! and `KindResolution::Ambiguous` the response is a kind-labeled definition
//! list with a redirect note instead of empty. Function and other
//! resolutions fall through to the call-graph analysis flow.

use anyhow::Result;

use cqs::store::{ReadOnly, Store};
use cqs::{
    analyze_impact, format_test_suggestions, impact_to_json, impact_to_mermaid, suggest_tests,
    ImpactOptions, ImpactResult, TestSuggestion,
};

use super::notes_text;
use super::KindFallbackOutput;
use crate::cli::commands::resolve::resolve_target;
use crate::cli::OutputFormat;

// ─── Args (surface-agnostic, MCP-ready) ────────────────────────────────────

/// Input for [`impact_core`]. Cross-project impact lives in the adapters
/// (separate cross-project analyzer); the core covers the single-project
/// path both surfaces share.
#[derive(Debug, serde::Deserialize)]
#[serde(default)]
pub(crate) struct ImpactArgs {
    /// Function name or `file:function`.
    pub name: String,
    /// Caller depth (1=direct, 2+=transitive); clamped 1..=10 in the core.
    pub depth: usize,
    /// Per-section truncation cap; clamped 1..=100 in the core.
    pub limit: usize,
    /// Suggest tests for untested callers.
    pub suggest_tests: bool,
    /// Include type-impacted functions (shared type dependencies).
    pub include_types: bool,
}

impl Default for ImpactArgs {
    fn default() -> Self {
        Self {
            name: String::new(),
            // Mirrors clap `--depth` default (`DEFAULT_DEPTH_BLAST`).
            depth: crate::cli::args::DEFAULT_DEPTH_BLAST,
            // Mirrors clap `LimitArg` default.
            limit: crate::cli::args::DEFAULT_LIMIT,
            suggest_tests: false,
            include_types: false,
        }
    }
}

// ─── Core output ────────────────────────────────────────────────────────────

/// Single source of truth for `cqs impact <name>` output. The function
/// path keeps the typed [`ImpactResult`] + suggestions so the CLI's text
/// rendering reads structured data; [`ImpactCoreOutput::to_value`] is the
/// single JSON projection (lib `impact_to_json` + injected `kind:
/// "function"` + optional `test_suggestions`). A kind mismatch carries the
/// shared fallback object.
///
/// This enum can't simply derive `Serialize` because the function path's
/// JSON is produced by the lib's `impact_to_json` transform, not a field
/// projection — `to_value` is the explicit serializer both surfaces use.
pub(crate) enum ImpactCoreOutput {
    /// Function path: the impact analysis result plus any test suggestions.
    /// `suggest_tests` records whether suggestions were *requested* so the
    /// JSON projection emits `test_suggestions` (possibly empty) exactly
    /// when the flag was set — matching the historical wire shape.
    Function {
        result: ImpactResult,
        suggestions: Vec<TestSuggestion>,
        suggest_tests: bool,
    },
    /// Kind mismatch (const/type/module/ambiguous): shared fallback object.
    Fallback(KindFallbackOutput),
}

impl ImpactCoreOutput {
    /// Project to the JSON value both surfaces emit. Function path:
    /// `impact_to_json(result)` with `kind: "function"` injected and, when
    /// test suggestions were requested, a `test_suggestions` array (empty
    /// if none were found). Fallback: the serialized [`KindFallbackOutput`].
    pub(crate) fn to_value(&self) -> Result<serde_json::Value> {
        match self {
            ImpactCoreOutput::Function {
                result,
                suggestions,
                suggest_tests,
            } => {
                let mut json = impact_to_json(result)?;
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("kind".into(), serde_json::json!("function"));
                    if *suggest_tests {
                        let suggestions_json = format_test_suggestions(suggestions);
                        obj.insert(
                            "test_suggestions".into(),
                            serde_json::json!(suggestions_json),
                        );
                    }
                }
                Ok(json)
            }
            ImpactCoreOutput::Fallback(fb) => Ok(serde_json::to_value(fb)?),
        }
    }
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs impact <name>` (single-project).
///
/// Classifies the name, then either runs the call-graph impact analysis
/// (Function / Multiple / Other / NotFound — Multiple resolves
/// deterministically, NotFound surfaces a not-found error from
/// `resolve_target`) or returns a kind-labeled fallback (Const / Type /
/// Module / Ambiguous). Test suggestions are computed off the
/// un-truncated result so the engine sees every untested caller; the
/// per-section cap is applied immediately after.
pub(crate) fn impact_core(
    store: &Store<ReadOnly>,
    root: &std::path::Path,
    args: &ImpactArgs,
) -> Result<ImpactCoreOutput> {
    let _span = tracing::info_span!("impact_core", name = %args.name, limit = args.limit).entered();
    let depth = args.depth.clamp(1, crate::cli::IMPACT_DEPTH_CAP);
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::impact(fk);
        return Ok(ImpactCoreOutput::Fallback(KindFallbackOutput::new(
            &args.name, &chunks, fk, "impact", &text,
        )));
    }

    // Resolve target + run shared impact analysis.
    let resolved = resolve_target(store, &args.name)?;
    let chunk = resolved.chunk;
    let mut result = analyze_impact(
        store,
        &chunk.name,
        root,
        &ImpactOptions {
            depth,
            include_types: args.include_types,
        },
    )?;

    // Compute suggestions BEFORE truncation so the engine sees every
    // untested caller, not just the first N.
    let suggestions = if args.suggest_tests {
        suggest_tests(store, &result, root)
    } else {
        Vec::new()
    };

    truncate_impact_sections(&mut result, limit);

    Ok(ImpactCoreOutput::Function {
        result,
        suggestions,
        suggest_tests: args.suggest_tests,
    })
}

// ─── Cross-project core ──────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs impact <name> --cross-project`.
///
/// Runs the cross-project BFS impact analysis, applies the shared `1..=100`
/// per-section cap, and returns the truncated [`ImpactResult`]. Unlike the
/// single-project core this carries no kind-fallback and no test suggestions
/// — the cross-project JSON has always been the bare `impact_to_json(result)`
/// shape (no `kind` / `test_suggestions`). Both surfaces (CLI cross branch,
/// daemon cross branch) call this so the retrieval + truncate discipline
/// can't drift; the adapter chooses text / mermaid / JSON rendering.
pub(crate) fn impact_cross_core(
    cross_ctx: &mut cqs::cross_project::CrossProjectContext,
    args: &ImpactArgs,
) -> Result<cqs::ImpactResult> {
    let _span =
        tracing::info_span!("impact_cross_core", name = %args.name, limit = args.limit).entered();
    let depth = args.depth.clamp(1, crate::cli::IMPACT_DEPTH_CAP);
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);
    let mut result = cqs::cross_project::analyze_impact_cross(
        cross_ctx,
        &args.name,
        depth,
        args.suggest_tests,
        args.include_types,
    )?;
    truncate_impact_sections(&mut result, limit);
    Ok(result)
}

// ─── CLI command (thin adapter over the core) ──────────────────────────────

// The CLI dispatcher inflates a shared arg struct rather than calling this
// directly, so we accept the lint here instead of forcing every call site
// through a wrapper.
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

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(root)?;
        let result = impact_cross_core(
            &mut cross_ctx,
            &ImpactArgs {
                name: name.to_string(),
                depth,
                limit,
                suggest_tests: do_suggest_tests,
                include_types,
            },
        )?;

        // Exhaustive match — adding a new `OutputFormat` variant fails to
        // compile until every render site adds an arm.
        match format {
            OutputFormat::Mermaid => {
                println!("{}", impact_to_mermaid(&result));
            }
            OutputFormat::Json => {
                // Cross-project JSON never emitted test_suggestions (the
                // historical path called `impact_to_json` directly), so
                // request=false keeps the wire shape.
                let out = ImpactCoreOutput::Function {
                    result,
                    suggestions: Vec::new(),
                    suggest_tests: false,
                };
                crate::cli::json_envelope::emit_json(&out.to_value()?)?;
            }
            OutputFormat::Text => {
                let rel_file = "(cross-project)";
                display_impact_text(&result, root, rel_file);
            }
        }
        return Ok(());
    }

    let args = ImpactArgs {
        name: name.to_string(),
        depth,
        limit,
        suggest_tests: do_suggest_tests,
        include_types,
    };
    let output = impact_core(store, root, &args)?;

    match &output {
        ImpactCoreOutput::Fallback(fb) => match format {
            OutputFormat::Json => {
                crate::cli::json_envelope::emit_json(&output.to_value()?)?;
                let _ = fb;
            }
            OutputFormat::Text | OutputFormat::Mermaid => {
                render_impact_fallback_text(name, store)?;
            }
        },
        ImpactCoreOutput::Function {
            result,
            suggestions,
            suggest_tests: _,
        } => match format {
            OutputFormat::Mermaid => {
                println!("{}", impact_to_mermaid(result));
            }
            OutputFormat::Json => {
                crate::cli::json_envelope::emit_json(&output.to_value()?)?;
            }
            OutputFormat::Text => {
                // Re-resolve for the relative file label the text header
                // prints. The core already validated resolution, so this
                // is an indexed lookup that can't meaningfully fail; on the
                // off chance it does, fall back to the bare function name.
                let rel_file = resolve_target(store, name)
                    .map(|r| cqs::rel_display(&r.chunk.file, root))
                    .unwrap_or_else(|_| result.function_name.clone());
                display_impact_text(result, root, &rel_file);
                if do_suggest_tests && !suggestions.is_empty() {
                    display_test_suggestions(suggestions);
                }
            }
        },
    }

    Ok(())
}

/// Plain-text/mermaid impact fallback renderer. The core decided a
/// fallback fires; for text the adapter re-runs `detect_fallback` (cheap
/// indexed lookup) to print the definition list.
fn render_impact_fallback_text(name: &str, store: &Store<ReadOnly>) -> Result<()> {
    let (chunks, fallback) = super::detect_fallback(store, name);
    if let Some(fk) = fallback {
        let text = notes_text::impact(fk);
        let lead = notes_text::impact_lead(fk, name);
        super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Definitions:");
    }
    Ok(())
}

/// Truncate each list inside `ImpactResult` to `limit`. Operates in-place
/// — used by both the local and cross-project paths in `cmd_impact`.
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

#[cfg(test)]
mod tests {
    use super::super::notes_text::FallbackKind;
    use super::super::{
        chunks_to_definitions, KindFallbackOutput, KIND_FALLBACK_MAX_CONTENT_BYTES,
        KIND_FALLBACK_MAX_DEFINITIONS,
    };
    use super::*;
    use cqs::store::ChunkSummary;
    use std::path::PathBuf;

    /// A wire caller can supply just `name` and inherit the defaults.
    #[test]
    fn impact_args_deserialize_minimal() {
        let args: ImpactArgs = serde_json::from_str(r#"{"name":"foo"}"#).unwrap();
        assert_eq!(args.name, "foo");
        assert_eq!(args.depth, crate::cli::args::DEFAULT_DEPTH_BLAST);
        assert_eq!(args.limit, crate::cli::args::DEFAULT_LIMIT);
        assert!(!args.suggest_tests);
        assert!(!args.include_types);
    }

    /// Build the serialized impact kind-fallback object the same way the
    /// core does, for shape assertions. Mirrors the per-kind notes via
    /// `notes_text::impact` so the test pins the production path, not a
    /// hand-rolled literal.
    fn impact_fallback_value(
        name: &str,
        chunks: &[ChunkSummary],
        fk: FallbackKind,
    ) -> serde_json::Value {
        let text = notes_text::impact(fk);
        let out = KindFallbackOutput::new(name, chunks, fk, "impact", &text);
        serde_json::to_value(&out).unwrap()
    }

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
        let json = impact_fallback_value("HANDLING_ADVICE", &[chunk], FallbackKind::Const);

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
        let json = impact_fallback_value("X", &[chunk], FallbackKind::Const);
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
        let json = impact_fallback_value("VERSION", &[c1, c2], FallbackKind::Const);

        let defs = json["definitions"].as_array().unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0]["language"], "rust");
        assert_eq!(defs[1]["language"], "c");
    }

    #[test]
    fn impact_function_path_injects_function_label() {
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
        let out = ImpactCoreOutput::Function {
            result,
            suggestions: Vec::new(),
            suggest_tests: false,
        };
        let json = out.to_value().unwrap();
        assert_eq!(json["kind"], "function");
        assert!(
            json.get("test_suggestions").is_none(),
            "no test_suggestions key when suggest_tests not requested"
        );
    }

    #[test]
    fn impact_function_path_emits_empty_test_suggestions_when_requested() {
        // When --suggest-tests is set but no suggestions are found, the
        // historical wire shape carried an empty `test_suggestions` array.
        let result = cqs::ImpactResult {
            function_name: "foo".to_string(),
            callers: Vec::new(),
            transitive_callers: Vec::new(),
            tests: Vec::new(),
            type_impacted: Vec::new(),
            degraded: false,
        };
        let out = ImpactCoreOutput::Function {
            result,
            suggestions: Vec::new(),
            suggest_tests: true,
        };
        let json = out.to_value().unwrap();
        assert!(
            json["test_suggestions"].is_array(),
            "test_suggestions must be present (empty array) when requested"
        );
    }

    // Type / Module / Ambiguous cells of the impact × kind matrix. Each
    // cell pins its `kind` label, the `fallback_from: "impact"` invariant,
    // and that the per-kind redirect note differs from Const so an agent
    // reading the response shape can tell which fallback fired.

    fn make_chunk_of_type(
        chunk_type: cqs::parser::ChunkType,
        name: &str,
        file: &str,
        line: u32,
    ) -> ChunkSummary {
        ChunkSummary {
            id: format!("{}:{}:{}", file, line, "abcd1234"),
            file: PathBuf::from(file),
            language: cqs::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("test signature for {}", name),
            content: format!("test content for {}", name),
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
    fn impact_type_fallback_emits_kind_type() {
        let chunk =
            make_chunk_of_type(cqs::parser::ChunkType::Struct, "MyStruct", "src/foo.rs", 10);
        let json = impact_fallback_value("MyStruct", &[chunk], FallbackKind::Type);
        assert_eq!(json["kind"], "type");
        assert_eq!(json["fallback_from"], "impact");
        assert_eq!(json["name"], "MyStruct");
        assert_eq!(json["definitions"].as_array().unwrap().len(), 1);
        assert!(json["note"].as_str().unwrap().contains("cqs deps"));
    }

    #[test]
    fn impact_module_fallback_emits_kind_module() {
        let chunk = make_chunk_of_type(cqs::parser::ChunkType::Module, "my_mod", "src/lib.rs", 5);
        let json = impact_fallback_value("my_mod", &[chunk], FallbackKind::Module);
        assert_eq!(json["kind"], "module");
        assert_eq!(json["fallback_from"], "impact");
        assert_eq!(json["name"], "my_mod");
    }

    #[test]
    fn impact_ambiguous_fallback_returns_all_kinds() {
        // Ambiguous case: `len` resolves to a method AND a const.
        // The fallback must surface both with their respective
        // chunk_type values so the consumer can disambiguate.
        let m = make_chunk_of_type(cqs::parser::ChunkType::Method, "len", "src/a.rs", 10);
        let c = make_chunk_of_type(cqs::parser::ChunkType::Constant, "len", "src/b.rs", 5);
        let json = impact_fallback_value("len", &[m, c], FallbackKind::Ambiguous);
        assert_eq!(json["kind"], "ambiguous");
        assert_eq!(json["fallback_from"], "impact");
        let defs = json["definitions"].as_array().unwrap();
        assert_eq!(defs.len(), 2);
        // Per-definition chunk_type lets the consumer distinguish.
        let chunk_types: Vec<&str> = defs
            .iter()
            .map(|d| d["chunk_type"].as_str().unwrap())
            .collect();
        assert!(chunk_types.contains(&"method"));
        assert!(chunk_types.contains(&"constant"));
    }

    #[test]
    fn impact_kind_fallback_shape_invariants() {
        // Every fallback shape carries the same five top-level keys
        // regardless of kind. Pin so a future "drop fallback_from" or
        // "rename note → message" decision is a deliberate test-failing
        // change, not an accidental drift.
        let chunk = make_chunk_of_type(cqs::parser::ChunkType::Class, "MyClass", "src/foo.rs", 10);
        let json = impact_fallback_value("MyClass", &[chunk], FallbackKind::Type);
        let obj = json.as_object().unwrap();
        let keys: std::collections::HashSet<&str> = obj.keys().map(|k| k.as_str()).collect();
        let expected: std::collections::HashSet<&str> =
            ["kind", "fallback_from", "name", "definitions", "note"]
                .iter()
                .copied()
                .collect();
        assert_eq!(keys, expected);
    }

    // Kind-fallback paths must cap definitions count and per-chunk content
    // size to prevent pathological "hot name" responses (e.g. `cqs callers
    // Result` matching hundreds of chunks, each emitting full content) from
    // saturating the daemon socket / agent's parse buffer.

    #[test]
    fn chunks_to_definitions_caps_count_at_max() {
        // Build many chunks; expect the helper to truncate to
        // KIND_FALLBACK_MAX_DEFINITIONS.
        let chunks: Vec<ChunkSummary> = (0..(KIND_FALLBACK_MAX_DEFINITIONS + 50))
            .map(|i| make_const_chunk(&format!("X{i}"), "src/lib.rs", i as u32))
            .collect();
        let defs = chunks_to_definitions(&chunks);
        assert_eq!(
            defs.len(),
            KIND_FALLBACK_MAX_DEFINITIONS,
            "definitions count must cap at KIND_FALLBACK_MAX_DEFINITIONS"
        );
    }

    #[test]
    fn chunks_to_definitions_truncates_oversized_content() {
        // Single chunk with content much larger than the per-entry cap.
        let mut big = make_const_chunk("BIG", "src/lib.rs", 1);
        big.content = "x".repeat(KIND_FALLBACK_MAX_CONTENT_BYTES * 2);
        let defs = chunks_to_definitions(&[big]);

        let entry = &defs[0];
        let content = entry["content"].as_str().unwrap();
        assert!(
            content.len() <= KIND_FALLBACK_MAX_CONTENT_BYTES + 32,
            "truncated content must be ≤ cap + suffix; got {}",
            content.len()
        );
        assert!(
            content.ends_with("... (truncated)"),
            "truncated entry must carry the truncation suffix; got: {}",
            &content[content.len().saturating_sub(40)..]
        );
        assert_eq!(
            entry["truncated"], true,
            "truncated entries must carry truncated=true field"
        );
    }

    #[test]
    fn chunks_to_definitions_passes_small_content_unchanged() {
        // Small content should NOT be truncated and should NOT carry
        // a truncated:true field (skip-when-default).
        let chunk = make_const_chunk("X", "src/lib.rs", 1);
        let defs = chunks_to_definitions(&[chunk]);

        let entry = &defs[0];
        assert!(
            entry.get("truncated").is_none(),
            "non-truncated entries must omit truncated field; got: {:?}",
            entry
        );
        assert!(
            !entry["content"]
                .as_str()
                .unwrap()
                .ends_with("... (truncated)"),
            "non-truncated content must not have suffix"
        );
    }

    #[test]
    fn chunks_to_definitions_truncation_respects_utf8_boundary() {
        // Pathological: content with a multi-byte UTF-8 char at the
        // truncation boundary. Naive byte-slicing would split the
        // codepoint and produce invalid UTF-8 (panic in `&str` slice).
        let mut chunk = make_const_chunk("X", "src/lib.rs", 1);
        // Build content that reaches the cap with `é` (2 bytes) at the
        // boundary so a naive cut would split the codepoint.
        let pad_len = KIND_FALLBACK_MAX_CONTENT_BYTES - 1;
        chunk.content = format!("{}é{}", "x".repeat(pad_len), "y".repeat(100));

        // Should not panic and should produce valid UTF-8 output.
        let defs = chunks_to_definitions(&[chunk]);
        let content = defs[0]["content"].as_str().unwrap();
        assert!(
            content.ends_with("... (truncated)"),
            "must still emit truncation suffix on UTF-8 boundary case"
        );
        // Implicit: as_str() succeeded → valid UTF-8.
    }
}
