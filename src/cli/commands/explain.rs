//! Explain command — generate a function card

use anyhow::{bail, Result};

use cqs::parser::ChunkType;
use cqs::{compute_hints, HnswIndex, SearchFilter, Store};

use crate::cli::{find_project_root, staleness};

use super::resolve::parse_target;

pub(crate) fn cmd_explain(cli: &crate::cli::Cli, target: &str, json: bool) -> Result<()> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

    // Resolve target to chunk
    let (file_filter, name) = parse_target(target);
    let results = store.search_by_name(name, 20)?;
    if results.is_empty() {
        bail!(
            "No function found matching '{}'. Check the name and try again.",
            name
        );
    }

    let matched = if let Some(file) = file_filter {
        results.iter().find(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };

    let source = matched.unwrap_or(&results[0]);
    let chunk = &source.chunk;

    // Proactive staleness warning
    if !cli.quiet && !cli.no_stale_check {
        if let Some(file_str) = chunk.file.to_str() {
            staleness::warn_stale_results(&store, &[file_str], &root);
        }
    }

    // Get callers
    let callers = match store.get_callers_full(&chunk.name) {
        Ok(callers) => callers,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get callers for {}", chunk.name);
            Vec::new()
        }
    };

    // Get callees — scope to the resolved chunk's file to avoid ambiguity
    let chunk_file = chunk.file.to_string_lossy();
    let callees = match store.get_callees_full(&chunk.name, Some(&chunk_file)) {
        Ok(callees) => callees,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get callees for {}", chunk.name);
            Vec::new()
        }
    };

    // Get similar (top 3) using embedding
    let similar = match store.get_chunk_with_embedding(&chunk.id)? {
        Some((_, embedding)) => {
            let filter = SearchFilter {
                languages: None,
                chunk_types: None,
                path_pattern: None,
                name_boost: 0.0,
                query_text: String::new(),
                enable_rrf: false,
                note_weight: 0.0,
                note_only: false,
            };
            let index = HnswIndex::try_load(&cqs_dir);
            let sim_results = store.search_filtered_with_index(
                &embedding,
                &filter,
                4, // +1 to exclude self
                0.3,
                index.as_deref(),
            )?;
            sim_results
                .into_iter()
                .filter(|r| r.chunk.id != chunk.id)
                .take(3)
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    // Compute hints (only for function/method chunk types)
    let hints = if matches!(chunk.chunk_type, ChunkType::Function | ChunkType::Method) {
        compute_hints(&store, &chunk.name, Some(callers.len())).ok()
    } else {
        None
    };

    if json {
        let callers_json: Vec<_> = callers
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": c.file.to_string_lossy().replace('\\', "/"),
                    "line": c.line,
                })
            })
            .collect();

        let callees_json: Vec<_> = callees
            .iter()
            .map(|(name, line)| {
                serde_json::json!({
                    "name": name,
                    "line": line,
                })
            })
            .collect();

        let similar_json: Vec<_> = similar
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.chunk.name,
                    "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                    "score": r.score,
                })
            })
            .collect();

        let rel_file = chunk
            .file
            .strip_prefix(&root)
            .unwrap_or(&chunk.file)
            .to_string_lossy()
            .replace('\\', "/");

        let mut output = serde_json::json!({
            "name": chunk.name,
            "file": rel_file,
            "language": chunk.language.to_string(),
            "chunk_type": chunk.chunk_type.to_string(),
            "lines": [chunk.line_start, chunk.line_end],
            "signature": chunk.signature,
            "doc": chunk.doc,
            "callers": callers_json,
            "callees": callees_json,
            "similar": similar_json,
        });

        if let Some(ref h) = hints {
            output["hints"] = serde_json::json!({
                "caller_count": h.caller_count,
                "test_count": h.test_count,
                "no_callers": h.caller_count == 0,
                "no_tests": h.test_count == 0,
            });
        }

        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;

        let rel_file = chunk.file.strip_prefix(&root).unwrap_or(&chunk.file);

        println!(
            "{} ({} {})",
            chunk.name.bold(),
            chunk.chunk_type,
            chunk.language
        );
        println!(
            "{}:{}-{}",
            rel_file.display(),
            chunk.line_start,
            chunk.line_end
        );

        if let Some(ref h) = hints {
            if h.caller_count == 0 || h.test_count == 0 {
                let caller_part = if h.caller_count == 0 {
                    format!("{}", "0 callers".yellow())
                } else {
                    format!("{} callers", h.caller_count)
                };
                let test_part = if h.test_count == 0 {
                    format!("{}", "0 tests".yellow())
                } else {
                    format!("{} tests", h.test_count)
                };
                println!("{} | {}", caller_part, test_part);
            } else {
                println!("{} callers | {} tests", h.caller_count, h.test_count);
            }
        }

        if !chunk.signature.is_empty() {
            println!();
            println!("{}", chunk.signature.dimmed());
        }

        if let Some(ref doc) = chunk.doc {
            println!();
            println!("{}", doc.green());
        }

        if !callers.is_empty() {
            println!();
            println!("{}", "Callers:".cyan());
            for c in &callers {
                let rel = c.file.strip_prefix(&root).unwrap_or(&c.file);
                println!("  {} ({}:{})", c.name, rel.display(), c.line);
            }
        }

        if !callees.is_empty() {
            println!();
            println!("{}", "Callees:".cyan());
            for (name, _) in &callees {
                println!("  {}", name);
            }
        }

        if !similar.is_empty() {
            println!();
            println!("{}", "Similar:".cyan());
            for r in &similar {
                let rel = r.chunk.file.strip_prefix(&root).unwrap_or(&r.chunk.file);
                println!(
                    "  {} ({}:{}) [{:.2}]",
                    r.chunk.name,
                    rel.display(),
                    r.chunk.line_start,
                    r.score
                );
            }
        }
    }

    Ok(())
}
