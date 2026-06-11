//! Call graph commands for cqs
//!
//! Provides callers/callees analysis.
//!
//! ## Polymorphic routing
//!
//! `cqs callers <name>` and `cqs callees <name>` consult
//! `cqs::kind::classify_hits` against an exact-name lookup before the
//! call-graph query (see `docs/polymorphic-routing.md`): kind-mismatch
//! fallbacks return an object with `kind`, `fallback_from`, `name`,
//! `definitions`, and `note` fields.
//!
//! ## Output topology (function path)
//!
//! Both commands emit one object shape on the function path: callers returns
//! `{name, callers: [...], count}` and callees returns `{name, calls: [...],
//! count}` (cross-project adds a `project` field to each entry). Agents
//! discriminate the dispatch decision by key-probing, not by JSON type: a
//! `fallback_from` key means the kind-mismatch fallback fired; a `callers` /
//! `calls` key means the function path ran. Both paths are JSON objects.

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::normalize_path;
use cqs::store::{CallerInfo, ReadOnly, Store};

use super::notes_text;
use super::KindFallbackOutput;

// ─── Args (surface-agnostic, MCP-ready) ────────────────────────────────────

/// Input for [`callers_core`] / [`callees_core`]. Both commands take the
/// same shape, so they share one struct.
///
/// Cross-project resolution lives in the CLI / daemon adapters, not the
/// core: the cross-project path has its own (multi-index) semantics and no
/// kind-fallback. The core covers the single-project path both surfaces
/// share.
#[derive(Debug, serde::Deserialize)]
#[serde(default)]
pub(crate) struct CallersArgs {
    /// Function name to analyze.
    pub name: String,
    /// Max callers/callees returned (clamped 1..=100 inside the core).
    pub limit: usize,
}

impl Default for CallersArgs {
    fn default() -> Self {
        Self {
            name: String::new(),
            // Mirrors clap `LimitArg` default.
            limit: crate::cli::args::DEFAULT_LIMIT,
        }
    }
}

/// Alias so `callees` reads naturally at call sites. Same shape as
/// [`CallersArgs`].
pub(crate) type CalleesArgs = CallersArgs;

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct CallerEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    /// Originating project name on the cross-project path; omitted (single
    /// project) on the standard path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleeEntry {
    pub name: String,
    pub line_start: u32,
    /// Originating project name on the cross-project path; omitted (single
    /// project) on the standard path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub project: String,
}

/// Function-path output for `cqs callers <name>`: `{name, callers, count}`.
/// The mirror of [`CalleesOutput`] — both commands share one object
/// topology so agents key-probe (`callers` vs `calls`) rather than
/// discriminating array-vs-object.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CallersOutput {
    pub name: String,
    pub callers: Vec<CallerEntry>,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleesOutput {
    pub name: String,
    pub calls: Vec<CalleeEntry>,
    pub count: usize,
}

/// Single JSON-schema source for `cqs callers <name>`. Happy path is the
/// `{name, callers, count}` object (cross-project adds `project` per entry);
/// a kind mismatch yields the shared fallback object. Both variants are JSON
/// objects — agents key-probe (`callers` ⇒ function path, `fallback_from` ⇒
/// fallback) rather than discriminating by JSON type.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum CallersCoreOutput {
    /// Function path: `{name, callers, count}`.
    Callers(CallersOutput),
    /// Kind mismatch: `{kind, fallback_from, name, definitions, note}`.
    Fallback(KindFallbackOutput),
}

/// Single JSON-schema source for `cqs callees <name>`. Happy path is the
/// `{name, calls, count}` object; kind mismatch is the shared fallback.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum CalleesCoreOutput {
    /// Function path: `{name, calls, count}`.
    Callees(CalleesOutput),
    /// Kind mismatch: `{kind, fallback_from, name, definitions, note}`.
    Fallback(KindFallbackOutput),
}

// ─── Shared JSON builders ──────────────────────────────────────────────────

/// Build typed caller entries from caller info -- shared between CLI and batch.
pub(crate) fn build_caller_entries(callers: &[CallerInfo]) -> Vec<CallerEntry> {
    let _span = tracing::info_span!("build_caller_entries", count = callers.len()).entered();
    callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: normalize_path(&c.file).to_string(),
            line_start: c.line,
            project: String::new(),
        })
        .collect()
}

/// Build typed callers output (`{name, callers, count}`) -- the function-path
/// shape both surfaces emit. Mirror of [`build_callees`].
pub(crate) fn build_callers(name: &str, callers: &[CallerInfo]) -> CallersOutput {
    let _span = tracing::info_span!("build_callers", name, count = callers.len()).entered();
    let entries = build_caller_entries(callers);
    CallersOutput {
        name: name.to_string(),
        count: entries.len(),
        callers: entries,
    }
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
                project: String::new(),
            })
            .collect(),
        count: callees.len(),
    }
}

// ─── Cores ──────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs callers <name>` (single-project).
///
/// All logic lives here: cap normalization, kind classification, fallback
/// vs. function-path selection, and the SQL → typed-output translation.
/// Never prints, reads env, or branches on surface — the CLI and daemon
/// adapters render the returned [`CallersCoreOutput`].
pub(crate) fn callers_core(
    store: &Store<ReadOnly>,
    args: &CallersArgs,
) -> Result<CallersCoreOutput> {
    let _span =
        tracing::info_span!("callers_core", name = %args.name, limit = args.limit).entered();
    // Standardised cap. The store query returns every caller; we truncate
    // before rendering so the user can paginate via repeated calls.
    let limit = args.limit.clamp(1, 100);

    // Polymorphic-routing kind detection. Dispatch the kind-mismatch
    // fallback before the call-graph query.
    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::callers(fk);
        return Ok(CallersCoreOutput::Fallback(KindFallbackOutput::new(
            &args.name, &chunks, fk, "callers", &text,
        )));
    }

    let mut callers = store
        .get_callers_full(&args.name)
        .context("Failed to load callers")?;
    callers.truncate(limit);
    Ok(CallersCoreOutput::Callers(build_callers(
        &args.name, &callers,
    )))
}

/// Surface-agnostic core for `cqs callees <name>` (single-project).
pub(crate) fn callees_core(
    store: &Store<ReadOnly>,
    args: &CalleesArgs,
) -> Result<CalleesCoreOutput> {
    let _span =
        tracing::info_span!("callees_core", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, 100);

    let (chunks, fallback) = super::detect_fallback(store, &args.name);
    if let Some(fk) = fallback {
        let text = notes_text::callees(fk);
        return Ok(CalleesCoreOutput::Fallback(KindFallbackOutput::new(
            &args.name, &chunks, fk, "callees", &text,
        )));
    }

    let mut callees = store
        .get_callees_full(&args.name, None)
        .context("Failed to load callees")?;
    callees.truncate(limit);
    Ok(CalleesCoreOutput::Callees(build_callees(
        &args.name, &callees,
    )))
}

// ─── Cross-project cores ─────────────────────────────────────────────────────
//
// The cross-project path has its own (multi-index) retrieval and no
// kind-fallback, so it gets its own core rather than a branch inside
// `callers_core`. Both surfaces (CLI `cmd_callers --cross-project`, daemon
// `dispatch_callers` cross branch) call these so the cap discipline and the
// `{name, callers|calls, count}` projection can't drift. Output topology is
// the same object shape as the single-project path; each entry gains a
// `project` field.

/// Surface-agnostic core for `cqs callers <name> --cross-project`.
///
/// Resolves callers across every project in `cross_ctx`, applies the shared
/// `1..=100` cap, and projects to the same `{name, callers, count}` object
/// the single-project core emits — entries carry their originating project.
pub(crate) fn callers_cross_core(
    cross_ctx: &mut cqs::cross_project::CrossProjectContext,
    args: &CallersArgs,
) -> Result<CallersOutput> {
    let _span =
        tracing::info_span!("callers_cross_core", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, 100);
    let mut callers = cross_ctx
        .get_callers_cross(&args.name)
        .context("Failed to load cross-project callers")?;
    callers.truncate(limit);
    let entries: Vec<CallerEntry> = callers
        .iter()
        .map(|c| CallerEntry {
            name: c.caller.name.clone(),
            file: normalize_path(&c.caller.file).to_string(),
            line_start: c.caller.line,
            project: c.project.clone(),
        })
        .collect();
    Ok(CallersOutput {
        name: args.name.clone(),
        count: entries.len(),
        callers: entries,
    })
}

/// Surface-agnostic core for `cqs callees <name> --cross-project`.
///
/// Mirror of [`callers_cross_core`]: projects to `{name, calls, count}` with
/// a `project` field on each entry.
pub(crate) fn callees_cross_core(
    cross_ctx: &mut cqs::cross_project::CrossProjectContext,
    args: &CalleesArgs,
) -> Result<CalleesOutput> {
    let _span =
        tracing::info_span!("callees_cross_core", name = %args.name, limit = args.limit).entered();
    let limit = args.limit.clamp(1, 100);
    let mut callees = cross_ctx
        .get_callees_cross(&args.name)
        .context("Failed to load cross-project callees")?;
    callees.truncate(limit);
    let calls: Vec<CalleeEntry> = callees
        .iter()
        .map(|c| CalleeEntry {
            name: c.name.clone(),
            line_start: c.line,
            project: c.project.clone(),
        })
        .collect();
    Ok(CalleesOutput {
        name: args.name.clone(),
        count: calls.len(),
        calls,
    })
}

// ─── CLI commands (thin adapters over the cores) ───────────────────────────

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
    let limit = limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let output = callers_cross_core(
            &mut cross_ctx,
            &CallersArgs {
                name: name.to_string(),
                limit,
            },
        )?;

        if json {
            crate::cli::json_envelope::emit_json(&output)?;
        } else if output.callers.is_empty() {
            println!("No callers found for '{}' (cross-project)", name);
        } else {
            println!("Functions that call '{}' (cross-project):", name);
            println!();
            for c in &output.callers {
                println!(
                    "  {} ({}:{}) [{}]",
                    c.name.cyan(),
                    c.file,
                    c.line_start,
                    c.project.dimmed()
                );
            }
            println!();
            println!("Total: {} caller(s)", output.count);
        }
        return Ok(());
    }

    // Standard single-project path — delegate to the shared core.
    match callers_core(
        store,
        &CallersArgs {
            name: name.to_string(),
            limit,
        },
    )? {
        CallersCoreOutput::Fallback(fb) => {
            if json {
                crate::cli::json_envelope::emit_json(&fb)?;
            } else {
                render_callers_fallback_text(name, store)?;
            }
        }
        CallersCoreOutput::Callers(output) => {
            if json {
                crate::cli::json_envelope::emit_json(&output)?;
            } else if output.callers.is_empty() {
                println!("No callers found for '{}'", name);
            } else {
                println!("Functions that call '{}':", name);
                println!();
                for caller in &output.callers {
                    println!(
                        "  {} ({}:{})",
                        caller.name.cyan(),
                        caller.file,
                        caller.line_start
                    );
                }
                println!();
                println!("Total: {} caller(s)", output.count);
            }
        }
    }
    Ok(())
}

/// Re-classify and render the plain-text callers fallback. The core
/// already decided a fallback fires; for text rendering the adapter needs
/// the chunks + kind to print the definition list, so it re-runs
/// `detect_fallback` (cheap indexed lookup). JSON callers render the typed
/// [`KindFallbackOutput`] directly and skip this.
fn render_callers_fallback_text(name: &str, store: &Store<ReadOnly>) -> Result<()> {
    let (chunks, fallback) = super::detect_fallback(store, name);
    if let Some(fk) = fallback {
        let text = notes_text::callers(fk);
        let lead = notes_text::callers_lead(fk, name);
        super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Definitions:");
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
    // See cmd_callers — same clamp range.
    let limit = limit.clamp(1, 100);

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let output = callees_cross_core(
            &mut cross_ctx,
            &CalleesArgs {
                name: name.to_string(),
                limit,
            },
        )?;

        if json {
            crate::cli::json_envelope::emit_json(&output)?;
        } else {
            println!("Functions called by '{}' (cross-project):", name.cyan());
            println!();
            if output.calls.is_empty() {
                println!("  (no function calls found)");
            } else {
                for c in &output.calls {
                    println!("  {} [{}]", c.name, c.project.dimmed());
                }
            }
            println!();
            println!("Total: {} call(s)", output.count);
        }
        return Ok(());
    }

    // Standard single-project path — delegate to the shared core.
    match callees_core(
        store,
        &CalleesArgs {
            name: name.to_string(),
            limit,
        },
    )? {
        CalleesCoreOutput::Fallback(fb) => {
            if json {
                crate::cli::json_envelope::emit_json(&fb)?;
            } else {
                render_callees_fallback_text(name, store)?;
            }
        }
        CalleesCoreOutput::Callees(output) => {
            if json {
                crate::cli::json_envelope::emit_json(&output)?;
            } else {
                println!("Functions called by '{}':", name.cyan());
                println!();
                if output.calls.is_empty() {
                    println!("  (no function calls found)");
                } else {
                    for callee in &output.calls {
                        println!("  {}", callee.name);
                    }
                }
                println!();
                println!("Total: {} call(s)", output.count);
            }
        }
    }
    Ok(())
}

/// Plain-text callees fallback renderer. See [`render_callers_fallback_text`].
fn render_callees_fallback_text(name: &str, store: &Store<ReadOnly>) -> Result<()> {
    let (chunks, fallback) = super::detect_fallback(store, name);
    if let Some(fk) = fallback {
        let text = notes_text::callees(fk);
        let lead = notes_text::callees_lead(fk, name);
        super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Definitions:");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::chunks_to_definitions;
    use super::*;
    use cqs::store::ChunkSummary;

    /// A wire caller can supply just `name` and inherit the default limit.
    #[test]
    fn callers_args_deserialize_minimal() {
        let args: CallersArgs = serde_json::from_str(r#"{"name":"foo"}"#).unwrap();
        assert_eq!(args.name, "foo");
        assert_eq!(args.limit, crate::cli::args::DEFAULT_LIMIT);
    }

    #[test]
    fn test_caller_entry_field_names() {
        let entry = CallerEntry {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
            project: String::new(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());
        // Single-project entry omits the empty `project` field.
        assert!(json.get("project").is_none());
    }

    #[test]
    fn test_caller_entry_project_field_present_when_set() {
        let entry = CallerEntry {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
            project: "openclaw".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["project"], "openclaw");
    }

    #[test]
    fn test_build_callers_empty() {
        let output = build_callers("foo", &[]);
        assert_eq!(output.count, 0);
        assert!(output.callers.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        // Callers now shares the callees object topology: {name, callers, count}.
        assert_eq!(json["name"], "foo");
        assert!(json["callers"].as_array().unwrap().is_empty());
        assert_eq!(json["count"], 0);
    }

    #[test]
    fn test_build_callers_object_shape() {
        let info = vec![CallerInfo {
            name: "caller_fn".into(),
            file: std::path::PathBuf::from("src/lib.rs"),
            line: 12,
        }];
        let output = build_callers("target", &info);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "target");
        assert_eq!(json["count"], 1);
        assert_eq!(json["callers"][0]["name"], "caller_fn");
        assert_eq!(json["callers"][0]["line_start"], 12);
        // Mirror of callees: agents key-probe `callers` vs `calls`.
        assert!(json.get("calls").is_none());
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
        assert_eq!(json["name"], "bar");
        assert!(json.get("function").is_none());
        assert_eq!(json["calls"][0]["line_start"], 10);
    }

    // Polymorphic-routing callers + callees kind-mismatch fallback shape.
    // Each test pins the JSON-builder contract so future schema tweaks are
    // deliberate, not accidental.

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
        // Build the typed `KindFallbackOutput` the core emits and serialize
        // it (rather than a hand-rolled `json!` literal), so this pins the
        // production fallback shape.
        use super::super::notes_text::FallbackKind;
        let chunk = make_chunk(cqs::parser::ChunkType::Constant, "X", "src/a.rs", 5);
        let text = notes_text::callers(FallbackKind::Const);
        let out = KindFallbackOutput::new("X", &[chunk], FallbackKind::Const, "callers", &text);
        let payload = serde_json::to_value(&out).unwrap();
        assert_eq!(payload["kind"], "const");
        assert_eq!(payload["fallback_from"], "callers");
        assert_eq!(payload["name"], "X");
        assert_eq!(payload["definitions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_callees_fallback_payload_shape() {
        // Mirror of the callers payload via the typed builder, with
        // `fallback_from: "callees"`.
        use super::super::notes_text::FallbackKind;
        let chunk = make_chunk(cqs::parser::ChunkType::Class, "MyClass", "src/a.rs", 5);
        let text = notes_text::callees(FallbackKind::Type);
        let out =
            KindFallbackOutput::new("MyClass", &[chunk], FallbackKind::Type, "callees", &text);
        let payload = serde_json::to_value(&out).unwrap();
        assert_eq!(payload["fallback_from"], "callees");
        assert_eq!(payload["definitions"][0]["chunk_type"], "class");
    }

    // The callers/callees kind fallback routes through the shared
    // `chunks_to_definitions`, which caps the entry count and truncates
    // oversized content. Without these caps, `cqs callers Result --json`
    // on a hot name could emit unbounded multi-MB JSON.
    #[test]
    fn test_callers_fallback_caps_definitions_count() {
        use super::super::KIND_FALLBACK_MAX_DEFINITIONS;
        let chunks: Vec<ChunkSummary> = (0..(KIND_FALLBACK_MAX_DEFINITIONS + 50))
            .map(|i| {
                make_chunk(
                    cqs::parser::ChunkType::Constant,
                    &format!("X{i}"),
                    "src/lib.rs",
                    i as u32,
                )
            })
            .collect();
        let defs = chunks_to_definitions(&chunks);
        assert_eq!(defs.len(), KIND_FALLBACK_MAX_DEFINITIONS);
    }

    #[test]
    fn test_callers_fallback_truncates_oversized_content() {
        use super::super::KIND_FALLBACK_MAX_CONTENT_BYTES;
        let mut big = make_chunk(cqs::parser::ChunkType::Constant, "BIG", "src/lib.rs", 1);
        big.content = "x".repeat(KIND_FALLBACK_MAX_CONTENT_BYTES * 2);
        let defs = chunks_to_definitions(&[big]);
        let content = defs[0]["content"].as_str().unwrap();
        assert!(content.ends_with("... (truncated)"));
        assert_eq!(defs[0]["truncated"], true);
    }
}
