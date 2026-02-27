//! Context command — module-level understanding
//!
//! Core logic is in shared functions (`build_compact_data`, `build_full_data`,
//! `compact_to_json`, `full_to_json`) so batch mode can reuse them without
//! duplicating ~120 lines.

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use cqs::store::{ChunkSummary, Store};

use crate::cli::staleness;

// ─── Shared core ────────────────────────────────────────────────────────────

/// Compact mode data: signatures + caller/callee counts per chunk.
pub(crate) struct CompactData {
    pub chunks: Vec<ChunkSummary>,
    pub caller_counts: HashMap<String, u64>,
    pub callee_counts: HashMap<String, u64>,
}

/// Build compact-mode data: chunks with caller/callee counts.
pub(crate) fn build_compact_data(store: &Store, path: &str) -> Result<CompactData> {
    let chunks = store.get_chunks_by_origin(path)?;
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

/// Serialize compact data to JSON.
pub(crate) fn compact_to_json(data: &CompactData, path: &str) -> serde_json::Value {
    let entries: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            let cc = data.caller_counts.get(&c.name).copied().unwrap_or(0);
            let ce = data.callee_counts.get(&c.name).copied().unwrap_or(0);
            serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "lines": [c.line_start, c.line_end],
                "caller_count": cc,
                "callee_count": ce,
            })
        })
        .collect();
    serde_json::json!({
        "file": path,
        "chunk_count": data.chunks.len(),
        "chunks": entries,
    })
}

/// Full mode data: chunks with external callers, callees, and dependent files.
pub(crate) struct FullData {
    pub chunks: Vec<ChunkSummary>,
    /// (caller_name, caller_file_rel, callee_name, line)
    pub external_callers: Vec<(String, String, String, u32)>,
    /// (callee_name, called_from)
    pub external_callees: Vec<(String, String)>,
    pub dependent_files: HashSet<String>,
}

/// Build full-mode data: chunks with external callers/callees/dependent files.
///
/// Shared between CLI summary mode (uses counts) and full mode (uses details).
pub(crate) fn build_full_data(store: &Store, path: &str, root: &Path) -> Result<FullData> {
    let chunks = store.get_chunks_by_origin(path)?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    let chunk_names: HashSet<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let names_vec: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();

    // Batch-fetch callers and callees for all chunks
    let callers_by_callee = store
        .get_callers_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callers for context");
            HashMap::new()
        });
    let callees_by_caller = store
        .get_callees_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callees for context");
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
            let caller_origin = caller.file.to_string_lossy().to_string();
            if !caller_origin.ends_with(path) {
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
    })
}

/// Serialize full data to JSON, optionally including content within a token budget.
///
/// When `content_set` is `Some`, only chunks whose names are in the set include content.
/// When `None`, no content is included.
pub(crate) fn full_to_json(
    data: &FullData,
    path: &str,
    content_set: Option<&HashSet<String>>,
    token_info: Option<(usize, usize)>,
) -> serde_json::Value {
    let chunks_json: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            let mut obj = serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "lines": [c.line_start, c.line_end],
                "doc": c.doc,
            });
            if let Some(included) = content_set {
                if included.contains(&c.name) {
                    obj["content"] = serde_json::json!(c.content);
                }
            }
            obj
        })
        .collect();
    let callers_json: Vec<_> = data
        .external_callers
        .iter()
        .map(|(name, file, calls, line)| {
            serde_json::json!({"caller": name, "caller_file": file, "calls": calls, "line": line})
        })
        .collect();
    let callees_json: Vec<_> = data
        .external_callees
        .iter()
        .map(|(name, from)| serde_json::json!({"callee": name, "called_from": from}))
        .collect();
    let mut dep_files: Vec<String> = data.dependent_files.iter().cloned().collect();
    dep_files.sort();

    let mut output = serde_json::json!({
        "file": path,
        "chunks": chunks_json,
        "external_callers": callers_json,
        "external_callees": callees_json,
        "dependent_files": dep_files,
    });
    if let Some((used, budget)) = token_info {
        output["token_count"] = serde_json::json!(used);
        output["token_budget"] = serde_json::json!(budget);
    }
    output
}

/// Pack chunks by relevance (caller count descending) within a token budget.
///
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
    let token_counts = super::count_tokens_batch(embedder, &texts);

    let (packed, used) = super::token_pack(indexed, &token_counts, budget, 0, |&(_, cc)| cc as f32);

    let included: HashSet<String> = packed
        .into_iter()
        .map(|(i, _)| chunks[i].name.clone())
        .collect();
    (included, used)
}

// ─── CLI command ────────────────────────────────────────────────────────────

pub(crate) fn cmd_context(
    cli: &crate::cli::Cli,
    path: &str,
    json: bool,
    summary: bool,
    compact: bool,
    max_tokens: Option<usize>,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_context", path, ?max_tokens).entered();
    let (store, root, _) = crate::cli::open_project_store()?;

    // --tokens is incompatible with --compact and --summary (those modes are deliberately minimal)
    if max_tokens.is_some() && (compact || summary) {
        bail!("--tokens cannot be used with --compact or --summary");
    }

    // Compact mode: signatures-only TOC with caller/callee counts
    if compact {
        let data = build_compact_data(&store, path)?;

        // Proactive staleness warning
        if !cli.quiet && !cli.no_stale_check {
            staleness::warn_stale_results(&store, &[path], &root);
        }

        if json {
            let output = compact_to_json(&data, path);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            print_compact_terminal(&data, path);
        }
        return Ok(());
    }

    // Summary and full modes need external caller/callee data
    let data = build_full_data(&store, path, &root)?;

    // Proactive staleness warning
    if !cli.quiet && !cli.no_stale_check {
        staleness::warn_stale_results(&store, &[path], &root);
    }

    if summary {
        if json {
            let output = summary_to_json(&data, path);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            print_summary_terminal(&data, path);
        }
    } else if json {
        let (content_set, token_info) = build_token_pack(&store, &data.chunks, max_tokens)?;
        let output = full_to_json(&data, path, content_set.as_ref(), token_info);
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let (content_set, token_info) = build_token_pack(&store, &data.chunks, max_tokens)?;
        print_full_terminal(&data, path, content_set.as_ref(), token_info);
    }

    Ok(())
}

/// Build token-packed content set if max_tokens is requested.
#[allow(clippy::type_complexity)]
fn build_token_pack(
    store: &Store,
    chunks: &[ChunkSummary],
    max_tokens: Option<usize>,
) -> Result<(Option<HashSet<String>>, Option<(usize, usize)>)> {
    let Some(budget) = max_tokens else {
        return Ok((None, None));
    };
    let embedder = cqs::Embedder::new()?;
    let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let caller_counts = store.get_caller_counts_batch(&names).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to fetch caller counts for token packing");
        HashMap::new()
    });
    let (included, used) = pack_by_relevance(chunks, &caller_counts, budget, &embedder);
    tracing::info!(
        chunks = included.len(),
        tokens = used,
        budget,
        "Token-budgeted context"
    );
    Ok((Some(included), Some((used, budget))))
}

fn summary_to_json(data: &FullData, path: &str) -> serde_json::Value {
    let chunks_summary: Vec<_> = data
        .chunks
        .iter()
        .map(|c| {
            serde_json::json!({"name": c.name, "chunk_type": c.chunk_type.to_string(), "lines": [c.line_start, c.line_end]})
        })
        .collect();
    let mut dep_files: Vec<String> = data.dependent_files.iter().cloned().collect();
    dep_files.sort();
    serde_json::json!({
        "file": path,
        "chunk_count": data.chunks.len(),
        "chunks": chunks_summary,
        "external_caller_count": data.external_callers.len(),
        "external_callee_count": data.external_callees.len(),
        "dependent_files": dep_files,
    })
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
                println!("{}", c.content);
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
}
