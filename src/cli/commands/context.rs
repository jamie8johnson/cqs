//! Context command — module-level understanding

use anyhow::{bail, Result};
use std::collections::HashSet;

use cqs::Store;

use crate::cli::{find_project_root, staleness};

pub(crate) fn cmd_context(
    cli: &crate::cli::Cli,
    path: &str,
    json: bool,
    summary: bool,
    compact: bool,
) -> Result<()> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

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

    // Collect external callers
    let mut external_callers = Vec::new();
    let mut dependent_files: HashSet<String> = HashSet::new();
    for chunk in &chunks {
        let callers = match store.get_callers_full(&chunk.name) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, name = %chunk.name, "Failed to get callers");
                Vec::new()
            }
        };
        for caller in callers {
            let caller_origin = caller.file.to_string_lossy().to_string();
            if caller_origin != origin && !caller_origin.ends_with(path) {
                let rel = caller
                    .file
                    .strip_prefix(&root)
                    .unwrap_or(&caller.file)
                    .to_string_lossy()
                    .replace('\\', "/");
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
        let chunk_file = chunk.file.to_string_lossy();
        let callees = match store.get_callees_full(&chunk.name, Some(&chunk_file)) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, name = %chunk.name, "Failed to get callees");
                Vec::new()
            }
        };
        for (callee_name, _) in callees {
            if !chunk_names.contains(callee_name.as_str())
                && seen_callees.insert(callee_name.clone())
            {
                external_callees.push((callee_name, chunk.name.clone()));
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
        let chunks_json: Vec<_> = chunks
            .iter()
            .map(|c| {
                serde_json::json!({"name": c.name, "chunk_type": c.chunk_type.to_string(), "signature": c.signature, "lines": [c.line_start, c.line_end], "doc": c.doc})
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

        let output = serde_json::json!({"file": path, "chunks": chunks_json, "external_callers": callers_json, "external_callees": callees_json, "dependent_files": dep_files});
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;
        println!("{} {}", "Context for:".cyan(), path.bold());
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
