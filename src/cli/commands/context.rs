//! Context command — module-level understanding

use anyhow::{bail, Result};
use std::collections::HashSet;

use crate::cli::staleness;

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

    let abs_path = root.join(path);
    let origin = abs_path.to_string_lossy().to_string();

    let mut chunks = store.get_chunks_by_origin(&origin)?;
    if chunks.is_empty() {
        chunks = store.get_chunks_by_origin(path)?;
    }
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    // Proactive staleness warning
    if !cli.quiet && !cli.no_stale_check {
        staleness::warn_stale_results(&store, &[&origin], &root);
    }

    // Compact mode: signatures-only TOC with caller/callee counts
    if compact {
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let caller_counts = store.get_caller_counts_batch(&names)?;
        let callee_counts = store.get_callee_counts_batch(&names)?;

        if json {
            let entries: Vec<_> = chunks
                .iter()
                .map(|c| {
                    let cc = caller_counts.get(&c.name).copied().unwrap_or(0);
                    let ce = callee_counts.get(&c.name).copied().unwrap_or(0);
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
            let output = serde_json::json!({
                "file": path,
                "chunk_count": chunks.len(),
                "chunks": entries,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            use colored::Colorize;
            println!("{} ({} chunks)", path.bold(), chunks.len());
            for c in &chunks {
                let cc = caller_counts.get(&c.name).copied().unwrap_or(0);
                let ce = callee_counts.get(&c.name).copied().unwrap_or(0);
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
        return Ok(());
    }

    let chunk_names: HashSet<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let names_vec: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();

    // Batch-fetch callers and callees for all chunks in two queries
    let callers_by_callee = store
        .get_callers_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callers for context");
            std::collections::HashMap::new()
        });
    let callees_by_caller = store
        .get_callees_full_batch(&names_vec)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callees for context");
            std::collections::HashMap::new()
        });

    // Collect external callers from batch results
    let mut external_callers = Vec::new();
    let mut dependent_files: HashSet<String> = HashSet::new();
    for chunk in &chunks {
        let callers = callers_by_callee
            .get(&chunk.name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for caller in callers {
            let caller_origin = caller.file.to_string_lossy().to_string();
            if caller_origin != origin && !caller_origin.ends_with(path) {
                let rel = cqs::rel_display(&caller.file, &root);
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

    // Collect external callees from batch results
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

    if summary {
        let mut dep_files: Vec<String> = dependent_files.into_iter().collect();
        dep_files.sort();

        if json {
            let chunks_summary: Vec<_> = chunks
                .iter()
                .map(|c| {
                    serde_json::json!({"name": c.name, "chunk_type": c.chunk_type.to_string(), "lines": [c.line_start, c.line_end]})
                })
                .collect();
            let output = serde_json::json!({
                "file": path,
                "chunk_count": chunks.len(),
                "chunks": chunks_summary,
                "external_caller_count": external_callers.len(),
                "external_callee_count": external_callees.len(),
                "dependent_files": dep_files,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            use colored::Colorize;
            println!("{} {}", "Context summary:".cyan(), path.bold());
            println!("  Chunks: {}", chunks.len());
            for c in &chunks {
                println!(
                    "    {} {} (:{}-{})",
                    c.chunk_type, c.name, c.line_start, c.line_end
                );
            }
            println!("  External callers: {}", external_callers.len());
            println!("  External callees: {}", external_callees.len());
            if !dep_files.is_empty() {
                println!("  Dependent files:");
                for f in &dep_files {
                    println!("    {}", f);
                }
            }
        }
    } else if json {
        // Token-budgeted content inclusion (sorted by caller count for relevance)
        let (content_map, token_info) = if let Some(budget) = max_tokens {
            let embedder = cqs::Embedder::new()?;
            let caller_counts = store
                .get_caller_counts_batch(&names_vec)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Failed to fetch caller counts for token packing");
                    std::collections::HashMap::new()
                });
            let (included, used) = pack_by_relevance(&chunks, &caller_counts, budget, &embedder);
            tracing::info!(
                chunks = included.len(),
                tokens = used,
                budget,
                "Token-budgeted context"
            );
            (Some(included), Some((used, budget)))
        } else {
            (None, None)
        };

        let chunks_json: Vec<_> = chunks
            .iter()
            .map(|c| {
                let mut obj = serde_json::json!({
                    "name": c.name,
                    "chunk_type": c.chunk_type.to_string(),
                    "signature": c.signature,
                    "lines": [c.line_start, c.line_end],
                    "doc": c.doc,
                });
                if let Some(ref included) = content_map {
                    if included.contains(&c.name) {
                        obj["content"] = serde_json::json!(c.content);
                    }
                }
                obj
            })
            .collect();
        let callers_json: Vec<_> = external_callers
            .iter()
            .map(|(name, file, calls, line)| {
                serde_json::json!({"caller": name, "caller_file": file, "calls": calls, "line": line})
            })
            .collect();
        let callees_json: Vec<_> = external_callees
            .iter()
            .map(|(name, from)| serde_json::json!({"callee": name, "called_from": from}))
            .collect();
        let mut dep_files: Vec<String> = dependent_files.into_iter().collect();
        dep_files.sort();

        let mut output = serde_json::json!({"file": path, "chunks": chunks_json, "external_callers": callers_json, "external_callees": callees_json, "dependent_files": dep_files});
        if let Some((used, budget)) = token_info {
            output["token_count"] = serde_json::json!(used);
            output["token_budget"] = serde_json::json!(budget);
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;

        // Token-budgeted content inclusion (sorted by caller count for relevance)
        let (content_set, token_info) = if let Some(budget) = max_tokens {
            let embedder = cqs::Embedder::new()?;
            let caller_counts = store
                .get_caller_counts_batch(&names_vec)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "Failed to fetch caller counts for token packing");
                    std::collections::HashMap::new()
                });
            let (included, used) = pack_by_relevance(&chunks, &caller_counts, budget, &embedder);
            tracing::info!(
                chunks = included.len(),
                tokens = used,
                budget,
                "Token-budgeted context"
            );
            (Some(included), Some((used, budget)))
        } else {
            (None, None)
        };

        let token_label = match token_info {
            Some((used, budget)) => format!(" ({} of {} tokens)", used, budget),
            None => String::new(),
        };
        println!("{} {}{}", "Context for:".cyan(), path.bold(), token_label);
        println!();

        println!("{}", "Chunks:".cyan());
        for c in &chunks {
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
            if let Some(ref included) = content_set {
                if included.contains(&c.name) {
                    println!("{}", "─".repeat(50));
                    println!("{}", c.content);
                    println!();
                }
            }
        }

        if !external_callers.is_empty() {
            println!();
            println!("{}", "External callers:".cyan());
            for (name, file, calls, line) in &external_callers {
                println!("  {} ({}:{}) -> {}", name, file, line, calls);
            }
        }

        if !external_callees.is_empty() {
            println!();
            println!("{}", "External callees:".cyan());
            for (name, from) in &external_callees {
                println!("  {} <- {}", name, from);
            }
        }

        if !dependent_files.is_empty() {
            println!();
            println!("{}", "Dependent files:".cyan());
            let mut files: Vec<_> = dependent_files.into_iter().collect();
            files.sort();
            for f in &files {
                println!("  {}", f);
            }
        }
    }

    Ok(())
}

/// Pack chunks by relevance (caller count descending) within a token budget.
///
/// Returns the set of included chunk names and total tokens used.
fn pack_by_relevance(
    chunks: &[cqs::store::ChunkSummary],
    caller_counts: &std::collections::HashMap<String, u64>,
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
