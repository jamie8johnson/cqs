//! Output and display functions for CLI results

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::reference::TaggedResult;
use cqs::store::{ParentContext, UnifiedResult};

/// Read context lines before and after a range in a file
///
/// # Arguments
/// * `line_start` - 1-indexed start line (0 treated as 1)
/// * `line_end` - 1-indexed end line (must be >= line_start)
pub fn read_context_lines(
    file: &Path,
    line_start: u32,
    line_end: u32,
    context: usize,
) -> Result<(Vec<String>, Vec<String>)> {
    // Size guard: don't read files larger than 10MB for context display
    const MAX_DISPLAY_FILE_SIZE: u64 = 10 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(file) {
        if meta.len() > MAX_DISPLAY_FILE_SIZE {
            anyhow::bail!(
                "File too large for context display: {}MB (limit {}MB)",
                meta.len() / (1024 * 1024),
                MAX_DISPLAY_FILE_SIZE / (1024 * 1024)
            );
        }
    }
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    // .lines() handles \r\n, but trim trailing \r for bare-CR edge cases
    let lines: Vec<&str> = content.lines().map(|l| l.trim_end_matches('\r')).collect();

    // Normalize: treat 0 as 1, ensure end >= start
    let line_start = line_start.max(1);
    let line_end = line_end.max(line_start);

    // Convert 1-indexed lines to 0-indexed array indices, clamped to valid range.
    // For an empty file (lines.len() == 0), both indices will be 0.
    let max_idx = lines.len().saturating_sub(1);
    let start_idx = (line_start as usize).saturating_sub(1).min(max_idx);
    let end_idx = (line_end as usize).saturating_sub(1).min(max_idx);

    // Context before
    let context_start = start_idx.saturating_sub(context);
    let before: Vec<String> = if start_idx <= lines.len() {
        lines[context_start..start_idx]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    };

    // Context after
    let context_end = (end_idx + context + 1).min(lines.len());
    let after: Vec<String> = if end_idx + 1 < lines.len() {
        lines[(end_idx + 1)..context_end]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    };

    Ok((before, after))
}

/// Display unified search results (code + notes)
pub fn display_unified_results(
    results: &[UnifiedResult],
    root: &Path,
    no_content: bool,
    context: Option<usize>,
    parents: Option<&HashMap<String, ParentContext>>,
) -> Result<()> {
    for result in results {
        match result {
            UnifiedResult::Code(r) => {
                // Paths are stored relative; strip_prefix handles legacy absolute paths
                let rel_path = r.chunk.file.strip_prefix(root).unwrap_or(&r.chunk.file);

                let parent_tag = if r.chunk.parent_id.is_some() {
                    " [has parent]"
                } else {
                    ""
                };
                let header = format!(
                    "{}:{} ({} {}) [{}] [{:.2}]{}",
                    rel_path.display(),
                    r.chunk.line_start,
                    r.chunk.chunk_type,
                    r.chunk.name,
                    r.chunk.language,
                    r.score,
                    parent_tag
                );

                println!("{}", header.cyan());

                if !no_content {
                    println!("{}", "─".repeat(50));

                    // Read context if requested
                    if let Some(n) = context {
                        if n > 0 {
                            let abs_path = root.join(&r.chunk.file);
                            if let Ok((before, _)) = read_context_lines(
                                &abs_path,
                                r.chunk.line_start,
                                r.chunk.line_end,
                                n,
                            ) {
                                for line in &before {
                                    println!("{}", format!("  {}", line).dimmed());
                                }
                            }
                        }
                    }

                    // Show signature or truncated content
                    if r.chunk.content.lines().count() <= 10 {
                        println!("{}", r.chunk.content);
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", line);
                        }
                        println!("    ...");
                    }

                    // Print after context if requested
                    if let Some(n) = context {
                        if n > 0 {
                            let abs_path = root.join(&r.chunk.file);
                            if let Ok((_, after)) = read_context_lines(
                                &abs_path,
                                r.chunk.line_start,
                                r.chunk.line_end,
                                n,
                            ) {
                                for line in &after {
                                    println!("{}", format!("  {}", line).dimmed());
                                }
                            }
                        }
                    }

                    // Show parent context if --expand
                    if let Some(parent) = parents.and_then(|p| p.get(&r.chunk.id)) {
                        let parent_header = format!(
                            "  Parent context: {} ({}:{}-{})",
                            parent.name,
                            rel_path.display(),
                            parent.line_start,
                            parent.line_end,
                        );
                        println!("{}", parent_header.dimmed());
                        println!("{}", "  ────────────────────────────────".dimmed());
                        for line in parent.content.lines().take(20) {
                            println!("{}", format!("  {}", line).dimmed());
                        }
                        if parent.content.lines().count() > 20 {
                            println!("{}", "  ...".dimmed());
                        }
                    }

                    println!();
                }
            }
            UnifiedResult::Note(r) => {
                // Format: [note:sentiment] text [score]
                let sentiment_indicator = if r.note.sentiment < -0.3 {
                    format!("v={:.1}", r.note.sentiment).red()
                } else if r.note.sentiment > 0.3 {
                    format!("v={:.1}", r.note.sentiment).green()
                } else {
                    format!("v={:.1}", r.note.sentiment).dimmed()
                };

                let header = format!("[note] {} [{:.2}]", sentiment_indicator, r.score);

                println!("{}", header.blue());

                if !no_content {
                    println!("{}", "─".repeat(50));
                    // Show truncated text
                    let text_lines: Vec<&str> = r.note.text.lines().collect();
                    if text_lines.len() <= 3 {
                        println!("{}", r.note.text);
                    } else {
                        for line in text_lines.iter().take(3) {
                            println!("{}", line);
                        }
                        println!("    ...");
                    }
                    println!();
                }
            }
        }
    }

    println!("{} results", results.len());
    Ok(())
}

/// Display unified results as JSON
pub fn display_unified_results_json(
    results: &[UnifiedResult],
    query: &str,
    parents: Option<&HashMap<String, ParentContext>>,
) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|r| match r {
            UnifiedResult::Code(r) => {
                let mut obj = serde_json::json!({
                    "type": "code",
                    // Normalize to forward slashes for consistent JSON output across platforms
                    "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                    "line_start": r.chunk.line_start,
                    "line_end": r.chunk.line_end,
                    "name": r.chunk.name,
                    "signature": r.chunk.signature,
                    "language": r.chunk.language.to_string(),
                    "chunk_type": r.chunk.chunk_type.to_string(),
                    "score": r.score,
                    "content": r.chunk.content,
                    "has_parent": r.chunk.parent_id.is_some(),
                });
                if let Some(parent) = parents.and_then(|p| p.get(&r.chunk.id)) {
                    obj["parent_name"] = serde_json::json!(parent.name);
                    obj["parent_content"] = serde_json::json!(parent.content);
                    obj["parent_line_start"] = serde_json::json!(parent.line_start);
                    obj["parent_line_end"] = serde_json::json!(parent.line_end);
                }
                obj
            }
            UnifiedResult::Note(r) => serde_json::json!({
                "type": "note",
                "id": r.note.id,
                "text": r.note.text,
                "sentiment": r.note.sentiment,
                "mentions": r.note.mentions,
                "score": r.score,
            }),
        })
        .collect();

    let output = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": results.len(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Display tagged search results (multi-index with source labels)
pub fn display_tagged_results(
    results: &[TaggedResult],
    root: &Path,
    no_content: bool,
    context: Option<usize>,
    parents: Option<&HashMap<String, ParentContext>>,
) -> Result<()> {
    for tagged in results {
        match &tagged.result {
            UnifiedResult::Code(r) => {
                let rel_path = r.chunk.file.strip_prefix(root).unwrap_or(&r.chunk.file);

                // Prepend source name for reference results
                let source_prefix = tagged
                    .source
                    .as_ref()
                    .map(|s| format!("[{}] ", s))
                    .unwrap_or_default();

                let parent_tag = if r.chunk.parent_id.is_some() {
                    " [has parent]"
                } else {
                    ""
                };
                let header = format!(
                    "{}{}:{} ({} {}) [{}] [{:.2}]{}",
                    source_prefix,
                    rel_path.display(),
                    r.chunk.line_start,
                    r.chunk.chunk_type,
                    r.chunk.name,
                    r.chunk.language,
                    r.score,
                    parent_tag
                );

                println!("{}", header.cyan());

                if !no_content {
                    println!("{}", "─".repeat(50));

                    // Context lines only for project results (reference source files may not exist)
                    if tagged.source.is_none() {
                        if let Some(n) = context {
                            if n > 0 {
                                let abs_path = root.join(&r.chunk.file);
                                if let Ok((before, _)) = read_context_lines(
                                    &abs_path,
                                    r.chunk.line_start,
                                    r.chunk.line_end,
                                    n,
                                ) {
                                    for line in &before {
                                        println!("{}", format!("  {}", line).dimmed());
                                    }
                                }
                            }
                        }
                    }

                    if r.chunk.content.lines().count() <= 10 {
                        println!("{}", r.chunk.content);
                    } else {
                        for line in r.chunk.content.lines().take(8) {
                            println!("{}", line);
                        }
                        println!("    ...");
                    }

                    // After context only for project results
                    if tagged.source.is_none() {
                        if let Some(n) = context {
                            if n > 0 {
                                let abs_path = root.join(&r.chunk.file);
                                if let Ok((_, after)) = read_context_lines(
                                    &abs_path,
                                    r.chunk.line_start,
                                    r.chunk.line_end,
                                    n,
                                ) {
                                    for line in &after {
                                        println!("{}", format!("  {}", line).dimmed());
                                    }
                                }
                            }
                        }
                    }

                    // Show parent context if --expand
                    if let Some(parent) = parents.and_then(|p| p.get(&r.chunk.id)) {
                        let parent_header = format!(
                            "  Parent context: {} ({}:{}-{})",
                            parent.name,
                            rel_path.display(),
                            parent.line_start,
                            parent.line_end,
                        );
                        println!("{}", parent_header.dimmed());
                        println!("{}", "  ────────────────────────────────".dimmed());
                        for line in parent.content.lines().take(20) {
                            println!("{}", format!("  {}", line).dimmed());
                        }
                        if parent.content.lines().count() > 20 {
                            println!("{}", "  ...".dimmed());
                        }
                    }

                    println!();
                }
            }
            UnifiedResult::Note(r) => {
                let sentiment_indicator = if r.note.sentiment < -0.3 {
                    format!("v={:.1}", r.note.sentiment).red()
                } else if r.note.sentiment > 0.3 {
                    format!("v={:.1}", r.note.sentiment).green()
                } else {
                    format!("v={:.1}", r.note.sentiment).dimmed()
                };

                let header = format!("[note] {} [{:.2}]", sentiment_indicator, r.score);
                println!("{}", header.blue());

                if !no_content {
                    println!("{}", "─".repeat(50));
                    let text_lines: Vec<&str> = r.note.text.lines().collect();
                    if text_lines.len() <= 3 {
                        println!("{}", r.note.text);
                    } else {
                        for line in text_lines.iter().take(3) {
                            println!("{}", line);
                        }
                        println!("    ...");
                    }
                    println!();
                }
            }
        }
    }

    println!("{} results", results.len());
    Ok(())
}

/// Display similar results as JSON
pub fn display_similar_results_json(
    results: &[cqs::store::SearchResult],
    target: &str,
) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                "line_start": r.chunk.line_start,
                "line_end": r.chunk.line_end,
                "name": r.chunk.name,
                "signature": r.chunk.signature,
                "language": r.chunk.language.to_string(),
                "chunk_type": r.chunk.chunk_type.to_string(),
                "score": r.score,
                "content": r.chunk.content,
            })
        })
        .collect();

    let output = serde_json::json!({
        "target": target,
        "results": json_results,
        "total": results.len(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Display tagged results as JSON (multi-index with source field)
pub fn display_tagged_results_json(
    results: &[TaggedResult],
    query: &str,
    parents: Option<&HashMap<String, ParentContext>>,
) -> Result<()> {
    let json_results: Vec<_> = results
        .iter()
        .map(|t| {
            let mut json = match &t.result {
                UnifiedResult::Code(r) => {
                    let mut obj = serde_json::json!({
                        "type": "code",
                        "file": r.chunk.file.to_string_lossy().replace('\\', "/"),
                        "line_start": r.chunk.line_start,
                        "line_end": r.chunk.line_end,
                        "name": r.chunk.name,
                        "signature": r.chunk.signature,
                        "language": r.chunk.language.to_string(),
                        "chunk_type": r.chunk.chunk_type.to_string(),
                        "score": r.score,
                        "content": r.chunk.content,
                        "has_parent": r.chunk.parent_id.is_some(),
                    });
                    if let Some(parent) = parents.and_then(|p| p.get(&r.chunk.id)) {
                        obj["parent_name"] = serde_json::json!(parent.name);
                        obj["parent_content"] = serde_json::json!(parent.content);
                        obj["parent_line_start"] = serde_json::json!(parent.line_start);
                        obj["parent_line_end"] = serde_json::json!(parent.line_end);
                    }
                    obj
                }
                UnifiedResult::Note(r) => serde_json::json!({
                    "type": "note",
                    "id": r.note.id,
                    "text": r.note.text,
                    "sentiment": r.note.sentiment,
                    "mentions": r.note.mentions,
                    "score": r.score,
                }),
            };
            if let Some(source) = &t.source {
                json["source"] = serde_json::json!(source);
            }
            json
        })
        .collect();

    let output = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": results.len(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
