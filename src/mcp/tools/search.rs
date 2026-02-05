//! Search tool - semantic code search

use anyhow::Result;
use serde_json::Value;

use crate::parser::Language;
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

    // Definition search mode - find by name only, skip embedding
    if args.name_only.unwrap_or(false) {
        let results = server.store.search_by_name(&args.query, limit)?;
        let json_results: Vec<_> = results
            .iter()
            .filter(|r| r.score >= threshold)
            .map(|r| {
                serde_json::json!({
                    "type": "code",
                    // Normalize to forward slashes for consistent JSON output across platforms
                    "file": r.chunk.file.strip_prefix(&server.project_root)
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
                })
            })
            .collect();

        return Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&json_results)?
            }]
        }));
    }

    // Semantic search mode (default)
    let embedder = server.ensure_embedder()?;
    let query_embedding = embedder.embed_query(&args.query)?;

    let filter = SearchFilter {
        languages: args
            .language
            .map(|l| vec![l.parse().unwrap_or(Language::Rust)]),
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
    let results: Vec<UnifiedResult> = if audit_active {
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
    };

    let json_results: Vec<_> = results
        .iter()
        .map(|r| match r {
            UnifiedResult::Code(r) => {
                serde_json::json!({
                    "type": "code",
                    // Normalize to forward slashes for consistent JSON output across platforms
                    "file": r.chunk.file.strip_prefix(&server.project_root)
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
        "query": args.query,
        "total": results.len(),
    });

    // Add audit mode status if active (using value captured earlier)
    if let Some(status) = audit_status {
        result["audit_mode"] = serde_json::json!(status);
    }

    // MCP tools/call requires content array format
    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
