//! Search tool - semantic code search

use anyhow::Result;
use serde_json::Value;

use crate::parser::Language;
use crate::reference::{self, TaggedResult};
use crate::store::{SearchFilter, UnifiedResult};

use super::super::server::McpServer;
use super::super::types::SearchArgs;
use super::super::validation::validate_query_length;

/// Execute semantic search
pub fn tool_search(server: &McpServer, arguments: Value) -> Result<Value> {
    // SAFETY: Allocation bounded by 1MB request body limit (HTTP) or trusted client (stdio)
    let args: SearchArgs = serde_json::from_value(arguments)?;
    validate_query_length(&args.query)?;

    // Clamp limit to [1, 20] - 0 treated as 1, >20 capped at 20
    let limit = args.limit.unwrap_or(5).clamp(1, 20);
    let threshold = args.threshold.unwrap_or(0.3);

    // Determine which sources to search
    let search_project = should_search_source(&args.sources, "project");
    let has_refs = !server.references.is_empty();

    // Definition search mode - find by name only, skip embedding
    if args.name_only.unwrap_or(false) {
        return tool_search_name_only(server, &args, limit, threshold, search_project);
    }

    // Semantic search mode (default)
    let embedder = server.ensure_embedder()?;
    let query_embedding = embedder.embed_query(&args.query)?;

    let filter = SearchFilter {
        languages: args
            .language
            .map(|l| {
                l.parse::<Language>()
                    .map(|lang| vec![lang])
                    .map_err(|_| anyhow::anyhow!("Unknown language '{}'. Supported: rust, python, typescript, javascript, go, c, java", l))
            })
            .transpose()?,
        path_pattern: args.path_pattern,
        name_boost: args.name_boost.unwrap_or(0.2),
        query_text: args.query.clone(),
        enable_rrf: !args.semantic_only.unwrap_or(false), // RRF on by default, disable with semantic_only
        note_weight: args.note_weight.unwrap_or(1.0),
    };

    // Validate filter parameters before search
    filter
        .validate()
        .map_err(|e| anyhow::anyhow!("Invalid search filter: {}", e))?;

    // Read-lock the index (allows background CAGRA build to upgrade it)
    let index_guard = server.index.read().unwrap_or_else(|e| {
        tracing::debug!("Index RwLock poisoned (prior panic), recovering");
        e.into_inner()
    });

    // Check audit mode - capture both is_active and status_line in a single lock
    // acquisition to avoid TOCTOU (state could change between separate locks)
    let (audit_active, audit_status) = {
        let guard = server.audit_mode.lock().unwrap_or_else(|e| {
            tracing::debug!("Audit mode lock poisoned (prior panic), recovering");
            e.into_inner()
        });
        (guard.is_active(), guard.status_line())
    };
    let search_start = std::time::Instant::now();

    // Search primary store
    let primary_results: Vec<UnifiedResult> = if search_project {
        if audit_active {
            // Code-only search when audit mode is active
            let code_results = server.store.search_filtered_with_index(
                &query_embedding,
                &filter,
                limit,
                threshold,
                index_guard.as_deref(),
            )?;
            code_results.into_iter().map(UnifiedResult::Code).collect()
        } else {
            // Unified search including notes
            server.store.search_unified_with_index(
                &query_embedding,
                &filter,
                limit,
                threshold,
                index_guard.as_deref(),
            )?
        }
    } else {
        vec![]
    };

    // Fast path: no references configured
    if !has_refs || !has_matching_refs(server, &args.sources) {
        let search_ms = search_start.elapsed().as_millis();
        tracing::info!(
            results = primary_results.len(),
            elapsed_ms = search_ms,
            audit = audit_active,
            "MCP search completed"
        );
        return format_unified_results(primary_results, &args.query, audit_status);
    }

    // Multi-index search: search each reference
    let mut ref_results = Vec::new();
    for ref_idx in &server.references {
        if !should_search_source(&args.sources, &ref_idx.name) {
            continue;
        }
        let results =
            reference::search_reference(ref_idx, &query_embedding, &filter, limit, threshold);
        if !results.is_empty() {
            ref_results.push((ref_idx.name.clone(), results));
        }
    }

    let tagged = reference::merge_results(primary_results, ref_results, limit);
    let search_ms = search_start.elapsed().as_millis();
    tracing::info!(
        results = tagged.len(),
        elapsed_ms = search_ms,
        audit = audit_active,
        "MCP multi-index search completed"
    );

    format_tagged_results(tagged, &args.query, audit_status)
}

/// Name-only search across primary and references
fn tool_search_name_only(
    server: &McpServer,
    args: &SearchArgs,
    limit: usize,
    threshold: f32,
    search_project: bool,
) -> Result<Value> {
    if args.query.trim().is_empty() {
        return Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": "[]"
            }]
        }));
    }

    let has_refs = !server.references.is_empty();

    // Primary name search
    let primary_results: Vec<_> = if search_project {
        server
            .store
            .search_by_name(&args.query, limit)?
            .into_iter()
            .filter(|r| r.score >= threshold)
            .collect()
    } else {
        vec![]
    };

    // Fast path: no references
    if !has_refs || !has_matching_refs(server, &args.sources) {
        let json_results: Vec<_> = primary_results
            .iter()
            .map(|r| format_code_result(r, &server.project_root, None))
            .collect();
        return Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&json_results)?
            }]
        }));
    }

    // Search references by name
    let mut ref_results = Vec::new();
    for ref_idx in &server.references {
        if !should_search_source(&args.sources, &ref_idx.name) {
            continue;
        }
        let results = reference::search_reference_by_name(ref_idx, &args.query, limit, threshold);
        if !results.is_empty() {
            ref_results.push((ref_idx.name.clone(), results));
        }
    }

    let primary_unified = primary_results
        .into_iter()
        .map(UnifiedResult::Code)
        .collect();
    let tagged = reference::merge_results(primary_unified, ref_results, limit);

    let json_results: Vec<_> = tagged
        .iter()
        .filter_map(|t| match &t.result {
            UnifiedResult::Code(r) => {
                let root = if t.source.is_some() {
                    // Reference results: paths are relative to reference source, don't strip
                    None
                } else {
                    Some(server.project_root.as_path())
                };
                Some(format_code_result(
                    r,
                    root.unwrap_or(&server.project_root),
                    t.source.as_deref(),
                ))
            }
            UnifiedResult::Note(_) => {
                tracing::warn!("Unexpected note in name_only results, skipping");
                None
            }
        })
        .collect();

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&json_results)?
        }]
    }))
}

/// Format a single code SearchResult as JSON
fn format_code_result(
    r: &crate::store::SearchResult,
    strip_root: &std::path::Path,
    source: Option<&str>,
) -> Value {
    let mut json = serde_json::json!({
        "type": "code",
        "file": r.chunk.file.strip_prefix(strip_root)
            .unwrap_or(&r.chunk.file)
            .to_string_lossy()
            .replace('\\', "/"),
        "line_start": r.chunk.line_start,
        "line_end": r.chunk.line_end,
        "name": r.chunk.name,
        "signature": r.chunk.signature,
        "language": r.chunk.language.to_string(),
        "chunk_type": r.chunk.chunk_type.to_string(),
        "score": r.score,
        "content": r.chunk.content,
    });
    if let Some(src) = source {
        json["source"] = serde_json::json!(src);
    }
    json
}

/// Format unified results (no references) — existing format for backward compat
fn format_unified_results(
    results: Vec<UnifiedResult>,
    query: &str,
    audit_status: Option<String>,
) -> Result<Value> {
    let json_results: Vec<_> = results
        .iter()
        .map(|r| match r {
            UnifiedResult::Code(r) => {
                serde_json::json!({
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
                })
            }
            UnifiedResult::Note(r) => {
                serde_json::json!({
                    "type": "note",
                    "text": r.note.text,
                    "sentiment": r.note.sentiment,
                    "mentions": r.note.mentions,
                    "score": r.score,
                })
            }
        })
        .collect();

    let mut result = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": results.len(),
    });

    if let Some(status) = audit_status {
        result["audit_mode"] = serde_json::json!(status);
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}

/// Format tagged results (multi-index) with source field
fn format_tagged_results(
    tagged: Vec<TaggedResult>,
    query: &str,
    audit_status: Option<String>,
) -> Result<Value> {
    let total = tagged.len();
    let json_results: Vec<_> = tagged
        .iter()
        .map(|t| {
            let mut json = match &t.result {
                UnifiedResult::Code(r) => {
                    serde_json::json!({
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
                    })
                }
                UnifiedResult::Note(r) => {
                    serde_json::json!({
                        "type": "note",
                        "text": r.note.text,
                        "sentiment": r.note.sentiment,
                        "mentions": r.note.mentions,
                        "score": r.score,
                    })
                }
            };
            if let Some(source) = &t.source {
                json["source"] = serde_json::json!(source);
            }
            json
        })
        .collect();

    let mut result = serde_json::json!({
        "results": json_results,
        "query": query,
        "total": total,
    });

    if let Some(status) = audit_status {
        result["audit_mode"] = serde_json::json!(status);
    }

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}

/// Check if a source should be searched based on the sources filter
fn should_search_source(sources: &Option<Vec<String>>, name: &str) -> bool {
    match sources {
        None => true, // No filter = search all
        Some(list) => list.iter().any(|s| s == name),
    }
}

/// Check if any configured references match the sources filter
fn has_matching_refs(server: &McpServer, sources: &Option<Vec<String>>) -> bool {
    server
        .references
        .iter()
        .any(|r| should_search_source(sources, &r.name))
}
