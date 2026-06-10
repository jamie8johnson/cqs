//! Type dependency command for cqs
//!
//! Shows which chunks reference a type (forward), or what types a function uses (reverse).
//! Core JSON builders are shared between CLI and batch handlers.

use std::path::Path;

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::store::{ChunkSummary, TypeUsage};

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct TypeUsageEntry {
    pub type_name: String,
    pub edge_kind: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DepsReverseOutput {
    pub name: String,
    pub types: Vec<TypeUsageEntry>,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DepsUserEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub chunk_type: String,
}

// ─── Shared JSON builders ──────────────────────────────────────────────────

/// Build typed reverse deps output (types used by a function) -- shared between CLI and batch.
pub(crate) fn build_deps_reverse(name: &str, types: &[TypeUsage]) -> DepsReverseOutput {
    let _span = tracing::info_span!("build_deps_reverse", name).entered();
    DepsReverseOutput {
        name: name.to_string(),
        types: types
            .iter()
            .map(|t| TypeUsageEntry {
                type_name: t.type_name.clone(),
                edge_kind: t.edge_kind.clone(),
            })
            .collect(),
        count: types.len(),
    }
}

/// Build typed forward deps output (chunks that use a type) -- shared between CLI and batch.
pub(crate) fn build_deps_forward(users: &[ChunkSummary], root: &Path) -> Vec<DepsUserEntry> {
    let _span = tracing::info_span!("build_deps_forward", count = users.len()).entered();
    users
        .iter()
        .map(|c| DepsUserEntry {
            name: c.name.clone(),
            file: cqs::rel_display(&c.file, root).to_string(),
            line_start: c.line_start,
            chunk_type: c.chunk_type.to_string(),
        })
        .collect()
}

// ─── Polymorphic-routing fallback ──────────────────────────────────────────

/// Polymorphic-routing kind-mismatch fallback for `cqs deps <name>`.
/// Same shape pattern as the impact / callers / callees / test-map / trace
/// fallbacks. Deps is dual-mode (forward = "type users", reverse =
/// "function's used types"), so Function and Type both have valid
/// semantics in their respective modes — the fallback fires only for
/// kinds that don't fit deps' model at all (Const, Module, Ambiguous).
fn deps_kind_fallback(
    name: &str,
    chunks: &[cqs::store::ChunkSummary],
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
        let definitions = super::chunks_to_definitions(chunks);
        let payload = serde_json::json!({
            "kind": kind_label,
            "fallback_from": "deps",
            "name": name,
            "definitions": definitions,
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

// ─── CLI command ────────────────────────────────────────────────────────────

/// Show type dependencies.
///
/// Forward (default): `cqs deps Config` -- who uses this type?
/// Reverse: `cqs deps --reverse func_name` -- what types does this function use?
///
/// **Polymorphic routing:** detects the name's kind up-front.
/// `Function` (with `--reverse`) and `Type` (default forward) both have
/// valid deps semantics and run the normal flow. `Const`, `Module`, and
/// `Ambiguous` get a kind-labeled fallback because deps' "uses-of-type" /
/// "uses-of-function" model doesn't fit those.
pub(crate) fn cmd_deps(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    name: &str,
    reverse: bool,
    limit: usize,
    cross_project: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_deps", name, reverse, limit, cross_project).entered();
    if cross_project {
        tracing::warn!("cross-project deps not yet supported, returning local result");
    }
    let store = &ctx.store;
    let root = &ctx.root;
    // Cap on user list (forward) or used-types list (reverse).
    let limit = limit.clamp(1, 100);

    // Polymorphic-routing kind detection. Const/Module/Ambiguous don't fit
    // deps' model — fall back with a redirect note. Function and Type continue
    // to the dual-mode flow below.
    let chunks = store.lookup_by_name(name)?;
    let hits: Vec<cqs::kind::KindHit> = chunks.iter().map(cqs::kind::KindHit::from).collect();
    let kind = cqs::kind::classify_hits(&hits);
    match kind {
        cqs::kind::Kind::Const => {
            return deps_kind_fallback(
                name, &chunks, json, "const",
                "consts don't have type dependencies in either direction; here are the definition sites. \
                 Use `cqs <name>` to find references to this const.",
                &format!("(deps) `{name}` is a const, not a function or type — type-dependency analysis doesn't apply."),
                "Use `cqs <name>` to find references to this const.",
            );
        }
        cqs::kind::Kind::Module => {
            return deps_kind_fallback(
                name, &chunks, json, "module",
                "modules don't have type dependencies in this view; here are the declaration sites. \
                 Use `cqs deps <type-or-function-in-module>` for an item-level analysis.",
                &format!("(deps) `{name}` is a module/namespace, not a function or type — type-dependency analysis doesn't apply at this granularity."),
                "Use `cqs deps <type-or-function-in-module>` for an item-level analysis.",
            );
        }
        cqs::kind::Kind::Ambiguous => {
            return deps_kind_fallback(
                name, &chunks, json, "ambiguous",
                "name resolves across multiple kinds (function/type/const/etc.); here are all matches. \
                 Re-run `cqs deps <name>` against a more specific name (e.g. `Type::method`).",
                &format!("(deps) `{name}` is ambiguous — matches multiple chunk kinds."),
                "Re-run with a more specific name (e.g. `Type::method`).",
            );
        }
        // Function | Type | Multiple | Other | NotFound: continue to
        // existing flow. Function with --reverse and Type forward both
        // have valid semantics; Multiple typically resolves via the
        // store query's deterministic ordering; NotFound surfaces an
        // empty result naturally.
        _ => {}
    }

    if reverse {
        // Limit at SQL time so we don't fetch every edge of a popular
        // function just to drop the tail.
        let types = store
            .get_types_used_by(name, limit)
            .context("Failed to load type dependencies")?;
        if json {
            let output = build_deps_reverse(name, &types);
            crate::cli::json_envelope::emit_json(&output)?;
        } else if types.is_empty() {
            println!("No type dependencies found for '{}'", name);
        } else {
            println!("Types used by '{}':", name.cyan());
            println!();
            for t in &types {
                if t.edge_kind.is_empty() {
                    println!("  {}", t.type_name);
                } else {
                    println!("  {} ({})", t.type_name, t.edge_kind.dimmed());
                }
            }
            println!();
            println!("Total: {} type(s)", types.len());
        }
    } else {
        // Limit at SQL time. Same shape as the reverse branch above.
        let users = store
            .get_type_users(name, limit)
            .context("Failed to load type users")?;
        if json {
            let output = build_deps_forward(&users, root);
            crate::cli::json_envelope::emit_json(&output)?;
        } else if users.is_empty() {
            println!("No users found for type '{}'", name);
        } else {
            println!("Chunks that use type '{}':", name.cyan());
            println!();
            for user in &users {
                println!(
                    "  {} ({}:{})",
                    user.name.cyan(),
                    cqs::rel_display(&user.file, root),
                    user.line_start
                );
            }
            println!();
            println!("Total: {} user(s)", users.len());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deps_reverse_field_names() {
        let output = DepsReverseOutput {
            name: "my_func".into(),
            types: vec![TypeUsageEntry {
                type_name: "Config".into(),
                edge_kind: "Param".into(),
            }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "my_func");
        assert!(json.get("function").is_none());
    }

    #[test]
    fn test_deps_reverse_empty() {
        let output = DepsReverseOutput {
            name: "foo".into(),
            types: vec![],
            count: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 0);
        assert!(json["types"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_deps_forward_empty() {
        let output = build_deps_forward(&[], std::path::Path::new("/"));
        assert!(output.is_empty());
    }

    #[test]
    fn test_deps_user_entry_field_names() {
        let entry = DepsUserEntry {
            name: "bar".into(),
            file: "src/foo.rs".into(),
            line_start: 15,
            chunk_type: "function".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());
    }

    fn make_const_chunk(name: &str, line: u32) -> ChunkSummary {
        ChunkSummary {
            id: format!("src/lib.rs:{line}:abcd1234"),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: cqs::parser::Language::Rust,
            chunk_type: cqs::parser::ChunkType::Constant,
            name: name.to_string(),
            signature: format!("pub const {name}: &str = \"...\";"),
            content: format!("pub const {name}: &str = \"...\";"),
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

    // The deps kind fallback routes through the shared
    // `chunks_to_definitions`, capping entry count and truncating oversized
    // content so a hot name can't emit unbounded JSON.
    #[test]
    fn test_deps_fallback_caps_definitions_count() {
        use super::super::{chunks_to_definitions, KIND_FALLBACK_MAX_DEFINITIONS};
        let chunks: Vec<ChunkSummary> = (0..(KIND_FALLBACK_MAX_DEFINITIONS + 50))
            .map(|i| make_const_chunk(&format!("X{i}"), i as u32))
            .collect();
        let defs = chunks_to_definitions(&chunks);
        assert_eq!(defs.len(), KIND_FALLBACK_MAX_DEFINITIONS);
    }

    #[test]
    fn test_deps_fallback_truncates_oversized_content() {
        use super::super::{chunks_to_definitions, KIND_FALLBACK_MAX_CONTENT_BYTES};
        let mut big = make_const_chunk("BIG", 1);
        big.content = "x".repeat(KIND_FALLBACK_MAX_CONTENT_BYTES * 2);
        let defs = chunks_to_definitions(&[big]);
        let content = defs[0]["content"].as_str().unwrap();
        assert!(content.ends_with("... (truncated)"));
        assert_eq!(defs[0]["truncated"], true);
    }
}
