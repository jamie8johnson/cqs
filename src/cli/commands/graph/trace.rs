//! Trace command — find shortest call path between two functions
//!
//! ## Polymorphic routing (Phase 1)
//!
//! `cqs trace <source> <target>` historically required both names to be
//! function-or-method names. If either resolves to a non-Function chunk
//! the call-graph BFS returns no path. Polymorphic-routing Phase 1
//! detects the source name's kind up-front: a non-Function source
//! short-circuits with a kind-labeled definition list + redirect note
//! instead of "no path found". The target name is left to
//! `resolve_target` (which produces its own typed error if missing).

use std::collections::{HashMap, VecDeque};

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::store::ReadOnly;
use cqs::Store;

use super::notes_text;
use crate::cli::commands::resolve::resolve_target;
use crate::cli::OutputFormat;

// ─── Args (surface-agnostic, MCP-ready) ────────────────────────────────────

/// Input for [`trace_core`]. Cross-project trace lives in the adapters
/// (separate cross-project BFS, no kind-fallback); the core covers the
/// single-project path.
#[derive(Debug, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TraceArgs {
    /// Source function name or `file:function`.
    pub source: String,
    /// Target function name or `file:function`.
    pub target: String,
    /// Max search depth — "give up after N hops."
    pub max_depth: usize,
    /// BFS visited-node ceiling (OOM guard on dense graphs). Resolved once at
    /// the adapter boundary from `CQS_TRACE_MAX_NODES` (default 10,000) via
    /// [`trace_max_nodes`]; the core never reads the env itself. `#[serde(default)]`
    /// so an MCP/wire caller that omits it falls back to the default ceiling.
    #[serde(default = "trace_max_nodes")]
    pub max_nodes: usize,
}

impl Default for TraceArgs {
    fn default() -> Self {
        Self {
            source: String::new(),
            target: String::new(),
            // Mirrors clap `--max-depth` default (`DEFAULT_DEPTH_TRACE`).
            max_depth: crate::cli::args::DEFAULT_DEPTH_TRACE as usize,
            max_nodes: trace_max_nodes(),
        }
    }
}

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceHop {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file: String,
    pub line_start: u32, // was "line"
    #[serde(skip_serializing_if = "String::is_empty")]
    pub signature: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceOutput {
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<TraceHop>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub found: bool,
    /// Source definition sites when the source name resolved to several
    /// same-kind callables (`KindResolution::Multiple` — e.g. a free function
    /// and a method that share a name). `resolve_target` picks one to seed the
    /// BFS, but the other candidates would otherwise vanish silently; this
    /// surfaces them alongside the path so the caller can re-run against a
    /// disambiguated `Type::method` if the chosen seed was the wrong one.
    /// Skip-when-empty: omitted entirely for the unambiguous single-source
    /// case, so the historical wire shape is byte-stable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<serde_json::Value>,
}

/// Trace's kind-mismatch fallback payload. Unlike the other graph
/// commands, trace classifies the *source* name and carries
/// `source`/`target` rather than a single `name` — so it cannot reuse the
/// shared `KindFallbackOutput`. Serializes to
/// `{kind, fallback_from: "trace", source, target, definitions, note}`,
/// the exact shape both surfaces have always emitted.
#[derive(Debug, serde::Serialize)]
pub(crate) struct TraceKindFallbackOutput {
    pub kind: &'static str,
    pub fallback_from: &'static str,
    pub source: String,
    pub target: String,
    pub definitions: Vec<serde_json::Value>,
    pub note: &'static str,
}

/// Single JSON-schema source for `cqs trace <source> <target>`. Happy path
/// is the `TraceOutput` object; a source kind-mismatch is the
/// trace-specific fallback object.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum TraceCoreOutput {
    /// Function path: `{source, target, path?, depth?, found}`.
    Trace(TraceOutput),
    /// Source kind mismatch: trace-shaped fallback object.
    Fallback(TraceKindFallbackOutput),
}

// ─── Shared JSON builder ───────────────────────────────────────────────────

/// Build typed trace output from BFS result.
///
/// Shared between CLI (`cmd_trace --json`) and batch (`dispatch_trace`).
/// Takes the BFS path (or None) and resolves chunk metadata via batch lookup.
pub(crate) fn build_trace_output<Mode>(
    store: &Store<Mode>,
    source_name: &str,
    target_name: &str,
    path: Option<&[String]>,
    root: &std::path::Path,
) -> Result<TraceOutput> {
    let _span = tracing::info_span!("build_trace_output", source_name, target_name).entered();

    match path {
        Some(names) => {
            let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            let batch_results = store.search_by_names_batch(&name_refs, 1)?;

            let hops: Vec<TraceHop> = names
                .iter()
                .map(
                    |name| match batch_results.get(name.as_str()).and_then(|v| v.first()) {
                        Some(r) => TraceHop {
                            name: name.clone(),
                            file: cqs::rel_display(&r.chunk.file, root).to_string(),
                            line_start: r.chunk.line_start,
                            signature: r.chunk.signature.clone(),
                        },
                        None => {
                            tracing::warn!(name, "Trace hop not found in index");
                            TraceHop {
                                name: name.clone(),
                                file: String::new(),
                                line_start: 0,
                                signature: String::new(),
                            }
                        }
                    },
                )
                .collect();

            Ok(TraceOutput {
                source: source_name.to_string(),
                target: target_name.to_string(),
                depth: Some(hops.len().saturating_sub(1)),
                path: Some(hops),
                found: true,
                candidates: Vec::new(),
            })
        }
        None => Ok(TraceOutput {
            source: source_name.to_string(),
            target: target_name.to_string(),
            path: None,
            depth: None,
            found: false,
            candidates: Vec::new(),
        }),
    }
}

// ─── Core ───────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs trace <source> <target>` (single-project).
///
/// Classifies the source name: a non-callable source (const/type/module/
/// ambiguous) short-circuits to a kind-labeled fallback instead of running
/// the BFS to "no path found". Otherwise resolves both names, handles the
/// trivial `source == target` case, and runs the shortest-path BFS over the
/// passed-in call graph (passed in, like test-map, so each adapter supplies
/// its own cached source).
pub(crate) fn trace_core(
    store: &Store<ReadOnly>,
    graph: &cqs::store::CallGraph,
    root: &std::path::Path,
    args: &TraceArgs,
) -> Result<TraceCoreOutput> {
    let _span =
        tracing::info_span!("trace_core", source = %args.source, target = %args.target).entered();

    // Polymorphic-routing kind detection on the source name. The trace BFS
    // requires a callable starting node. Classify the source once and route
    // every non-callable shape to an explicit response instead of letting
    // the BFS silently return "no path found":
    //   - Const / Type / Module / Ambiguous → the shared kind-labeled
    //     fallback (`super::fallback_kind`).
    //   - Other (macro / impl / service / stored-proc / extern) → a
    //     trace-specific `Kind::Other` fallback; these chunk types aren't
    //     callable nodes, so no call path can originate from them.
    //   - Function / Multiple → proceed to the BFS. `Multiple` (several
    //     same-kind callables sharing a name) additionally surfaces its
    //     candidate definition sites alongside the result so the caller can
    //     see the disambiguation `resolve_target` made for it.
    // The target's kind is left to `resolve_target` to surface its own
    // error if missing.
    // One read of the source's name rows: `detect_kind_for_store` returns
    // both the routing resolution and the full summaries it classified, so
    // the fallback / Other / Multiple renders below reuse `source_chunks`
    // rather than re-querying `WHERE name = ?` (DS-V1.40-8/10 — no snapshot
    // drift between the routing decision and the rendered definitions).
    let (source_resolution, source_chunks) =
        match cqs::kind::detect_kind_for_store(store, &args.source) {
            Ok(pair) => pair,
            Err(e) => {
                // Kind detection is a best-effort routing hint; a store hiccup
                // here must not kill the trace. Warn and fall through to the
                // normal resolve + BFS path, which runs its own queries.
                tracing::warn!(
                    error = %e,
                    source = %args.source,
                    "trace source kind detection failed; falling through to BFS"
                );
                (
                    cqs::kind::KindResolution::Resolved(cqs::kind::Kind::Function),
                    Vec::new(),
                )
            }
        };

    if let Some(fk) = super::fallback_kind(source_resolution) {
        let text = notes_text::trace(fk);
        let definitions = super::chunks_to_definitions(&source_chunks);
        super::record_kind_fallback(&args.source, fk.label(), "trace", definitions.len());
        return Ok(TraceCoreOutput::Fallback(TraceKindFallbackOutput {
            kind: fk.label(),
            fallback_from: "trace",
            source: args.source.clone(),
            target: args.target.clone(),
            definitions,
            note: text.note,
        }));
    }

    if source_resolution == cqs::kind::KindResolution::Resolved(cqs::kind::Kind::Other) {
        let definitions = super::chunks_to_definitions(&source_chunks);
        super::record_kind_fallback(&args.source, "other", "trace", definitions.len());
        return Ok(TraceCoreOutput::Fallback(TraceKindFallbackOutput {
            kind: "other",
            fallback_from: "trace",
            source: args.source.clone(),
            target: args.target.clone(),
            definitions,
            note: notes_text::trace_other_note(),
        }));
    }

    // Resolve source and target to chunk names.
    let source_resolved = resolve_target(store, &args.source)?;
    let target_resolved = resolve_target(store, &args.target)?;
    let source_name = source_resolved.chunk.name.clone();
    let target_name = target_resolved.chunk.name.clone();

    // Symmetric target-kind validation. The source is routed up-front (a
    // non-callable source short-circuits to a fallback); the target was
    // historically left entirely to `resolve_target`, so a non-callable
    // target — which can never be reached as a callable BFS node — would
    // silently report "no path found" with no indication the target was
    // the wrong kind. Classify the target and warn on a non-callable so the
    // empty result is attributable.
    match cqs::kind::detect_kind_for_store(store, &args.target) {
        Ok((target_resolution, _chunks)) => {
            if !matches!(
                target_resolution,
                cqs::kind::KindResolution::Resolved(cqs::kind::Kind::Function)
                    | cqs::kind::KindResolution::Multiple
                    | cqs::kind::KindResolution::NotFound
            ) {
                tracing::warn!(
                    target = %args.target,
                    kind = ?target_resolution,
                    "trace target is not a callable; the call-graph BFS can only reach callable nodes, so any 'no path found' may reflect the target's kind rather than graph distance"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, target = %args.target, "trace target kind detection failed");
        }
    }

    // `KindResolution::Multiple`: several same-kind callables share this
    // source name. `resolve_target` picked one to seed the BFS; surface the
    // full candidate set so the caller sees what it chose among (and can
    // re-run against a disambiguated `Type::method` if the seed was wrong).
    // Reuse the rows read up-front (DS-V1.40-8/10) — no re-query.
    let source_candidates: Vec<serde_json::Value> =
        if source_resolution == cqs::kind::KindResolution::Multiple {
            super::chunks_to_definitions(&source_chunks)
        } else {
            // Single callable source (`Kind::Function`) — nothing to disambiguate.
            Vec::new()
        };

    // Trivial case: source == target.
    if source_name == target_name {
        let trivial_path = vec![source_name.clone()];
        let mut output =
            build_trace_output(store, &source_name, &target_name, Some(&trivial_path), root)?;
        output.candidates = source_candidates;
        return Ok(TraceCoreOutput::Trace(output));
    }

    let path = bfs_shortest_path(
        &graph.forward,
        &source_name,
        &target_name,
        args.max_depth,
        args.max_nodes,
    );
    let mut output = build_trace_output(store, &source_name, &target_name, path.as_deref(), root)?;
    output.candidates = source_candidates;
    Ok(TraceCoreOutput::Trace(output))
}

// ─── Cross-project core ──────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs trace <source> <target> --cross-project`.
///
/// Runs the cross-project shortest-path BFS and projects to the
/// [`cqs::cross_project::CrossProjectTraceResult`] both surfaces emit
/// (`{source, target, path?, depth?, found}` with per-hop `project`).
/// Carries no kind-fallback — the cross-project path never had one. Both the
/// CLI and daemon cross branches call this so the result assembly can't
/// drift. `max_depth` is the only knob; trace returns a single shortest path
/// so there is no `limit` cap to apply.
pub(crate) fn trace_cross_core(
    cross_ctx: &mut cqs::cross_project::CrossProjectContext,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Result<cqs::cross_project::CrossProjectTraceResult> {
    let _span = tracing::info_span!("trace_cross_core", source, target).entered();
    let result = cqs::cross_project::trace_cross(cross_ctx, source, target, max_depth)?;
    Ok(cqs::cross_project::CrossProjectTraceResult {
        source: source.to_string(),
        target: target.to_string(),
        depth: result.as_ref().map(|p| p.len().saturating_sub(1)),
        found: result.is_some(),
        path: result,
    })
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_trace(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    source: &str,
    target: &str,
    max_depth: usize,
    _limit: usize,
    format: &OutputFormat,
    cross_project: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_trace", source, target, cross_project).entered();
    // Task A3: `--limit` is accepted for parity with other graph commands.
    // Today trace returns a single shortest path so the cap is a no-op; left
    // in the signature so a future k-shortest-paths variant can read it
    // without a re-flatten and so batch users get a uniform flag set.

    if cross_project {
        let mut cross_ctx = cqs::cross_project::CrossProjectContext::from_config(&ctx.root)?;
        let trace_result = trace_cross_core(&mut cross_ctx, source, target, max_depth)?;

        // Exhaustive match instead of if/else-if chains so a future
        // `OutputFormat` variant fails to compile until every render site
        // adds an arm, rather than silently absorbing unknown formats as
        // "text".
        match format {
            OutputFormat::Json => {
                crate::cli::json_envelope::emit_json(&trace_result)?;
            }
            OutputFormat::Mermaid => {
                if let Some(ref path) = trace_result.path {
                    println!("graph TD");
                    for (i, hop) in path.iter().enumerate() {
                        let id = node_letter(i);
                        let label = if hop.project.is_empty() {
                            mermaid_escape(&hop.name)
                        } else {
                            format!(
                                "{} [{}]",
                                mermaid_escape(&hop.name),
                                mermaid_escape(&hop.project)
                            )
                        };
                        println!("    {}[\"{}\"]", id, label);
                    }
                    for i in 0..path.len().saturating_sub(1) {
                        println!("    {} --> {}", node_letter(i), node_letter(i + 1));
                    }
                } else {
                    println!("graph TD");
                    println!(
                        "    %% No call path found from {} to {} within depth {}",
                        source, target, max_depth
                    );
                }
            }
            OutputFormat::Text => {
                if let Some(ref path) = trace_result.path {
                    println!(
                        "Call path from {} to {} ({} hop{}, cross-project):",
                        source.cyan(),
                        target.cyan(),
                        path.len().saturating_sub(1),
                        if path.len().saturating_sub(1) == 1 {
                            ""
                        } else {
                            "s"
                        }
                    );
                    println!();
                    for (i, hop) in path.iter().enumerate() {
                        let prefix = if i == 0 {
                            "  ".to_string()
                        } else {
                            "  \u{2192} ".to_string()
                        };
                        if hop.project.is_empty() {
                            println!("{}{}", prefix, hop.name.cyan());
                        } else {
                            println!("{}{} [{}]", prefix, hop.name.cyan(), hop.project.dimmed());
                        }
                    }
                } else {
                    println!(
                        "No call path found from {} to {} within depth {} (cross-project).",
                        source.cyan(),
                        target.cyan(),
                        max_depth
                    );
                }
            }
        }
        return Ok(());
    }

    let store = &ctx.store;
    let root = &ctx.root;

    // Load the call graph up-front so the core stays surface-agnostic (the
    // daemon adapter passes its snapshot Arc instead). Cached at the store
    // level — cheap even when a source kind-fallback fires and the graph
    // goes unused.
    let graph = store
        .get_call_graph()
        .context("Failed to load call graph")?;

    let args = TraceArgs {
        source: source.to_string(),
        target: target.to_string(),
        max_depth,
        // Resolve the env ceiling once here, at the adapter boundary, so the
        // core receives a value instead of reading the process env.
        max_nodes: trace_max_nodes(),
    };
    match trace_core(store, &graph, root, &args)? {
        TraceCoreOutput::Fallback(fb) => match format {
            OutputFormat::Json => {
                crate::cli::json_envelope::emit_json(&fb)?;
            }
            OutputFormat::Text | OutputFormat::Mermaid => {
                render_trace_fallback_text(source, store)?;
            }
        },
        TraceCoreOutput::Trace(output) => render_trace_output(&output, format, max_depth)?,
    }

    Ok(())
}

/// Render a [`TraceOutput`] in the requested format. JSON emits the typed
/// struct directly; Text and Mermaid derive their rendering from the same
/// struct's hops so all three formats agree on the computed path.
fn render_trace_output(
    output: &TraceOutput,
    format: &OutputFormat,
    max_depth: usize,
) -> Result<()> {
    let source = &output.source;
    let target = &output.target;
    // Trivial case (`source == target`): the core returns a single-hop
    // path. Each format keeps its historical trivial-case rendering.
    let trivial = output.found && output.path.as_ref().map(|p| p.len() == 1).unwrap_or(false);

    match (format, &output.path) {
        (OutputFormat::Json, _) => {
            crate::cli::json_envelope::emit_json(output)?;
        }
        (OutputFormat::Text, Some(hops)) if trivial => {
            println!("{} and {} are the same function.", source, target);
            let _ = hops;
        }
        (OutputFormat::Mermaid, Some(hops)) if trivial => {
            let hop = &hops[0];
            println!("graph TD");
            if hop.file.is_empty() {
                println!("    A[\"{}\"]", mermaid_escape(&hop.name));
            } else {
                println!(
                    "    A[\"{} ({}:{})\"]",
                    mermaid_escape(&hop.name),
                    mermaid_escape(&hop.file),
                    hop.line_start
                );
            }
        }
        (OutputFormat::Text, Some(hops)) => {
            let edges = hops.len().saturating_sub(1);
            println!(
                "Call path from {} to {} ({} hop{}):",
                source.cyan(),
                target.cyan(),
                edges,
                if edges == 1 { "" } else { "s" }
            );
            println!();
            for (i, hop) in hops.iter().enumerate() {
                let prefix = if i == 0 {
                    "  ".to_string()
                } else {
                    "  \u{2192} ".to_string()
                };
                if hop.file.is_empty() {
                    println!("{}{}", prefix, hop.name.cyan());
                } else {
                    println!(
                        "{}{} ({}:{})",
                        prefix,
                        hop.name.cyan(),
                        hop.file,
                        hop.line_start
                    );
                }
            }
        }
        (OutputFormat::Mermaid, Some(hops)) => {
            println!("graph TD");
            for (i, hop) in hops.iter().enumerate() {
                let label = if hop.file.is_empty() {
                    mermaid_escape(&hop.name)
                } else {
                    format!(
                        "{} ({}:{})",
                        mermaid_escape(&hop.name),
                        mermaid_escape(&hop.file),
                        hop.line_start
                    )
                };
                println!("    {}[\"{}\"]", node_letter(i), label);
            }
            for i in 0..hops.len().saturating_sub(1) {
                println!("    {} --> {}", node_letter(i), node_letter(i + 1));
            }
        }
        // No path found (`output.path == None`).
        (OutputFormat::Text, None) => {
            println!(
                "No call path found from {} to {} within depth {}.",
                source.cyan(),
                target.cyan(),
                max_depth
            );
        }
        (OutputFormat::Mermaid, None) => {
            println!("graph TD");
            println!(
                "    %% No call path found from {} to {} within depth {}",
                source, target, max_depth
            );
        }
    }
    // `KindResolution::Multiple` source: surface the candidate definition
    // sites the BFS seed was chosen from so a Text consumer sees the disambiguation.
    // JSON already carries `candidates`; Mermaid stays graph-only.
    if matches!(format, OutputFormat::Text) && !output.candidates.is_empty() {
        println!();
        println!(
            "Note: `{}` matches {} callable definitions — traced from one of them. Candidate sites:",
            source,
            output.candidates.len()
        );
        for c in &output.candidates {
            let file = c.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let line = c.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("  {file}:{line}");
        }
    }
    Ok(())
}

/// Plain-text/mermaid trace fallback renderer (source kind mismatch). The
/// core decided the fallback; for text the adapter re-runs
/// `detect_fallback` (cheap indexed lookup) to print the source
/// definition list with a "Source definitions:" heading.
fn render_trace_fallback_text(source: &str, store: &Store<ReadOnly>) -> Result<()> {
    // Re-detect the source kind so the text renderer covers the same shapes
    // the core's JSON path does: the four FallbackKinds plus the
    // trace-specific `Kind::Other`. One indexed read returns both the
    // resolution and the summaries the renderer prints (DS-V1.40-8/10).
    let (source_resolution, chunks) = match cqs::kind::detect_kind_for_store(store, source) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(error = %e, source, "trace fallback-text kind detection failed");
            return Ok(());
        }
    };
    if let Some(fk) = super::fallback_kind(source_resolution) {
        let text = notes_text::trace(fk);
        let lead = notes_text::trace_lead(fk, source);
        super::render_kind_fallback_text(&lead, &chunks, text.text_redirect, "Source definitions:");
    } else if source_resolution == cqs::kind::KindResolution::Resolved(cqs::kind::Kind::Other) {
        let lead = notes_text::trace_other_lead(source);
        super::render_kind_fallback_text(
            &lead,
            &chunks,
            notes_text::trace_other_redirect(),
            "Source definitions:",
        );
    }
    Ok(())
}

/// Generate mermaid node ID from index (A, B, C, ..., Z, A1, B1, ...)
fn node_letter(i: usize) -> String {
    let letter = (b'A' + (i % 26) as u8) as char;
    if i < 26 {
        letter.to_string()
    } else {
        format!("{}{}", letter, i / 26)
    }
}

/// Escape characters that are special in Mermaid labels
fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Default maximum nodes in trace BFS traversal.
const DEFAULT_TRACE_MAX_NODES: usize = 10_000;

/// Returns the trace BFS node cap, reading `CQS_TRACE_MAX_NODES` once on first call.
///
/// Resolved at the adapter boundary (CLI `cmd_trace`, daemon `dispatch_trace`)
/// and threaded into [`TraceArgs::max_nodes`] so the core stays env-free. Also
/// serves as the `#[serde(default)]` for `max_nodes` when a wire caller omits it.
pub(crate) fn trace_max_nodes() -> usize {
    use std::sync::OnceLock;
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| match std::env::var("CQS_TRACE_MAX_NODES") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(
                    cap = n,
                    "Trace BFS node cap overridden via CQS_TRACE_MAX_NODES"
                );
                n
            }
            _ => {
                tracing::warn!(
                    val,
                    "CQS_TRACE_MAX_NODES invalid, using default {DEFAULT_TRACE_MAX_NODES}"
                );
                DEFAULT_TRACE_MAX_NODES
            }
        },
        Err(_) => DEFAULT_TRACE_MAX_NODES,
    })
}

/// BFS shortest path through forward adjacency list.
/// Capped at `CQS_TRACE_MAX_NODES` (default 10,000) visited nodes to prevent
/// OOM on dense graphs.
///
/// Predecessor encoding is `Option<String>` rather than `String` because
/// the call graph can legitimately contain empty `caller_name` values
/// (anonymous closures, expression chunks where the parent chunk has
/// `name = ""`). `None` is the unambiguous source marker; `Some("")` is a
/// real-but-nameless predecessor and the chain walks through it correctly.
/// Using `String::new()` as the source-sentinel would make a mid-graph
/// anonymous predecessor terminate chain reconstruction early and silently
/// truncate paths.
pub(crate) fn bfs_shortest_path(
    forward: &HashMap<std::sync::Arc<str>, Vec<std::sync::Arc<str>>>,
    source: &str,
    target: &str,
    max_depth: usize,
    max_nodes: usize,
) -> Option<Vec<String>> {
    let mut visited: HashMap<String, Option<String>> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(source.to_string(), None);
    queue.push_back((source.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if current == target {
            let mut path = vec![current.clone()];
            let mut node = current.clone();
            while let Some(Some(pred)) = visited.get(&node).cloned() {
                path.push(pred.clone());
                node = pred;
            }
            path.reverse();
            return Some(path);
        }
        if visited.len() >= max_nodes {
            tracing::warn!(max_nodes, "BFS trace capped — graph too dense");
            break;
        }
        if depth >= max_depth {
            continue;
        }

        if let Some(callees) = forward.get(current.as_str()) {
            for callee in callees {
                if !visited.contains_key(callee.as_ref()) {
                    visited.insert(callee.to_string(), Some(current.clone()));
                    queue.push_back((callee.to_string(), depth + 1));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A wire caller can supply just `source`/`target` and inherit the defaults.
    #[test]
    fn trace_args_deserialize_minimal() {
        let args: TraceArgs = serde_json::from_str(r#"{"source":"a","target":"b"}"#).unwrap();
        assert_eq!(args.source, "a");
        assert_eq!(args.target, "b");
        assert_eq!(
            args.max_depth,
            crate::cli::args::DEFAULT_DEPTH_TRACE as usize
        );
        assert_eq!(args.max_nodes, trace_max_nodes());
    }

    /// Convert a `HashMap<String, Vec<String>>` to `HashMap<Arc<str>, Vec<Arc<str>>>` for tests.
    fn arc_map(m: HashMap<String, Vec<String>>) -> HashMap<Arc<str>, Vec<Arc<str>>> {
        m.into_iter()
            .map(|(k, vs)| {
                let k: Arc<str> = Arc::from(k.as_str());
                let vs: Vec<Arc<str>> = vs.into_iter().map(|v| Arc::from(v.as_str())).collect();
                (k, vs)
            })
            .collect()
    }

    // ===== node_letter tests =====

    #[test]
    fn test_node_letter_a_to_z() {
        assert_eq!(node_letter(0), "A");
        assert_eq!(node_letter(1), "B");
        assert_eq!(node_letter(25), "Z");
    }

    #[test]
    fn test_node_letter_beyond_z() {
        // After Z: A1, B1, ...
        assert_eq!(node_letter(26), "A1");
        assert_eq!(node_letter(27), "B1");
        assert_eq!(node_letter(51), "Z1");
        assert_eq!(node_letter(52), "A2");
    }

    // ===== mermaid_escape tests =====

    #[test]
    fn test_mermaid_escape_quotes() {
        assert_eq!(mermaid_escape("hello \"world\""), "hello &quot;world&quot;");
    }

    #[test]
    fn test_mermaid_escape_angle_brackets() {
        assert_eq!(mermaid_escape("Vec<T>"), "Vec&lt;T&gt;");
    }

    #[test]
    fn test_mermaid_escape_plain() {
        assert_eq!(mermaid_escape("simple_name"), "simple_name");
    }

    // ===== bfs_shortest_path tests =====

    #[test]
    fn test_bfs_direct_path() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "B", 10, 10_000);
        assert!(result.is_some());
        let path = result.unwrap();
        assert_eq!(path, vec!["A", "B"]);
    }

    #[test]
    fn test_bfs_no_path() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "C", 10, 10_000);
        assert!(result.is_none(), "No path from A to C");
    }

    #[test]
    fn test_bfs_respects_max_depth() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        forward.insert("B".to_string(), vec!["C".to_string()]);
        forward.insert("C".to_string(), vec!["D".to_string()]);
        let forward = arc_map(forward);
        // Path A->B->C->D exists but depth=2 should not reach D
        let result = bfs_shortest_path(&forward, "A", "D", 2, 10_000);
        assert!(result.is_none(), "Should not find path beyond max_depth=2");
    }

    #[test]
    fn test_bfs_multi_hop() {
        let mut forward = HashMap::new();
        forward.insert("A".to_string(), vec!["B".to_string()]);
        forward.insert("B".to_string(), vec!["C".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "C", 10, 10_000);
        assert!(result.is_some());
        let path = result.unwrap();
        assert_eq!(path, vec!["A", "B", "C"]);
    }

    /// Anonymous nodes (`name = ""`, common for closure / expression
    /// chunks) along the BFS path must not be confused with the
    /// source-sentinel during path reconstruction. The predecessor is
    /// encoded as `Option<String>` so `None` is unambiguously the source
    /// and `Some("")` is a real-but-nameless predecessor that the chain
    /// walks through.
    #[test]
    fn test_bfs_path_walks_through_empty_named_node() {
        let mut forward = HashMap::new();
        // Source A → anonymous "" → target Z.
        forward.insert("A".to_string(), vec!["".to_string()]);
        forward.insert("".to_string(), vec!["Z".to_string()]);
        let forward = arc_map(forward);
        let result = bfs_shortest_path(&forward, "A", "Z", 10, 10_000);
        assert!(
            result.is_some(),
            "BFS through anonymous mid-chain node must find Z"
        );
        let path = result.unwrap();
        assert_eq!(
            path,
            vec!["A", "", "Z"],
            "path reconstruction must include the empty-named node, \
             not stop at it as if it were the source"
        );
    }

    // ===== TraceOutput serialization tests =====

    #[test]
    fn test_trace_output_not_found() {
        let output = TraceOutput {
            source: "a".into(),
            target: "b".into(),
            path: None,
            depth: None,
            found: false,
            candidates: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["found"], false);
        assert!(json.get("path").is_none());
        assert!(json.get("depth").is_none());
        // Skip-when-empty: no candidates on the unambiguous case.
        assert!(json.get("candidates").is_none());
    }

    #[test]
    fn test_trace_output_found() {
        let output = TraceOutput {
            source: "a".into(),
            target: "c".into(),
            path: Some(vec![
                TraceHop {
                    name: "a".into(),
                    file: "src/a.rs".into(),
                    line_start: 1,
                    signature: "fn a()".into(),
                },
                TraceHop {
                    name: "b".into(),
                    file: "src/b.rs".into(),
                    line_start: 10,
                    signature: "fn b()".into(),
                },
                TraceHop {
                    name: "c".into(),
                    file: "src/c.rs".into(),
                    line_start: 20,
                    signature: "fn c()".into(),
                },
            ]),
            depth: Some(2),
            found: true,
            candidates: Vec::new(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["found"], true);
        assert_eq!(json["depth"], 2);
        assert_eq!(json["path"][0]["line_start"], 1); // was "line"
        assert!(json["path"][0].get("line").is_none());
    }

    fn make_const_chunk(name: &str, line: u32) -> cqs::store::ChunkSummary {
        cqs::store::ChunkSummary {
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

    // The trace kind fallback routes through the shared
    // `chunks_to_definitions`, capping entry count and truncating oversized
    // content so a hot name can't emit unbounded JSON.
    #[test]
    fn test_trace_fallback_caps_definitions_count() {
        use super::super::{chunks_to_definitions, KIND_FALLBACK_MAX_DEFINITIONS};
        let chunks: Vec<cqs::store::ChunkSummary> = (0..(KIND_FALLBACK_MAX_DEFINITIONS + 50))
            .map(|i| make_const_chunk(&format!("X{i}"), i as u32))
            .collect();
        let defs = chunks_to_definitions(&chunks);
        assert_eq!(defs.len(), KIND_FALLBACK_MAX_DEFINITIONS);
    }

    #[test]
    fn test_trace_fallback_truncates_oversized_content() {
        use super::super::{chunks_to_definitions, KIND_FALLBACK_MAX_CONTENT_BYTES};
        let mut big = make_const_chunk("BIG", 1);
        big.content = "x".repeat(KIND_FALLBACK_MAX_CONTENT_BYTES * 2);
        let defs = chunks_to_definitions(&[big]);
        let content = defs[0]["content"].as_str().unwrap();
        assert!(content.ends_with("... (truncated)"));
        assert_eq!(defs[0]["truncated"], true);
    }

    /// Seed a single chunk of a chosen `chunk_type` and run `trace_core`
    /// against it as the source — exercising the kind-routing branch.
    fn trace_core_for_source_kind(chunk_type: cqs::parser::ChunkType) -> TraceCoreOutput {
        use cqs::store::Store;

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let mock_embedding = cqs::Embedding::new(emb_vec);

        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&db).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let content = if matches!(chunk_type, cqs::parser::ChunkType::Macro) {
            "macro_rules! my_macro { () => {} }"
        } else {
            "fn my_macro() {}"
        };
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let chunk = cqs::parser::Chunk {
            id: format!("src/lib.rs:1:{}", &hash[..8]),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: cqs::parser::Language::Rust,
            chunk_type,
            name: "my_macro".to_string(),
            signature: "macro_rules! my_macro".to_string(),
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

        // Reopen read-only — `trace_core` takes a `Store<ReadOnly>`.
        let store = Store::open_readonly(&db).unwrap();
        let graph = store.get_call_graph().unwrap();
        let args = TraceArgs {
            source: "my_macro".to_string(),
            target: "anything".to_string(),
            max_depth: 5,
            max_nodes: 10_000,
        };
        trace_core(&store, &graph, dir.path(), &args).unwrap()
    }

    /// A `Kind::Other` source (macro / impl / service) must short-circuit to
    /// the trace-specific `other` fallback instead of silently running the
    /// BFS to "no path found". Pins the fallback shape.
    #[test]
    fn trace_other_kind_source_emits_other_fallback() {
        let out = trace_core_for_source_kind(cqs::parser::ChunkType::Macro);
        match out {
            TraceCoreOutput::Fallback(fb) => {
                assert_eq!(fb.kind, "other", "macro source must label as other");
                assert_eq!(fb.fallback_from, "trace");
                assert_eq!(fb.source, "my_macro");
                assert_eq!(fb.definitions.len(), 1);
                assert!(!fb.note.is_empty());
            }
            TraceCoreOutput::Trace(t) => {
                panic!("expected Other-kind fallback, got a trace result: {t:?}")
            }
        }
    }
}
