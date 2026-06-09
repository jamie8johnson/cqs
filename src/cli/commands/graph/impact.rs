//! Impact command — what breaks if you change a function
//!
//! ## Polymorphic routing
//!
//! `cqs impact <name>` consults `cqs::kind::classify_hits` against an
//! exact-name lookup before running the call-graph analysis. For
//! [`Kind::Const`], [`Kind::Type`], [`Kind::Module`], and
//! [`Kind::Ambiguous`] the response is a kind-labeled definition list with
//! a redirect note instead of empty. Function and other kinds fall through
//! to the call-graph analysis flow.

use anyhow::Result;

use cqs::kind::{classify_hits, Kind, KindHit};
use cqs::{
    analyze_impact, format_test_suggestions, impact_to_json, impact_to_mermaid, suggest_tests,
    ImpactOptions,
};

use crate::cli::commands::resolve::resolve_target;
use crate::cli::OutputFormat;

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
    let depth = depth.clamp(1, 10);
    // Per-section truncation cap. Default 5 from `LimitArg`. The analyzer
    // returns the full result; we apply the cap at render time so the
    // underlying graph data is unaffected (other consumers — mermaid,
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

        // Exhaustive match — adding a new `OutputFormat` variant fails to
        // compile until every render site adds an arm.
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

    // Polymorphic-routing kind detection. Read once up-front so the
    // dispatcher branch happens before the resolve-+-analyze flow that
    // assumes a function. Hits double-duty as the input to the kind-
    // mismatch fallbacks below — one SQL query covers all.
    //
    // Per-kind routing:
    // - Function: call-graph impact analysis (with `kind: "function"` label)
    // - Const: kind-labeled definition list + redirect note
    // - Type: kind-labeled definition list + redirect to `cqs deps <type>`
    // - Module: kind-labeled definition list + redirect to file-import search
    // - Ambiguous: aggregate across kinds with `kind` per result
    // - Multiple, NotFound, Other: fall through to the call-graph flow
    //   (which may resolve via FTS or return "not found")
    let chunks = store.lookup_by_name(name)?;
    let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
    let kind = classify_hits(&hits);
    match kind {
        Kind::Const => return cmd_impact_const_fallback(name, &chunks, format),
        Kind::Type => return cmd_impact_type_fallback(name, &chunks, format),
        Kind::Module => return cmd_impact_module_fallback(name, &chunks, format),
        Kind::Ambiguous => return cmd_impact_ambiguous_fallback(name, &chunks, format),
        // Function | Multiple | Other | NotFound: fall through to the
        // resolve_target + analyze_impact flow. Multiple is safe
        // because resolve_target picks the first / best match;
        // NotFound surfaces a "not found" error from resolve_target.
        _ => {}
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
/// path or a kind-mismatch fallback.
fn impact_to_json_with_kind(result: &cqs::ImpactResult, kind: &str) -> Result<serde_json::Value> {
    let mut json = impact_to_json(result)?;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("kind".into(), serde_json::json!(kind));
    }
    Ok(json)
}

/// Build a definitions list from chunks. Used by every kind-mismatch
/// fallback below. Each entry carries file/line/language/chunk_type/
/// signature/content — the same shape every kind emits.
///
/// Caps at [`KIND_FALLBACK_MAX_DEFINITIONS`] entries and truncates per-chunk
/// content at [`KIND_FALLBACK_MAX_CONTENT_BYTES`]. Hot names like `Result` /
/// `Error` match hundreds of chunks across a corpus; without the cap,
/// `cqs callers Result --json` could return multi-MB JSON (and on the daemon
/// path, write a multi-MB JSONL line that pegs the receiver's parse buffer).
/// The cap mirrors the `clamp(1, 100)` discipline the rest of the graph
/// commands use; the per-entry truncation keeps pathological monster-chunk
/// content from ballooning the response.
fn chunks_to_definitions(chunks: &[cqs::store::ChunkSummary]) -> Vec<serde_json::Value> {
    chunks
        .iter()
        .take(KIND_FALLBACK_MAX_DEFINITIONS)
        .map(chunk_to_definition_value)
        .collect()
}

/// Maximum number of `definitions[]` entries returned in a kind-mismatch
/// fallback response. Mirrors the standard graph-command result cap.
pub(crate) const KIND_FALLBACK_MAX_DEFINITIONS: usize = 100;

/// Per-entry `content` byte cap inside a kind-mismatch fallback
/// `definitions[]` entry. Truncated content is suffixed with
/// `"... (truncated)"` and the entry gains a `truncated: true` field
/// so consumers can distinguish capped chunks from full ones.
pub(crate) const KIND_FALLBACK_MAX_CONTENT_BYTES: usize = 2048;

/// Shared chunk-to-definition transformation for both CLI-direct and
/// daemon-path kind fallbacks. Truncates content per
/// [`KIND_FALLBACK_MAX_CONTENT_BYTES`].
pub(crate) fn chunk_to_definition_value(c: &cqs::store::ChunkSummary) -> serde_json::Value {
    let (content, truncated) = if c.content.len() > KIND_FALLBACK_MAX_CONTENT_BYTES {
        // Truncate at a UTF-8 char boundary at or below the byte cap.
        // `floor_char_boundary` would be cleaner but isn't stable yet.
        let mut end = KIND_FALLBACK_MAX_CONTENT_BYTES;
        while !c.content.is_char_boundary(end) {
            end -= 1;
        }
        (format!("{}... (truncated)", &c.content[..end]), true)
    } else {
        (c.content.clone(), false)
    };
    let mut entry = serde_json::Map::new();
    entry.insert(
        "file".to_string(),
        serde_json::json!(cqs::normalize_path(&c.file)),
    );
    entry.insert("line_start".to_string(), serde_json::json!(c.line_start));
    entry.insert("line_end".to_string(), serde_json::json!(c.line_end));
    entry.insert(
        "language".to_string(),
        serde_json::json!(c.language.to_string()),
    );
    entry.insert(
        "chunk_type".to_string(),
        serde_json::json!(c.chunk_type.to_string()),
    );
    entry.insert("signature".to_string(), serde_json::json!(c.signature));
    entry.insert("content".to_string(), serde_json::json!(content));
    if truncated {
        entry.insert("truncated".to_string(), serde_json::json!(true));
    }
    serde_json::Value::Object(entry)
}

/// Generic kind-mismatch fallback JSON builder. Every non-Function kind
/// that lands on `cqs impact <name>` gets a response of this shape:
///
/// ```json
/// {
///   "kind": "<const|type|module|ambiguous>",
///   "fallback_from": "impact",
///   "name": "<queried>",
///   "definitions": [{...}, ...],
///   "note": "<kind-specific redirect>"
/// }
/// ```
///
/// `kind_label` and `note` parameterize the per-kind message; the
/// `definitions` array is the same shape every cell emits so consumer
/// agents only learn one schema.
fn build_impact_kind_fallback_json(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
    kind_label: &str,
    note: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": kind_label,
        "fallback_from": "impact",
        "name": name,
        "definitions": chunks_to_definitions(chunks),
        "note": note,
    })
}

/// Const-specific JSON builder. Thin wrapper around
/// [`build_impact_kind_fallback_json`]; the canonical const constructor
/// for the test suite and the path the [`cmd_impact_const_fallback`]
/// dispatcher calls, so a regression to the generic helper surfaces here
/// where the per-kind tests live.
fn build_impact_const_fallback_json(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
) -> serde_json::Value {
    build_impact_kind_fallback_json(
        name,
        chunks,
        "const",
        "consts don't have call-graph impact; here are the definition sites. \
         Use `cqs <name>` or `cqs search <name>` to find references.",
    )
}

/// Generic kind-mismatch fallback dispatcher. Emits the JSON shape via
/// `emit_json` (which honors the active `OutputFormat::current()`);
/// Text/Mermaid surfaces print a plain-text equivalent with the same
/// kind-specific redirect.
///
/// `text_lead` is the first line printed for Text/Mermaid (e.g.
/// `"(impact) `{name}` is a const, not a function — ..."`).
/// `text_redirect` is the trailing redirect (e.g.
/// `"Use `cqs <name>`..."`).
fn cmd_impact_kind_fallback(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
    format: &OutputFormat,
    kind_label: &str,
    note: &str,
    text_lead: &str,
    text_redirect: &str,
) -> Result<()> {
    debug_assert!(
        !chunks.is_empty(),
        "Kind fallback called with no hits — caller must classify before dispatching"
    );
    match format {
        OutputFormat::Json => {
            let json = build_impact_kind_fallback_json(name, chunks, kind_label, note);
            crate::cli::json_envelope::emit_json(&json)?;
        }
        OutputFormat::Text | OutputFormat::Mermaid => {
            println!("{text_lead}");
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
            println!("{text_redirect}");
        }
    }
    Ok(())
}

/// Kind-mismatch fallback: `cqs impact <const>` returns a kind-labeled
/// definition list + redirect note instead of empty. Consts aren't
/// tracked by the call-graph layer, so call-graph impact analysis would
/// otherwise return zero callers and leave the agent with nothing
/// actionable.
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
                "(impact) `{name}` is a const, not a function — call-graph impact analysis doesn't apply."
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

/// Kind-mismatch fallback: `cqs impact <type>` (struct, enum,
/// trait, class, interface, type alias, ...). Returns the type's
/// definition site(s) + a redirect to `cqs deps <type>` for type-
/// dependency analysis (the closest analogue to "what breaks if you
/// change this" for a type).
fn cmd_impact_type_fallback(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
    format: &OutputFormat,
) -> Result<()> {
    cmd_impact_kind_fallback(
        name, chunks, format, "type",
        "types don't have call-graph impact; here are the definition sites. \
         Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.",
        &format!("(impact) `{name}` is a type, not a function — call-graph impact analysis doesn't apply."),
        "Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.",
    )
}

/// Kind-mismatch fallback: `cqs impact <module>` (Rust mod, C++
/// namespace, package, ...). Returns the module's declaration site(s)
/// + a redirect to find files importing the module.
fn cmd_impact_module_fallback(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
    format: &OutputFormat,
) -> Result<()> {
    cmd_impact_kind_fallback(
        name, chunks, format, "module",
        "modules don't have call-graph impact; here are the declaration sites. \
         Use `cqs <name>` to find files that reference this module.",
        &format!("(impact) `{name}` is a module/namespace, not a function — call-graph impact analysis doesn't apply."),
        "Use `cqs <name>` to find files that reference this module.",
    )
}

/// Kind-mismatch fallback: `cqs impact <ambiguous-name>` (name
/// resolves across multiple kinds — e.g. `len` is both a method and
/// a const in some codebases). Returns all matched chunks. The
/// per-definition `chunk_type` field tells the consumer which entry
/// is the function/method vs. the type vs. the const.
fn cmd_impact_ambiguous_fallback(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
    format: &OutputFormat,
) -> Result<()> {
    cmd_impact_kind_fallback(
        name, chunks, format, "ambiguous",
        "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
         Re-run `cqs impact <name>` against a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate by content.",
        &format!("(impact) `{name}` is ambiguous — matches multiple chunk kinds."),
        "Re-run with a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate.",
    )
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
        let json =
            build_impact_kind_fallback_json("MyStruct", &[chunk], "type", "test note for type");
        assert_eq!(json["kind"], "type");
        assert_eq!(json["fallback_from"], "impact");
        assert_eq!(json["name"], "MyStruct");
        assert_eq!(json["definitions"].as_array().unwrap().len(), 1);
        assert_eq!(json["note"], "test note for type");
    }

    #[test]
    fn impact_module_fallback_emits_kind_module() {
        let chunk = make_chunk_of_type(cqs::parser::ChunkType::Module, "my_mod", "src/lib.rs", 5);
        let json =
            build_impact_kind_fallback_json("my_mod", &[chunk], "module", "test note for module");
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
        let json =
            build_impact_kind_fallback_json("len", &[m, c], "ambiguous", "test ambiguous note");
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
    fn build_impact_kind_fallback_json_shape_invariants() {
        // Every fallback shape carries the same five top-level keys
        // regardless of kind. Pin so a future "drop fallback_from" or
        // "rename note → message" decision is a deliberate test-failing
        // change, not an accidental drift.
        let chunk = make_chunk_of_type(cqs::parser::ChunkType::Class, "MyClass", "src/foo.rs", 10);
        let json = build_impact_kind_fallback_json("MyClass", &[chunk], "type", "n/a");
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
