//! Context command — module-level understanding
//!
//! ## Command-core split (Phase 2b)
//!
//! [`context_core`] is the surface-agnostic JSON producer for all three modes
//! (compact / summary / full). It routes through the shared data builders
//! (`build_compact_data`, `build_full_data`) and the single JSON-schema sources
//! (`compact_to_json`, `summary_to_json`, [`full_to_json`]). Both the CLI
//! ([`cmd_context`] JSON path) and the daemon (`dispatch_context`) drive it, so
//! the wire shape is identical — the daemon's full-context path now carries the
//! same `external_callers` / `external_callees` / `dependent_files` /
//! `injection_flags` / `line_start`/`line_end` shape the CLI always emitted.
//! Reads no env; the embedder for `--tokens` packing is passed by the adapter.

use anyhow::{bail, Context as _, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use cqs::store::{ChunkSummary, ReadOnly, Store};
use cqs::Embedder;

use crate::cli::staleness;

// ─── Args (surface-agnostic, MCP-ready) ──────────────────────────────────────

/// Input for [`context_core`] — the context knobs both the CLI and a future
/// MCP `context` tool deserialize into. Store/root and the optional embedder
/// (for `--tokens` packing) come from the adapter.
///
/// `#[serde(default)]` so a wire caller can supply just `path` and inherit the
/// production defaults. `summary`/`compact` are mutually-exclusive modes (clap
/// enforces; the core treats `compact` as winning if both are somehow set).
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct ContextArgs {
    /// File path (relative to project root) to build context for.
    pub path: String,
    /// Summary mode: aggregated caller/callee counts + chunk TOC.
    pub summary: bool,
    /// Compact mode: signatures-only TOC with per-chunk caller/callee counts.
    pub compact: bool,
    /// Token budget — full mode only; packs chunk content into the budget.
    pub tokens: Option<usize>,
}

// ─── Core ─────────────────────────────────────────────────────────────────────

/// Surface-agnostic JSON core for `cqs context`. Dispatches on mode (compact /
/// summary / full) and returns the assembled `serde_json::Value` from the
/// shared schema sources. The `--tokens` full-mode packing needs an embedder;
/// the adapter supplies one (lazily built CLI-side, cached daemon-side) only
/// when `tokens` is set. Reads no env and never prints.
pub(crate) fn context_core(
    store: &Store<ReadOnly>,
    root: &Path,
    args: &ContextArgs,
    embedder: Option<&Embedder>,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("context_core", path = %args.path).entered();

    // Normalize backslash input from Windows / agent pipelines so the
    // `origin`-column match (forward-slash-normalized) and the JSON `file`
    // field are canonical across surfaces.
    let normalized = cqs::normalize_path(Path::new(&args.path));

    if args.compact {
        let data = build_compact_data(store, &normalized)?;
        return Ok(compact_to_json(&data, &normalized)?);
    }

    let data = build_full_data(store, &normalized, root)?;

    if args.summary {
        return Ok(summary_to_json(&data, &normalized)?);
    }

    // Full mode — optional token-budgeted content.
    let (content_set, token_info) = match (args.tokens, embedder) {
        (Some(budget), Some(emb)) => {
            let names: Vec<&str> = data.chunks.iter().map(|c| c.name.as_str()).collect();
            let caller_counts = store.get_caller_counts_batch(&names).context(
                "Failed to fetch caller counts for token packing — ranking signal required",
            )?;
            let (included, used) = pack_by_relevance(&data.chunks, &caller_counts, budget, emb);
            (Some(included), Some((used, budget)))
        }
        _ => (None, None),
    };

    Ok(full_to_json(
        &data,
        &normalized,
        content_set.as_ref(),
        token_info,
    )?)
}

// ─── Shared core ────────────────────────────────────────────────────────────

/// Compact mode data: signatures + caller/callee counts per chunk.
pub(crate) struct CompactData {
    pub chunks: Vec<ChunkSummary>,
    pub caller_counts: HashMap<String, u64>,
    pub callee_counts: HashMap<String, u64>,
}

/// Build compact-mode data: chunks with caller/callee counts.
pub(crate) fn build_compact_data<Mode>(store: &Store<Mode>, path: &str) -> Result<CompactData> {
    let _span = tracing::info_span!("build_compact_data", path).entered();
    // Normalize backslash input from Windows / agent pipelines.
    // `get_chunks_by_origin` matches on the stored `origin` column which is
    // forward-slash-normalized; unnormalized `src\foo.rs` silently returns empty.
    let normalized = cqs::normalize_path(Path::new(path));
    let chunks = store
        .get_chunks_by_origin(&normalized)
        .context("Failed to load chunks for file")?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }
    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let caller_counts = store.get_caller_counts_batch(&names)?;
    let callee_counts = store.get_callee_counts_batch(&names)?;
    Ok(CompactData {
        chunks,
        caller_counts,
        callee_counts,
    })
}

/// Typed output for compact context mode.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CompactOutput<'a> {
    pub file: &'a str,
    pub chunk_count: usize,
    pub chunks: Vec<CompactChunkEntry>,
}

/// A single chunk in compact context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CompactChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub caller_count: u64,
    pub callee_count: u64,
    /// Every chunk-returning JSON output must carry a trust_level.
    /// `cqs context --compact` reads from the project store only; always
    /// "user-code".
    pub trust_level: &'static str,
    /// Per-chunk injection-heuristic flags. The schema-stability contract
    /// requires the field be present.
    pub injection_flags: Vec<String>,
}

/// Serialize compact data to JSON.
///
/// Returns a `Result` so a `Serialize` impl bug surfaces as an error
/// rather than coercing to `{}` and a tracing warn the caller can't see.
pub(crate) fn compact_to_json(
    data: &CompactData,
    path: &str,
) -> Result<serde_json::Value, serde_json::Error> {
    let entries: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            let cc = data.caller_counts.get(&c.name).copied().unwrap_or(0);
            let ce = data.callee_counts.get(&c.name).copied().unwrap_or(0);
            CompactChunkEntry {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                signature: c.signature.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                caller_count: cc,
                callee_count: ce,
                trust_level: "user-code",
                // Compact mode relays only the signature (no doc / content),
                // so the scan is scoped to what it actually emits.
                injection_flags: cqs::llm::validation::detect_all_injection_patterns(&c.signature)
                    .into_iter()
                    .map(String::from)
                    .collect(),
            }
        })
        .collect();
    let output = CompactOutput {
        file: path,
        chunk_count: data.chunks.len(),
        chunks: entries,
    };
    serde_json::to_value(&output)
}

/// Full mode data: chunks with external callers, callees, and dependent files.
pub(crate) struct FullData {
    pub chunks: Vec<ChunkSummary>,
    /// (caller_name, caller_file_rel, callee_name, line)
    pub external_callers: Vec<(String, String, String, u32)>,
    /// (callee_name, called_from)
    pub external_callees: Vec<(String, String)>,
    pub dependent_files: HashSet<String>,
    /// Human-readable warnings from store batch failures during
    /// assembly. Populated when `get_callers_full_batch` or
    /// `get_callees_full_batch` fall back to empty maps; surfaces via
    /// `FullOutput`/`SummaryOutput` so JSON consumers can distinguish
    /// "no external callers" from "batch query failed silently".
    pub warnings: Vec<String>,
}

/// Build full-mode data: chunks with external callers/callees/dependent files.
/// Shared between CLI summary mode (uses counts) and full mode (uses details).
pub(crate) fn build_full_data<Mode>(
    store: &Store<Mode>,
    path: &str,
    root: &Path,
) -> Result<FullData> {
    let _span = tracing::info_span!("build_full_data", path).entered();
    // Normalize backslash input from Windows / agent pipelines.
    // `get_chunks_by_origin` matches on the stored `origin` column which is
    // forward-slash-normalized; unnormalized `src\foo.rs` silently returns empty.
    let normalized = cqs::normalize_path(Path::new(path));
    let chunks = store
        .get_chunks_by_origin(&normalized)
        .context("Failed to load chunks for file")?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    let chunk_names: HashSet<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let names_vec: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();

    // Batch-fetch callers and callees for all chunks.
    // Collect warnings on fallback so the JSON consumer can
    // distinguish "no external callers" from "the batch query failed".
    let mut warnings: Vec<String> = Vec::new();
    let callers_by_callee = store
        .get_callers_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callers for context");
            warnings.push(format!(
                "get_callers_full_batch failed: {e}; external_callers may be incomplete"
            ));
            HashMap::new()
        });
    let callees_by_caller = store
        .get_callees_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callees for context");
            warnings.push(format!(
                "get_callees_full_batch failed: {e}; external_callees may be incomplete"
            ));
            HashMap::new()
        });

    // Collect external callers
    let mut external_callers = Vec::new();
    let mut dependent_files: HashSet<String> = HashSet::new();
    for chunk in &chunks {
        let callers = callers_by_callee
            .get(&chunk.name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for caller in callers {
            // Normalize caller_origin and compare against the
            // slash-normalized user path; otherwise Windows backslash input
            // mis-classifies in-file callers as external.
            let caller_origin = cqs::normalize_path(&caller.file);
            if !caller_origin.ends_with(normalized.as_str()) {
                let rel = cqs::rel_display(&caller.file, root);
                external_callers.push((
                    caller.name.clone(),
                    rel.clone(),
                    chunk.name.clone(),
                    caller.line,
                ));
                dependent_files.insert(rel);
            }
        }
    }

    // Collect external callees
    let mut external_callees = Vec::new();
    let mut seen_callees: HashSet<String> = HashSet::new();
    for chunk in &chunks {
        let callees = callees_by_caller
            .get(&chunk.name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for (callee_name, _) in callees {
            if !chunk_names.contains(callee_name.as_str())
                && seen_callees.insert(callee_name.clone())
            {
                external_callees.push((callee_name.clone(), chunk.name.clone()));
            }
        }
    }

    Ok(FullData {
        chunks,
        external_callers,
        external_callees,
        dependent_files,
        warnings,
    })
}

/// Typed output for full context mode.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FullOutput<'a> {
    pub file: &'a str,
    pub chunks: Vec<FullChunkEntry>,
    pub external_callers: Vec<ExternalCallerEntry>,
    pub external_callees: Vec<ExternalCalleeEntry>,
    pub dependent_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<usize>,
    /// Partial-data warnings from batch store failures.
    /// Omitted when empty so the normal happy-path output is unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A chunk in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct FullChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Every chunk-returning JSON output must carry a
    /// trust_level. `cqs context` reads from the project store only;
    /// always "user-code". SECURITY.md mitigation contract.
    pub trust_level: &'static str,
    /// Per-chunk injection-heuristic flags scanned over exactly the relayed
    /// surfaces: doc + signature always, content only when it is emitted on
    /// this chunk (token-budgeted). The schema-stability contract requires the
    /// field be present and an empty `Vec<String>` reflects "no heuristics
    /// fired".
    pub injection_flags: Vec<String>,
}

/// An external caller in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ExternalCallerEntry {
    pub name: String,
    pub file: String,
    pub calls: String,
    pub line_start: u32,
}

/// An external callee in full context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ExternalCalleeEntry {
    pub name: String,
    pub called_from: String,
}

/// Scan exactly the relayed surfaces of a chunk for injection heuristics.
/// `cqs context` (full mode) relays `doc` and `signature` verbatim always, and
/// `content` only when it fits the token budget (`content_relayed`). A payload
/// in any emitted surface must surface in `injection_flags`, so the scan covers
/// doc + signature always and content only when it is relayed — flagging
/// un-relayed content would over-report on a response that never carried it.
/// SECURITY.md promises the flags fire on every chunk-returning JSON output
/// whenever a heuristic matched on a relayed surface — doc-borne payloads were
/// the RT-RELAY gap.
fn scan_chunk_injection_flags(chunk: &ChunkSummary, content_relayed: bool) -> Vec<String> {
    let doc = chunk.doc.as_deref().unwrap_or("");
    let scan_text = if content_relayed {
        format!("{doc}\n{}\n{}", chunk.signature, chunk.content)
    } else {
        format!("{doc}\n{}", chunk.signature)
    };
    cqs::llm::validation::detect_all_injection_patterns(&scan_text)
        .into_iter()
        .map(String::from)
        .collect()
}

/// Serialize full data to JSON, optionally including content within a token budget.
/// When `content_set` is `Some`, only chunks whose names are in the set include content.
/// When `None`, no content is included.
pub(crate) fn full_to_json(
    data: &FullData,
    path: &str,
    content_set: Option<&HashSet<String>>,
    token_info: Option<(usize, usize)>,
) -> Result<serde_json::Value, serde_json::Error> {
    let chunks: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            let content =
                content_set.and_then(|set| set.contains(&c.name).then(|| c.content.clone()));
            // Scan exactly the relayed surfaces: doc + signature always, and
            // content only when it is emitted on this chunk (in the budgeted
            // content set). A doc-borne payload fires even when `content` is
            // omitted; an un-relayed content is not scanned so the flags never
            // over-report.
            let injection_flags = scan_chunk_injection_flags(c, content.is_some());
            FullChunkEntry {
                name: c.name.clone(),
                chunk_type: c.chunk_type.to_string(),
                signature: c.signature.clone(),
                line_start: c.line_start,
                line_end: c.line_end,
                doc: c.doc.clone(),
                content,
                trust_level: "user-code",
                injection_flags,
            }
        })
        .collect();
    let callers: Vec<_> = data
        .external_callers
        .iter()
        .map(|(caller_name, file, calls, line)| ExternalCallerEntry {
            name: caller_name.clone(),
            file: file.clone(),
            calls: calls.clone(),
            line_start: *line,
        })
        .collect();
    let callees: Vec<_> = data
        .external_callees
        .iter()
        .map(|(callee_name, from)| ExternalCalleeEntry {
            name: callee_name.clone(),
            called_from: from.clone(),
        })
        .collect();
    let mut dep_files: Vec<String> = data.dependent_files.iter().cloned().collect();
    dep_files.sort();

    let output = FullOutput {
        file: path,
        chunks,
        external_callers: callers,
        external_callees: callees,
        dependent_files: dep_files,
        token_count: token_info.map(|(used, _)| used),
        token_budget: token_info.map(|(_, budget)| budget),
        warnings: data.warnings.clone(),
    };
    // Surface Serialize bugs as Err rather than coerce to `{}`.
    serde_json::to_value(&output)
}

/// Pack chunks by relevance (caller count descending) within a token budget.
/// Returns the set of included chunk names and total tokens used.
pub(crate) fn pack_by_relevance(
    chunks: &[ChunkSummary],
    caller_counts: &HashMap<String, u64>,
    budget: usize,
    embedder: &cqs::Embedder,
) -> (HashSet<String>, usize) {
    let _pack_span = tracing::info_span!("token_pack_context", budget).entered();

    // Build (index, caller_count) pairs for token_pack to sort by
    let indexed: Vec<(usize, u64)> = (0..chunks.len())
        .map(|i| {
            let cc = caller_counts.get(&chunks[i].name).copied().unwrap_or(0);
            (i, cc)
        })
        .collect();
    let texts: Vec<&str> = indexed
        .iter()
        .map(|&(i, _)| chunks[i].content.as_str())
        .collect();
    let token_counts = crate::cli::commands::count_tokens_batch(embedder, &texts);

    let (packed, used) =
        crate::cli::commands::token_pack(indexed, &token_counts, budget, 0, |&(_, cc)| cc as f32);

    let included: HashSet<String> = packed
        .into_iter()
        .map(|(i, _)| chunks[i].name.clone())
        .collect();
    (included, used)
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_context(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    path: &str,
    json: bool,
    summary: bool,
    compact: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_context", path, ?max_tokens).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // --tokens is incompatible with --compact and --summary (those modes are deliberately minimal)
    if max_tokens.is_some() && (compact || summary) {
        bail!("--tokens cannot be used with --compact or --summary");
    }

    // JSON path routes through the shared `context_core` (same code the daemon
    // runs), so the wire shape is identical across surfaces. The embedder for
    // full-mode `--tokens` packing is built lazily here only when needed.
    if json {
        // Proactive staleness warning (surface I/O — adapter owns it).
        if !ctx.cli.quiet && !ctx.cli.no_stale_check {
            staleness::warn_stale_results(store, &[path], root);
        }
        let embedder = if max_tokens.is_some() && !compact && !summary {
            Some(cqs::Embedder::new(ctx.model_config().clone())?)
        } else {
            None
        };
        let args = ContextArgs {
            path: path.to_string(),
            summary,
            compact,
            tokens: max_tokens,
        };
        let output = context_core(store, root, &args, embedder.as_ref())?;
        crate::cli::json_envelope::emit_json(&output)?;
        return Ok(());
    }

    // Text path keeps the raw data structs for rendering.

    // Compact mode: signatures-only TOC with caller/callee counts
    if compact {
        let data = build_compact_data(store, path)?;

        // Proactive staleness warning
        if !ctx.cli.quiet && !ctx.cli.no_stale_check {
            staleness::warn_stale_results(store, &[path], root);
        }

        print_compact_terminal(&data, path);
        return Ok(());
    }

    // Summary and full modes need external caller/callee data
    let data = build_full_data(store, path, root)?;

    // Proactive staleness warning
    if !ctx.cli.quiet && !ctx.cli.no_stale_check {
        staleness::warn_stale_results(store, &[path], root);
    }

    if summary {
        print_summary_terminal(&data, path);
    } else {
        let (content_set, token_info) =
            build_token_pack(store, &data.chunks, max_tokens, ctx.model_config())?;
        print_full_terminal(&data, path, content_set.as_ref(), token_info);
    }

    Ok(())
}

/// Build token-packed content set if max_tokens is requested.
#[allow(clippy::type_complexity)]
fn build_token_pack<Mode>(
    store: &Store<Mode>,
    chunks: &[ChunkSummary],
    max_tokens: Option<usize>,
    model_config: &cqs::embedder::ModelConfig,
) -> Result<(Option<HashSet<String>>, Option<(usize, usize)>)> {
    let Some(budget) = max_tokens else {
        return Ok((None, None));
    };
    let embedder = cqs::Embedder::new(model_config.clone())?;
    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    // Propagate the batch failure rather than silently degrading
    // ranking to file-order. Token-packing without the caller-count
    // signal produces a worse result than failing the command.
    let caller_counts = store
        .get_caller_counts_batch(&names)
        .context("Failed to fetch caller counts for token packing — ranking signal required")?;
    let (included, used) = pack_by_relevance(chunks, &caller_counts, budget, &embedder);
    tracing::info!(
        chunks = included.len(),
        tokens = used,
        budget,
        "Token-budgeted context"
    );
    Ok((Some(included), Some((used, budget))))
}

/// Typed output for summary context mode.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SummaryOutput<'a> {
    pub file: &'a str,
    pub chunk_count: usize,
    pub chunks: Vec<SummaryChunkEntry>,
    pub external_caller_count: usize,
    pub external_callee_count: usize,
    pub dependent_files: Vec<String>,
    /// Partial-data warnings from batch store failures in
    /// `build_full_data`. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A chunk in summary context output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct SummaryChunkEntry {
    pub name: String,
    pub chunk_type: String,
    pub line_start: u32,
    pub line_end: u32,
    /// Every chunk-returning JSON output must carry a trust_level.
    /// `cqs context --summary` reads from the project store only; always
    /// "user-code".
    pub trust_level: &'static str,
    /// Per-chunk injection-heuristic flags.
    pub injection_flags: Vec<String>,
}

pub(crate) fn summary_to_json(
    data: &FullData,
    path: &str,
) -> Result<serde_json::Value, serde_json::Error> {
    let chunks: Vec<_> = data
        .chunks
        .iter()
        .map(|c| SummaryChunkEntry {
            name: c.name.clone(),
            chunk_type: c.chunk_type.to_string(),
            line_start: c.line_start,
            line_end: c.line_end,
            trust_level: "user-code",
            injection_flags: Vec::new(),
        })
        .collect();
    let mut dep_files: Vec<String> = data.dependent_files.iter().cloned().collect();
    dep_files.sort();
    let output = SummaryOutput {
        file: path,
        chunk_count: data.chunks.len(),
        chunks,
        external_caller_count: data.external_callers.len(),
        external_callee_count: data.external_callees.len(),
        dependent_files: dep_files,
        warnings: data.warnings.clone(),
    };
    // Surface Serialize bugs as Err rather than coerce to `{}`.
    serde_json::to_value(&output)
}

fn print_compact_terminal(data: &CompactData, path: &str) {
    use colored::Colorize;
    println!("{} ({} chunks)", path.bold(), data.chunks.len());
    for c in &data.chunks {
        let cc = data.caller_counts.get(&c.name).copied().unwrap_or(0);
        let ce = data.callee_counts.get(&c.name).copied().unwrap_or(0);
        let sig = if c.signature.is_empty() {
            c.name.clone()
        } else {
            c.signature.clone()
        };
        let caller_label = if cc == 1 { "caller" } else { "callers" };
        let callee_label = if ce == 1 { "callee" } else { "callees" };
        println!(
            "  {}  [{} {}, {} {}]",
            sig.dimmed(),
            cc,
            caller_label,
            ce,
            callee_label,
        );
    }
}

fn print_summary_terminal(data: &FullData, path: &str) {
    use colored::Colorize;
    println!("{} {}", "Context summary:".cyan(), path.bold());
    println!("  Chunks: {}", data.chunks.len());
    for c in &data.chunks {
        println!(
            "    {} {} (:{}-{})",
            c.chunk_type, c.name, c.line_start, c.line_end
        );
    }
    println!("  External callers: {}", data.external_callers.len());
    println!("  External callees: {}", data.external_callees.len());
    if !data.dependent_files.is_empty() {
        let mut dep_files: Vec<&String> = data.dependent_files.iter().collect();
        dep_files.sort();
        println!("  Dependent files:");
        for f in dep_files {
            println!("    {}", f);
        }
    }
    // Surface partial-data warnings at the bottom.
    for w in &data.warnings {
        println!("{} {}", "Warning:".yellow(), w);
    }
}

fn print_full_terminal(
    data: &FullData,
    path: &str,
    content_set: Option<&HashSet<String>>,
    token_info: Option<(usize, usize)>,
) {
    use colored::Colorize;

    let token_label = match token_info {
        Some((used, budget)) => format!(" ({} of {} tokens)", used, budget),
        None => String::new(),
    };
    println!("{} {}{}", "Context for:".cyan(), path.bold(), token_label);
    println!();

    println!("{}", "Chunks:".cyan());
    for c in &data.chunks {
        println!(
            "  {} {} (:{}-{})",
            c.chunk_type,
            c.name.bold(),
            c.line_start,
            c.line_end
        );
        if !c.signature.is_empty() {
            println!("    {}", c.signature.dimmed());
        }
        // Print content if within token budget
        if let Some(included) = content_set {
            if included.contains(&c.name) {
                println!("{}", "\u{2500}".repeat(50));
                println!("{}", crate::cli::display::sanitize_for_terminal(&c.content));
                println!();
            }
        }
    }

    if !data.external_callers.is_empty() {
        println!();
        println!("{}", "External callers:".cyan());
        for (name, file, calls, line) in &data.external_callers {
            println!("  {} ({}:{}) -> {}", name, file, line, calls);
        }
    }

    if !data.external_callees.is_empty() {
        println!();
        println!("{}", "External callees:".cyan());
        for (name, from) in &data.external_callees {
            println!("  {} <- {}", name, from);
        }
    }

    if !data.dependent_files.is_empty() {
        println!();
        println!("{}", "Dependent files:".cyan());
        let mut files: Vec<&String> = data.dependent_files.iter().collect();
        files.sort();
        for f in files {
            println!("  {}", f);
        }
    }

    // Surface partial-data warnings at the bottom.
    for w in &data.warnings {
        println!("{} {}", "Warning:".yellow(), w);
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::language::{ChunkType, Language};
    use cqs::store::ChunkSummary;
    use std::path::PathBuf;

    /// A wire/MCP caller can supply only `path` and inherit defaults.
    #[test]
    fn context_args_deserialize_minimal() {
        let args: ContextArgs = serde_json::from_str(r#"{"path": "src/lib.rs"}"#).unwrap();
        assert_eq!(args.path, "src/lib.rs");
        assert!(!args.summary);
        assert!(!args.compact);
        assert!(args.tokens.is_none());
    }

    /// `ContextArgs::default` must match the clap `ContextArgs` defaults.
    /// Parses `cqs context <path>` via a throwaway `clap::Parser` wrapper.
    #[test]
    fn context_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::ContextArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-context", "src/lib.rs"])
            .unwrap()
            .args;
        let core = ContextArgs {
            path: clap_args.path.clone(),
            summary: clap_args.summary,
            compact: clap_args.compact,
            tokens: clap_args.tokens,
        };
        let expected = ContextArgs {
            path: "src/lib.rs".to_string(),
            ..ContextArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap context defaults drifted from ContextArgs::default — update both together"
        );
    }

    fn make_chunk(name: &str, line_start: u32, line_end: u32) -> ChunkSummary {
        ChunkSummary {
            id: format!("id_{name}"),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content: format!("fn {name}() {{ }}"),
            doc: Some(format!("Doc for {name}")),
            line_start,
            line_end,
            content_hash: format!("hash_{name}"),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
            parser_version: 0,
            vendored: false,
        }
    }

    // ===== build_compact_data + build_full_data =====
    //
    // These exercise the Store-backed builders (not just the JSON serializers
    // tested above). Inlines a chunk + call-graph fixture because
    // `cqs::test_helpers` is `#[cfg(test)]`-gated in the library crate and
    // unreachable from the bin crate.

    use cqs::embedder::Embedding;
    use cqs::parser::{
        CallEdgeKind as PCallEdgeKind, CallSite as PCallSite, Chunk, ChunkType as PChunkType,
        FunctionCalls, Language as PLanguage,
    };
    use cqs::store::{ModelInfo, Store};
    use std::path::Path;
    use tempfile::TempDir;

    fn make_full_chunk(file: &str, name: &str, line_start: u32) -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("{file}:{line_start}:{name}"),
            file: PathBuf::from(file),
            language: PLanguage::Rust,
            chunk_type: PChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start,
            line_end: line_start + 4,
            byte_start: 0,
            content_hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    fn zero_embedding() -> Embedding {
        let mut v = vec![0.0_f32; cqs::EMBEDDING_DIM];
        v[0] = 1.0;
        Embedding::new(v)
    }

    /// Seeds:
    ///   src/target.rs  — chunk_a (line 1), chunk_b (line 10)
    ///   src/caller.rs  — caller_x (line 1)  → calls chunk_a, chunk_b
    ///   src/other.rs   — chunk_a calls extern_fn (external callee)
    fn seed_context_fixture() -> (Store, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).expect("open");
        store.init(&ModelInfo::default()).expect("init");

        let target_chunks = vec![
            (
                make_full_chunk("src/target.rs", "chunk_a", 1),
                zero_embedding(),
            ),
            (
                make_full_chunk("src/target.rs", "chunk_b", 10),
                zero_embedding(),
            ),
        ];
        store
            .upsert_chunks_batch(&target_chunks, Some(0))
            .expect("upsert target");

        let caller_chunks = vec![(
            make_full_chunk("src/caller.rs", "caller_x", 1),
            zero_embedding(),
        )];
        store
            .upsert_chunks_batch(&caller_chunks, Some(0))
            .expect("upsert caller");

        // caller_x calls chunk_a + chunk_b (external callers of target file)
        store
            .upsert_function_calls(
                Path::new("src/caller.rs"),
                &[FunctionCalls {
                    name: "caller_x".to_string(),
                    line_start: 1,
                    calls: vec![
                        PCallSite {
                            callee_name: "chunk_a".to_string(),
                            line_number: 2,
                            kind: PCallEdgeKind::Call,
                        },
                        PCallSite {
                            callee_name: "chunk_b".to_string(),
                            line_number: 3,
                            kind: PCallEdgeKind::Call,
                        },
                    ],
                }],
            )
            .expect("upsert calls (caller_x)");

        // chunk_a calls extern_fn (external callee for target file)
        store
            .upsert_function_calls(
                Path::new("src/target.rs"),
                &[FunctionCalls {
                    name: "chunk_a".to_string(),
                    line_start: 1,
                    calls: vec![PCallSite {
                        callee_name: "extern_fn".to_string(),
                        line_number: 5,
                        kind: PCallEdgeKind::Call,
                    }],
                }],
            )
            .expect("upsert calls (chunk_a)");

        (store, dir)
    }

    #[test]
    fn build_compact_data_returns_chunks_with_caller_callee_counts() {
        let (store, _dir) = seed_context_fixture();
        let data = build_compact_data(&store, "src/target.rs").expect("build_compact_data");

        let names: Vec<&str> = data.chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"chunk_a"));
        assert!(names.contains(&"chunk_b"));
        assert_eq!(data.chunks.len(), 2);

        // chunk_a: 1 caller (caller_x), 1 callee (extern_fn)
        assert_eq!(data.caller_counts.get("chunk_a").copied().unwrap_or(0), 1);
        assert_eq!(data.callee_counts.get("chunk_a").copied().unwrap_or(0), 1);

        // chunk_b: 1 caller (caller_x), 0 callees
        assert_eq!(data.caller_counts.get("chunk_b").copied().unwrap_or(0), 1);
        assert_eq!(data.callee_counts.get("chunk_b").copied().unwrap_or(0), 0);
    }

    #[test]
    fn build_compact_data_errors_on_unknown_path() {
        let (store, _dir) = seed_context_fixture();
        match build_compact_data(&store, "src/does_not_exist.rs") {
            Ok(_) => panic!("expected error for unindexed path"),
            Err(err) => assert!(
                err.to_string().contains("No indexed chunks found"),
                "expected 'No indexed chunks' error, got: {err}"
            ),
        }
    }

    #[test]
    fn build_compact_data_normalizes_backslash_paths() {
        // `src\target.rs` (Windows / agent shell input) must match the
        // slash-normalized `src/target.rs` stored in `origin`.
        let (store, _dir) = seed_context_fixture();
        let data = build_compact_data(&store, "src\\target.rs").expect("backslash input");
        assert_eq!(data.chunks.len(), 2);
    }

    #[test]
    fn build_full_data_classifies_external_callers_and_callees() {
        let (store, _dir) = seed_context_fixture();
        let root = std::path::Path::new("");
        let data = build_full_data(&store, "src/target.rs", root).expect("build_full_data");

        // chunks contain both target-file functions
        assert_eq!(data.chunks.len(), 2);

        // External callers: caller_x → chunk_a, caller_x → chunk_b (both from src/caller.rs)
        assert_eq!(
            data.external_callers.len(),
            2,
            "expected two external caller edges (caller_x→chunk_a, caller_x→chunk_b), got {:?}",
            data.external_callers
        );
        let caller_names: HashSet<&str> = data
            .external_callers
            .iter()
            .map(|(n, _, _, _)| n.as_str())
            .collect();
        assert!(caller_names.contains("caller_x"));
        let callee_targets: HashSet<&str> = data
            .external_callers
            .iter()
            .map(|(_, _, callee, _)| callee.as_str())
            .collect();
        assert!(callee_targets.contains("chunk_a"));
        assert!(callee_targets.contains("chunk_b"));

        // External callees: extern_fn (called by chunk_a, not in target file)
        assert_eq!(data.external_callees.len(), 1);
        assert_eq!(data.external_callees[0].0, "extern_fn");
        assert_eq!(data.external_callees[0].1, "chunk_a");

        // Dependent files include src/caller.rs (where caller_x lives)
        assert!(
            data.dependent_files.iter().any(|f| f.contains("caller.rs")),
            "dependent_files must surface caller.rs, got {:?}",
            data.dependent_files
        );

        // No batch failures → no warnings
        assert!(data.warnings.is_empty());
    }

    // ===== pack_by_relevance =====
    //
    // Requires a real `cqs::Embedder` (the function takes one to call
    // `count_tokens_batch`). Gated behind `#[ignore]` to match the
    // pipeline tests' "Requires model" pattern.

    #[test]
    #[ignore = "Requires ONNX embedder model on disk"]
    fn pack_by_relevance_prefers_higher_caller_count_within_budget() {
        use cqs::embedder::ModelConfig;
        use cqs::Embedder;

        let embedder = match Embedder::new_cpu(ModelConfig::resolve(None, None)) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("CPU embedder unavailable: {err}; skipping (#1305)");
                return;
            }
        };

        // Three chunks; chunk_hi has a much higher caller count than chunk_lo.
        // With a small budget, packer must pick chunk_hi first.
        let chunks = vec![
            make_chunk("chunk_lo", 1, 5),
            make_chunk("chunk_mid", 6, 10),
            make_chunk("chunk_hi", 11, 15),
        ];
        let mut caller_counts = HashMap::new();
        caller_counts.insert("chunk_lo".to_string(), 1);
        caller_counts.insert("chunk_mid".to_string(), 5);
        caller_counts.insert("chunk_hi".to_string(), 100);

        // Budget large enough for everything → all included.
        let (included_all, used_all) =
            pack_by_relevance(&chunks, &caller_counts, 10_000, &embedder);
        assert_eq!(included_all.len(), 3);
        assert!(used_all > 0, "token usage must be positive");

        // Budget large enough for ~1 chunk → must include chunk_hi.
        // We can't pin the exact byte→token ratio, but we can verify
        // (a) the included set is non-empty, (b) chunk_hi appears whenever
        // anything is included.
        let (included_some, _used_some) = pack_by_relevance(&chunks, &caller_counts, 20, &embedder);
        if !included_some.is_empty() {
            assert!(
                included_some.contains("chunk_hi"),
                "highest caller_count must win priority, got {:?}",
                included_some
            );
        }
    }

    // ===== HP-1: compact_to_json tests =====

    #[test]
    fn hp1_compact_to_json_all_fields() {
        let chunks = vec![make_chunk("foo", 1, 10), make_chunk("bar", 12, 20)];
        let mut caller_counts = HashMap::new();
        caller_counts.insert("foo".to_string(), 3);
        caller_counts.insert("bar".to_string(), 0);
        let mut callee_counts = HashMap::new();
        callee_counts.insert("foo".to_string(), 1);
        callee_counts.insert("bar".to_string(), 5);

        let data = CompactData {
            chunks,
            caller_counts,
            callee_counts,
        };
        let json = compact_to_json(&data, "src/lib.rs").unwrap();

        // Top-level fields
        assert_eq!(json["file"], "src/lib.rs");
        assert_eq!(json["chunk_count"], 2);

        // First chunk: verify all CompactChunkEntry fields
        let c0 = &json["chunks"][0];
        assert_eq!(c0["name"], "foo");
        assert_eq!(c0["chunk_type"], "function");
        assert_eq!(c0["signature"], "fn foo()");
        assert_eq!(c0["line_start"], 1);
        assert_eq!(c0["line_end"], 10);
        assert_eq!(c0["caller_count"], 3);
        assert_eq!(c0["callee_count"], 1);

        // Second chunk: zero callers
        let c1 = &json["chunks"][1];
        assert_eq!(c1["name"], "bar");
        assert_eq!(c1["caller_count"], 0);
        assert_eq!(c1["callee_count"], 5);
    }

    #[test]
    fn hp1_compact_to_json_missing_counts_default_to_zero() {
        // When caller/callee counts maps are empty, should default to 0
        let chunks = vec![make_chunk("orphan", 1, 5)];
        let data = CompactData {
            chunks,
            caller_counts: HashMap::new(),
            callee_counts: HashMap::new(),
        };
        let json = compact_to_json(&data, "src/orphan.rs").unwrap();

        assert_eq!(json["chunks"][0]["caller_count"], 0);
        assert_eq!(json["chunks"][0]["callee_count"], 0);
    }

    // ===== HP-1: full_to_json tests =====

    #[test]
    fn hp1_full_to_json_all_fields() {
        let chunks = vec![make_chunk("process", 5, 25)];
        let external_callers = vec![(
            "main".to_string(),
            "src/main.rs".to_string(),
            "process".to_string(),
            3u32,
        )];
        let external_callees = vec![("validate".to_string(), "process".to_string())];
        let mut dependent_files = HashSet::new();
        dependent_files.insert("src/main.rs".to_string());

        let data = FullData {
            chunks,
            external_callers,
            external_callees,
            dependent_files,
            warnings: Vec::new(),
        };
        let json = full_to_json(&data, "src/lib.rs", None, None).unwrap();

        // Top-level
        assert_eq!(json["file"], "src/lib.rs");

        // Chunks
        let chunks_arr = json["chunks"].as_array().unwrap();
        assert_eq!(chunks_arr.len(), 1);
        let c0 = &chunks_arr[0];
        assert_eq!(c0["name"], "process");
        assert_eq!(c0["chunk_type"], "function");
        assert_eq!(c0["signature"], "fn process()");
        assert_eq!(c0["line_start"], 5);
        assert_eq!(c0["line_end"], 25);
        assert_eq!(c0["doc"], "Doc for process");
        assert!(
            c0.get("content").is_none(),
            "content should be absent when content_set is None"
        );

        // External callers
        let callers = json["external_callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "main");
        assert_eq!(callers[0]["file"], "src/main.rs");
        assert_eq!(callers[0]["calls"], "process");
        assert_eq!(callers[0]["line_start"], 3);

        // External callees
        let callees = json["external_callees"].as_array().unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["name"], "validate");
        assert_eq!(callees[0]["called_from"], "process");

        // Dependent files
        let dep = json["dependent_files"].as_array().unwrap();
        assert_eq!(dep.len(), 1);
        assert_eq!(dep[0], "src/main.rs");

        // Token fields absent when not provided
        assert!(
            json.get("token_count").is_none(),
            "token_count should be absent when token_info is None"
        );
        assert!(
            json.get("token_budget").is_none(),
            "token_budget should be absent when token_info is None"
        );
    }

    #[test]
    fn hp1_full_to_json_with_token_info() {
        let data = FullData {
            chunks: vec![make_chunk("f", 1, 5)],
            external_callers: vec![],
            external_callees: vec![],
            dependent_files: HashSet::new(),
            warnings: Vec::new(),
        };
        let json = full_to_json(&data, "src/lib.rs", None, Some((150, 500))).unwrap();

        assert_eq!(json["token_count"], 150);
        assert_eq!(json["token_budget"], 500);
    }

    #[test]
    fn hp1_full_to_json_with_content_set() {
        let chunks = vec![
            make_chunk("included", 1, 10),
            make_chunk("excluded", 12, 20),
        ];
        let data = FullData {
            chunks,
            external_callers: vec![],
            external_callees: vec![],
            dependent_files: HashSet::new(),
            warnings: Vec::new(),
        };
        let mut content_set = HashSet::new();
        content_set.insert("included".to_string());

        let json = full_to_json(&data, "src/lib.rs", Some(&content_set), None).unwrap();

        let chunks_arr = json["chunks"].as_array().unwrap();
        assert!(
            chunks_arr[0]["content"].is_string(),
            "included chunk should have content"
        );
        assert_eq!(chunks_arr[0]["content"], "fn included() { }");
        assert!(
            chunks_arr[1].get("content").is_none(),
            "excluded chunk should not have content"
        );
    }

    // ===== HP-1: summary_to_json tests =====

    #[test]
    fn hp1_summary_to_json_all_fields() {
        let data = FullData {
            chunks: vec![make_chunk("a", 1, 10), make_chunk("b", 15, 30)],
            external_callers: vec![
                (
                    "caller1".to_string(),
                    "src/c.rs".to_string(),
                    "a".to_string(),
                    5,
                ),
                (
                    "caller2".to_string(),
                    "src/d.rs".to_string(),
                    "b".to_string(),
                    8,
                ),
            ],
            external_callees: vec![("ext_fn".to_string(), "a".to_string())],
            dependent_files: {
                let mut s = HashSet::new();
                s.insert("src/c.rs".to_string());
                s.insert("src/d.rs".to_string());
                s
            },
            warnings: Vec::new(),
        };
        let json = summary_to_json(&data, "src/lib.rs").unwrap();

        assert_eq!(json["file"], "src/lib.rs");
        assert_eq!(json["chunk_count"], 2);
        assert_eq!(json["external_caller_count"], 2);
        assert_eq!(json["external_callee_count"], 1);

        // Chunks have minimal fields
        let chunks = json["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["name"], "a");
        assert_eq!(chunks[0]["chunk_type"], "function");
        assert_eq!(chunks[0]["line_start"], 1);
        assert_eq!(chunks[0]["line_end"], 10);
        // Summary chunks should NOT have signature, doc, content, caller_count etc.
        assert!(chunks[0].get("signature").is_none());
        assert!(chunks[0].get("doc").is_none());
        assert!(chunks[0].get("content").is_none());

        // Dependent files sorted
        let dep = json["dependent_files"].as_array().unwrap();
        assert_eq!(dep.len(), 2);
        assert_eq!(dep[0], "src/c.rs");
        assert_eq!(dep[1], "src/d.rs");
    }

    #[test]
    fn hp1_compact_chunk_count_matches_array_length() {
        // chunk_count should match chunks array length
        let chunks = vec![
            make_chunk("a", 1, 5),
            make_chunk("b", 6, 10),
            make_chunk("c", 11, 15),
        ];
        let data = CompactData {
            chunks,
            caller_counts: HashMap::new(),
            callee_counts: HashMap::new(),
        };
        let json = compact_to_json(&data, "src/lib.rs").unwrap();

        let arr_len = json["chunks"].as_array().unwrap().len();
        let count_field = json["chunk_count"].as_u64().unwrap();
        assert_eq!(
            count_field, arr_len as u64,
            "chunk_count field must equal chunks array length"
        );
    }

    /// SECURITY.md lists `cqs context` (all three shapes) as JSON outputs that
    /// carry `trust_level` and `injection_flags` per chunk. This regression-pin
    /// keeps SECURITY.md honest across `CompactChunkEntry` / `FullChunkEntry` /
    /// `SummaryChunkEntry`; removing the fields would break the contract
    /// silently.
    #[test]
    fn context_chunks_emit_sec_trust_level_and_injection_flags() {
        // Compact shape.
        let compact_data = CompactData {
            chunks: vec![make_chunk("a", 1, 5)],
            caller_counts: HashMap::new(),
            callee_counts: HashMap::new(),
        };
        let compact_json = compact_to_json(&compact_data, "src/lib.rs").unwrap();
        let c = &compact_json["chunks"][0];
        assert_eq!(
            c["trust_level"], "user-code",
            "SECURITY.md:57 promises trust_level on context --compact chunks"
        );
        let cf = c["injection_flags"]
            .as_array()
            .expect("SECURITY.md:57 promises injection_flags on context --compact chunks");
        assert!(cf.is_empty());

        // Full shape.
        let full_data = FullData {
            chunks: vec![make_chunk("b", 1, 5)],
            external_callers: vec![],
            external_callees: vec![],
            dependent_files: HashSet::new(),
            warnings: Vec::new(),
        };
        let full_json = full_to_json(&full_data, "src/lib.rs", None, None).unwrap();
        let f = &full_json["chunks"][0];
        assert_eq!(
            f["trust_level"], "user-code",
            "SECURITY.md:57 promises trust_level on context --full chunks"
        );
        let ff = f["injection_flags"]
            .as_array()
            .expect("SECURITY.md:57 promises injection_flags on context --full chunks");
        assert!(ff.is_empty());

        // Summary shape.
        let summary_json = summary_to_json(&full_data, "src/lib.rs").unwrap();
        let s = &summary_json["chunks"][0];
        assert_eq!(
            s["trust_level"], "user-code",
            "SECURITY.md:57 promises trust_level on context --summary chunks"
        );
        let sf = s["injection_flags"]
            .as_array()
            .expect("SECURITY.md:57 promises injection_flags on context --summary chunks");
        assert!(sf.is_empty());
    }
}
