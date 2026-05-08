//! Call graph commands for cqs
//!
//! Provides callers/callees analysis.
//!
//! ## Polymorphic routing (Phase 1)
//!
//! `cqs callers <name>` and `cqs callees <name>` historically required a
//! function-or-method name and returned an empty `Vec` for any other
//! chunk kind — the misrouted-to-empty failure mode the polymorphic-
//! routing design (`docs/polymorphic-routing.md`) targets. Both commands
//! now consult `cqs::kind::classify_hits` against an exact-name lookup
//! before the call-graph query: kind-mismatch fallbacks return an object
//! with `kind`, `fallback_from`, `name`, `definitions`, and `note`
//! fields. The function-path success shape (a flat array of caller/callee
//! entries) is unchanged so existing consumers see no change on the
//! happy path; agents detect the dispatch decision by type
//! (`isinstance(parsed, list)` ⇒ function path, `dict` ⇒ fallback).

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::kind::{classify_hits, Kind, KindHit};
use cqs::normalize_path;
use cqs::store::{CallerInfo, ChunkSummary};

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct CallerEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32, // was "line"
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleeEntry {
    pub name: String,
    pub line_start: u32, // was "line"
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleesOutput {
    pub name: String, // was "function"
    pub calls: Vec<CalleeEntry>,
    pub count: usize,
}

// ─── Shared JSON builders ──────────────────────────────────────────────────

/// Build typed caller output from caller info -- shared between CLI and batch.
pub(crate) fn build_callers(callers: &[CallerInfo]) -> Vec<CallerEntry> {
    let _span = tracing::info_span!("build_callers", count = callers.len()).entered();
    callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: normalize_path(&c.file).to_string(),
            line_start: c.line,
        })
        .collect()
}

/// Build typed callees output -- shared between CLI and batch.
pub(crate) fn build_callees(name: &str, callees: &[(String, u32)]) -> CalleesOutput {
    let _span = tracing::info_span!("build_callees", name, count = callees.len()).entered();
    CalleesOutput {
        name: name.to_string(),
        calls: callees
            .iter()
            .map(|(n, line)| CalleeEntry {
                name: n.clone(),
                line_start: *line,
            })
            .collect(),
        count: callees.len(),
    }
}

// ─── Polymorphic-routing fallbacks ─────────────────────────────────────────

/// Build a definitions list from chunks for kind-mismatch fallbacks.
/// Mirrors `cmd_impact`'s `chunks_to_definitions` (per-command duplication
/// is intentional for now — if the pattern stays stable across all 6
/// commands, lift to a shared module in a follow-up).
fn chunks_to_definitions(chunks: &[ChunkSummary]) -> Vec<serde_json::Value> {
    chunks
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
        .collect()
}

/// Generic kind-mismatch fallback dispatcher for `cqs callers <name>`.
/// Same shape as the impact-side fallbacks. JSON path emits an object
/// with `{kind, fallback_from, name, definitions, note}`; text path
/// prints the lead, definitions, and redirect note.
fn callers_kind_fallback(
    name: &str,
    chunks: &[ChunkSummary],
    json: bool,
    kind_label: &str,
    note: &str,
    text_lead: &str,
    text_redirect: &str,
) -> Result<()> {
    debug_assert!(
        !chunks.is_empty(),
        "Kind fallback called with no hits — caller must classify before dispatching"
    );
    if json {
        let payload = serde_json::json!({
            "kind": kind_label,
            "fallback_from": "callers",
            "name": name,
            "definitions": chunks_to_definitions(chunks),
            "note": note,
        });
        crate::cli::json_envelope::emit_json(&payload)?;
    } else {
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
    Ok(())
}

// ─── CLI commands ──────────────────────────────────────────────────────────

/// Find functions that call the specified function
pub(crate) fn cmd_callers(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    limit: usize,
    cross_project: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_callers", name, limit, cross_project).entered();
    let store = &ctx.store;
    // Task A3: standardised cap. The store query returns every caller; we
    // truncate before rendering so the user can paginate via repeated calls
    // (no offset surfaced yet — by design, agents pass `--limit N` once and
    // ask for more by name if needed).
    let limit = limit.clamp(1, 100);

    // Polymorphic-routing kind detection (Phase 1). Dispatch kind-
    // mismatch fallbacks before the call-graph query, except on the
    // cross-project path which has its own (cross-project)
    // resolution semantics.
    if !cross_project {
        let chunks = store.lookup_by_name(name)?;
        let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
        let kind = classify_hits(&hits);
        match kind {
            Kind::Const => {
                return callers_kind_fallback(
                    name, &chunks, json, "const",
                    "consts don't have callers; here are the definition sites. \
                     Use `cqs <name>` or `cqs search <name>` to find references.",
                    &format!("(callers) `{name}` is a const, not a function — call-graph callers analysis doesn't apply."),
                    "Use `cqs <name>` or `cqs search <name>` to find references.",
                );
            }
            Kind::Type => {
                return callers_kind_fallback(
                    name, &chunks, json, "type",
                    "types don't have callers in the call-graph sense; here are the definition sites. \
                     Use `cqs deps <name>` for type-dependency callers or `cqs <name>` to find usage references.",
                    &format!("(callers) `{name}` is a type, not a function — call-graph callers analysis doesn't apply."),
                    "Use `cqs deps <name>` for type-dependency analysis or `cqs <name>` to find usage references.",
                );
            }
            Kind::Module => {
                return callers_kind_fallback(
                    name, &chunks, json, "module",
                    "modules don't have callers in the call-graph sense; here are the declaration sites. \
                     Use `cqs <name>` to find files that reference this module.",
                    &format!("(callers) `{name}` is a module/namespace, not a function — call-graph callers analysis doesn't apply."),
                    "Use `cqs <name>` to find files that reference this module.",
                );
            }
            Kind::Ambiguous => {
                return callers_kind_fallback(
                    name, &chunks, json, "ambiguous",
                    "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
                     Re-run `cqs callers <name>` against a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate.",
                    &format!("(callers) `{name}` is ambiguous — matches multiple chunk kinds."),
                    "Re-run with a more specific name (e.g. `Type::method`) or use `cqs <name>` to disambiguate.",
                );
            }
            // Function | Multiple | Other | NotFound: fall through to
            // the existing call-graph query.
            _ => {}
        }
    }

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let mut callers = cross_ctx
            .get_callers_cross(name)
            .context("Failed to load cross-project callers")?;
        callers.truncate(limit);

        if callers.is_empty() {
            if json {
                crate::cli::json_envelope::emit_json(&serde_json::json!([]))?;
            } else {
                println!("No callers found for '{}' (cross-project)", name);
            }
            return Ok(());
        }

        if json {
            crate::cli::json_envelope::emit_json(&callers)?;
        } else {
            println!("Functions that call '{}' (cross-project):", name);
            println!();
            for c in &callers {
                println!(
                    "  {} ({}:{}) [{}]",
                    c.caller.name.cyan(),
                    c.caller.file.display(),
                    c.caller.line,
                    c.project.dimmed()
                );
            }
            println!();
            println!("Total: {} caller(s)", callers.len());
        }
        return Ok(());
    }

    // Standard single-project path
    let mut callers = store
        .get_callers_full(name)
        .context("Failed to load callers")?;
    callers.truncate(limit);

    if callers.is_empty() {
        if json {
            crate::cli::json_envelope::emit_json(&serde_json::json!([]))?;
        } else {
            println!("No callers found for '{}'", name);
        }
        return Ok(());
    }

    if json {
        let output = build_callers(&callers);
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        println!("Functions that call '{}':", name);
        println!();
        for caller in &callers {
            println!(
                "  {} ({}:{})",
                caller.name.cyan(),
                caller.file.display(),
                caller.line
            );
        }
        println!();
        println!("Total: {} caller(s)", callers.len());
    }

    Ok(())
}

/// Generic kind-mismatch fallback dispatcher for `cqs callees <name>`.
fn callees_kind_fallback(
    name: &str,
    chunks: &[ChunkSummary],
    json: bool,
    kind_label: &str,
    note: &str,
    text_lead: &str,
    text_redirect: &str,
) -> Result<()> {
    debug_assert!(
        !chunks.is_empty(),
        "Kind fallback called with no hits — caller must classify before dispatching"
    );
    if json {
        let payload = serde_json::json!({
            "kind": kind_label,
            "fallback_from": "callees",
            "name": name,
            "definitions": chunks_to_definitions(chunks),
            "note": note,
        });
        crate::cli::json_envelope::emit_json(&payload)?;
    } else {
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
    Ok(())
}

/// Find functions called by the specified function
pub(crate) fn cmd_callees(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    limit: usize,
    cross_project: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_callees", name, limit, cross_project).entered();
    let store = &ctx.store;
    // Task A3: see cmd_callers — same clamp range.
    let limit = limit.clamp(1, 100);

    // Polymorphic-routing kind detection (Phase 1). Same dispatch
    // pattern as cmd_callers above.
    if !cross_project {
        let chunks = store.lookup_by_name(name)?;
        let hits: Vec<KindHit> = chunks.iter().map(KindHit::from).collect();
        let kind = classify_hits(&hits);
        match kind {
            Kind::Const => {
                return callees_kind_fallback(
                    name, &chunks, json, "const",
                    "consts don't have callees; the const's value is its content. \
                     Use `cqs explain <name>` or `cqs read --focus <name>` to inspect.",
                    &format!("(callees) `{name}` is a const, not a function — call-graph callees analysis doesn't apply."),
                    "Use `cqs explain <name>` or `cqs read --focus <name>` to inspect the value.",
                );
            }
            Kind::Type => {
                return callees_kind_fallback(
                    name, &chunks, json, "type",
                    "types don't have callees; here are the definition sites. \
                     Use `cqs deps <name>` for the type's type dependencies or `cqs callees <Type::method>` for a specific method's callees.",
                    &format!("(callees) `{name}` is a type, not a function — call-graph callees analysis doesn't apply."),
                    "Use `cqs deps <name>` for type-dependency analysis or call against a specific method (`Type::method`).",
                );
            }
            Kind::Module => {
                return callees_kind_fallback(
                    name, &chunks, json, "module",
                    "modules don't have callees; here are the declaration sites. \
                     Use `cqs callees <function-in-module>` for a specific function's callees.",
                    &format!("(callees) `{name}` is a module/namespace, not a function — call-graph callees analysis doesn't apply."),
                    "Use `cqs callees <function-in-module>` for a specific function's callees.",
                );
            }
            Kind::Ambiguous => {
                return callees_kind_fallback(
                    name, &chunks, json, "ambiguous",
                    "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
                     Re-run `cqs callees <name>` against a more specific name (e.g. `Type::method`).",
                    &format!("(callees) `{name}` is ambiguous — matches multiple chunk kinds."),
                    "Re-run with a more specific name (e.g. `Type::method`).",
                );
            }
            _ => {}
        }
    }

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let mut callees = cross_ctx
            .get_callees_cross(name)
            .context("Failed to load cross-project callees")?;
        callees.truncate(limit);

        if json {
            crate::cli::json_envelope::emit_json(&callees)?;
        } else {
            println!("Functions called by '{}' (cross-project):", name.cyan());
            println!();
            if callees.is_empty() {
                println!("  (no function calls found)");
            } else {
                for c in &callees {
                    println!("  {} [{}]", c.name, c.project.dimmed());
                }
            }
            println!();
            println!("Total: {} call(s)", callees.len());
        }
        return Ok(());
    }

    // Standard single-project path
    let mut callees = store
        .get_callees_full(name, None)
        .context("Failed to load callees")?;
    callees.truncate(limit);

    if json {
        let output = build_callees(name, &callees);
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        println!("Functions called by '{}':", name.cyan());
        println!();
        if callees.is_empty() {
            println!("  (no function calls found)");
        } else {
            for (callee_name, _line) in &callees {
                println!("  {}", callee_name);
            }
        }
        println!();
        println!("Total: {} call(s)", callees.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_caller_entry_field_names() {
        let entry = CallerEntry {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none()); // normalized away
    }

    #[test]
    fn test_build_callers_empty() {
        let output = build_callers(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn test_build_callees_empty() {
        let output = build_callees("foo", &[]);
        assert_eq!(output.count, 0);
        assert!(output.calls.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "foo");
    }

    #[test]
    fn test_callees_output_field_names() {
        let output = build_callees("bar", &[("baz".into(), 10)]);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "bar"); // was "function"
        assert!(json.get("function").is_none());
        assert_eq!(json["calls"][0]["line_start"], 10);
    }

    // Polymorphic-routing Phase 1: callers + callees kind-mismatch fallback
    // shape. Each test pins the JSON-builder contract so future schema
    // tweaks are deliberate, not accidental.

    fn make_chunk(
        chunk_type: cqs::parser::ChunkType,
        name: &str,
        file: &str,
        line: u32,
    ) -> ChunkSummary {
        ChunkSummary {
            id: format!("{}:{}:{}", file, line, "abcd1234"),
            file: std::path::PathBuf::from(file),
            language: cqs::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("test sig for {}", name),
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
    fn test_chunks_to_definitions_shape() {
        let chunk = make_chunk(cqs::parser::ChunkType::Constant, "X", "src/a.rs", 5);
        let defs = chunks_to_definitions(&[chunk]);
        assert_eq!(defs.len(), 1);
        let d = &defs[0];
        // The 7 fields every kind-mismatch fallback emits.
        for key in &[
            "file",
            "line_start",
            "line_end",
            "language",
            "chunk_type",
            "signature",
            "content",
        ] {
            assert!(d.get(*key).is_some(), "missing field: {key}");
        }
        assert_eq!(d["chunk_type"], "constant");
        assert_eq!(d["language"], "rust");
    }

    /// Pin the `{kind, fallback_from, name, definitions, note}` shape
    /// for the callers fallback. The test mirrors the impact module's
    /// `build_impact_kind_fallback_json_shape_invariants` — same shape,
    /// different `fallback_from` value.
    #[test]
    fn test_callers_fallback_payload_shape() {
        // Build the payload by mimicking what `callers_kind_fallback`
        // assembles before calling `emit_json`. Direct unit test of the
        // dispatcher would need stdout capture; this asserts the shape
        // contract instead.
        let chunk = make_chunk(cqs::parser::ChunkType::Constant, "X", "src/a.rs", 5);
        let payload = serde_json::json!({
            "kind": "const",
            "fallback_from": "callers",
            "name": "X",
            "definitions": chunks_to_definitions(&[chunk]),
            "note": "test note",
        });
        assert_eq!(payload["kind"], "const");
        assert_eq!(payload["fallback_from"], "callers");
        assert_eq!(payload["name"], "X");
        assert_eq!(payload["definitions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_callees_fallback_payload_shape() {
        // Mirror of the callers payload, with `fallback_from: "callees"`.
        let chunk = make_chunk(cqs::parser::ChunkType::Class, "MyClass", "src/a.rs", 5);
        let payload = serde_json::json!({
            "kind": "type",
            "fallback_from": "callees",
            "name": "MyClass",
            "definitions": chunks_to_definitions(&[chunk]),
            "note": "test note",
        });
        assert_eq!(payload["fallback_from"], "callees");
        assert_eq!(payload["definitions"][0]["chunk_type"], "class");
    }
}
