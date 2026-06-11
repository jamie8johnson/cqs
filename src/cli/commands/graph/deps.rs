//! Type dependency command for cqs
//!
//! Shows which chunks reference a type (forward), or what types a function uses (reverse).
//! Core JSON builders are shared between CLI and batch handlers.

use std::path::Path;

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::store::{ChunkSummary, ReadOnly, Store, TypeUsage};

use super::notes_text;
use super::KindFallbackOutput;

// ─── Args (surface-agnostic, MCP-ready) ────────────────────────────────────

/// Input for [`deps_core`]. Cross-project deps is not yet supported (both
/// surfaces warn and return the local result); the flag lives on the
/// adapter side, so the core covers the single-project path.
#[derive(Debug, serde::Deserialize)]
#[serde(default)]
pub(crate) struct DepsArgs {
    /// Type name (forward) or function name (with `reverse`).
    pub name: String,
    /// Reverse: show types used by a function instead of type users.
    pub reverse: bool,
    /// Cap on type users (forward) or used types (reverse), clamped 1..=100.
    pub limit: usize,
}

impl Default for DepsArgs {
    fn default() -> Self {
        Self {
            name: String::new(),
            reverse: false,
            // Mirrors clap `LimitArg` default.
            limit: crate::cli::args::DEFAULT_LIMIT,
        }
    }
}

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

/// Single JSON-schema source for `cqs deps <name>`. Three happy-path
/// shapes plus the shared fallback. `#[serde(untagged)]` preserves the
/// historical wire shapes: reverse ⇒ `{name, types, count}`, forward ⇒
/// `[DepsUserEntry, …]`, kind mismatch ⇒ the fallback object.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum DepsCoreOutput {
    /// `--reverse`: types used by a function — `{name, types, count}`.
    Reverse(DepsReverseOutput),
    /// Forward (default): chunks that use a type — flat array.
    Forward(Vec<DepsUserEntry>),
    /// Kind mismatch (const/module/ambiguous): the shared fallback object.
    Fallback(KindFallbackOutput),
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

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs deps <name>` (single-project).
///
/// deps is dual-mode: forward (default) lists chunks that use a type;
/// reverse (`--reverse`) lists types a function uses. Function (reverse)
/// and Type (forward) both have valid semantics and run the normal flow;
/// Const / Module / Ambiguous fall back since deps' "uses-of-X" model
/// doesn't fit them. `Type` deliberately routes to the forward query even
/// though it is a fallback-eligible kind elsewhere — that's why this
/// matches on the `FallbackKind` rather than blindly emitting a fallback.
pub(crate) fn deps_core(
    store: &Store<ReadOnly>,
    root: &Path,
    args: &DepsArgs,
) -> Result<DepsCoreOutput> {
    let _span =
        tracing::info_span!("deps_core", name = %args.name, reverse = args.reverse, limit = args.limit)
            .entered();
    let limit = args.limit.clamp(1, crate::cli::GRAPH_LIMIT_CAP);

    // Const/Module/Ambiguous don't fit deps' model. `notes_text::deps`
    // returns `None` for Type, encoding "deps runs the forward query for a
    // type" — so a Type classification falls through to the normal flow.
    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        if let Some(text) = notes_text::deps(fk) {
            return Ok(DepsCoreOutput::Fallback(KindFallbackOutput::new(
                &args.name, &chunks, fk, "deps", &text,
            )));
        }
    }

    // Forward (no `--reverse`) deps answers "who uses this *type*?". A
    // function name routes through here too (Function isn't a fallback kind),
    // runs `get_type_users`, finds nothing, and would emit a silent empty
    // list. Detect that misroute and surface the kind-labeled redirect to
    // `--reverse` instead — mirroring the established fallback shape.
    if !args.reverse {
        match cqs::kind::detect_kind_for_store(store, &args.name) {
            Ok((cqs::kind::KindResolution::Resolved(cqs::kind::Kind::Function), def_chunks)) => {
                // Reuse the rows the kind detection already read — one read
                // feeds both the routing decision and the rendering
                // (DS-V1.40-8/10), so no second `get_chunks_by_name` here.
                let definitions = super::chunks_to_definitions(&def_chunks);
                super::record_kind_fallback(&args.name, "function", "deps", definitions.len());
                return Ok(DepsCoreOutput::Fallback(super::KindFallbackOutput {
                    kind: "function",
                    fallback_from: "deps",
                    name: args.name.clone(),
                    definitions,
                    note: notes_text::deps_function_forward_note(),
                }));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, name = %args.name, "deps forward kind detection failed; running forward query");
            }
        }
    }

    if args.reverse {
        // Limit at SQL time so we don't fetch every edge of a popular
        // function just to drop the tail.
        let types = store
            .get_types_used_by(&args.name, limit)
            .context("Failed to load type dependencies")?;
        Ok(DepsCoreOutput::Reverse(build_deps_reverse(
            &args.name, &types,
        )))
    } else {
        let users = store
            .get_type_users(&args.name, limit)
            .context("Failed to load type users")?;
        Ok(DepsCoreOutput::Forward(build_deps_forward(&users, root)))
    }
}

// ─── CLI command (thin adapter over the core) ──────────────────────────────

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

    let args = DepsArgs {
        name: name.to_string(),
        reverse,
        limit,
    };
    match deps_core(store, root, &args)? {
        DepsCoreOutput::Fallback(fb) => {
            if json {
                crate::cli::json_envelope::emit_json(&fb)?;
            } else {
                render_deps_fallback_text(name, reverse, store)?;
            }
        }
        DepsCoreOutput::Reverse(output) => {
            if json {
                crate::cli::json_envelope::emit_json(&output)?;
            } else if output.types.is_empty() {
                println!("No type dependencies found for '{}'", name);
            } else {
                println!("Types used by '{}':", name.cyan());
                println!();
                for t in &output.types {
                    if t.edge_kind.is_empty() {
                        println!("  {}", t.type_name);
                    } else {
                        println!("  {} ({})", t.type_name, t.edge_kind.dimmed());
                    }
                }
                println!();
                println!("Total: {} type(s)", output.count);
            }
        }
        DepsCoreOutput::Forward(users) => {
            if json {
                crate::cli::json_envelope::emit_json(&users)?;
            } else if users.is_empty() {
                println!("No users found for type '{}'", name);
            } else {
                println!("Chunks that use type '{}':", name.cyan());
                println!();
                for user in &users {
                    println!("  {} ({}:{})", user.name.cyan(), user.file, user.line_start);
                }
                println!();
                println!("Total: {} user(s)", users.len());
            }
        }
    }
    Ok(())
}

/// Plain-text deps fallback renderer. The core decided a fallback fires;
/// for text the adapter re-runs `detect_fallback` (cheap indexed lookup)
/// to print the definition list. Type never reaches here (it routes to the
/// forward query), so an unexpected `None` from `notes_text::deps` is a
/// no-op.
fn render_deps_fallback_text(name: &str, reverse: bool, store: &Store<ReadOnly>) -> Result<()> {
    let (chunks, fallback) = super::detect_fallback(store, name);
    if let Some(fk) = fallback {
        if let (Some(text), Some(lead)) = (notes_text::deps(fk), notes_text::deps_lead(fk, name)) {
            super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Definitions:");
        }
        return Ok(());
    }
    // Forward-deps Function misroute: the core emitted the `function`
    // fallback; mirror it in text.
    if !reverse {
        if let Ok((cqs::kind::KindResolution::Resolved(cqs::kind::Kind::Function), def_chunks)) =
            cqs::kind::detect_kind_for_store(store, name)
        {
            // Reuse the rows kind detection already read (DS-V1.40-8/10).
            super::render_kind_fallback_text(
                &notes_text::deps_function_forward_lead(name),
                &def_chunks,
                notes_text::deps_function_forward_redirect(),
                "Definitions:",
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wire caller can supply just `name` and inherit the defaults.
    #[test]
    fn deps_args_deserialize_minimal() {
        let args: DepsArgs = serde_json::from_str(r#"{"name":"Config"}"#).unwrap();
        assert_eq!(args.name, "Config");
        assert!(!args.reverse);
        assert_eq!(args.limit, crate::cli::args::DEFAULT_LIMIT);
    }

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

    /// Forward deps (no `--reverse`) on a *function* name finds no
    /// type-users and historically emitted a silent empty list. It must now
    /// short-circuit to a `function` kind fallback that redirects the caller
    /// to `--reverse`, instead of the misleading empty result.
    #[test]
    fn deps_forward_on_function_emits_function_fallback() {
        use cqs::store::Store;

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let mock_embedding = cqs::Embedding::new(emb_vec);

        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&db).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let content = "fn do_work() {}";
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = cqs::parser::Chunk {
            id: format!("src/lib.rs:1:{}", &hash[..8]),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: cqs::parser::Language::Rust,
            chunk_type: cqs::parser::ChunkType::Function,
            name: "do_work".to_string(),
            signature: "fn do_work()".to_string(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 1,
            content_hash: hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding)], Some(100))
            .unwrap();
        drop(store);

        // Reopen read-only — `deps_core` takes a `Store<ReadOnly>`.
        let store = Store::open_readonly(&db).unwrap();
        let args = DepsArgs {
            name: "do_work".to_string(),
            reverse: false,
            limit: 50,
        };
        let out = deps_core(&store, dir.path(), &args).unwrap();
        match out {
            DepsCoreOutput::Fallback(fb) => {
                assert_eq!(fb.kind, "function");
                assert_eq!(fb.fallback_from, "deps");
                assert_eq!(fb.name, "do_work");
                assert_eq!(fb.definitions.len(), 1);
                assert!(
                    fb.note.contains("--reverse"),
                    "note should redirect to --reverse: {}",
                    fb.note
                );
            }
            other => panic!("expected function fallback, got: {other:?}"),
        }

        // Reverse mode on the same function runs the normal flow (no
        // fallback) — pin that the misroute guard is forward-only.
        let rev = DepsArgs {
            name: "do_work".to_string(),
            reverse: true,
            limit: 50,
        };
        match deps_core(&store, dir.path(), &rev).unwrap() {
            DepsCoreOutput::Reverse(_) => {}
            other => panic!("reverse deps on a function must run normal flow, got: {other:?}"),
        }
    }
}
