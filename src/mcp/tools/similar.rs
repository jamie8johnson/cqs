//! Similar tool — find code similar to a given function

use anyhow::{bail, Result};
use serde_json::Value;

use crate::store::SearchFilter;

use super::super::server::McpServer;
use super::resolve::parse_target;

pub fn tool_similar(server: &McpServer, arguments: Value) -> Result<Value> {
    let target = arguments
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: target"))?;

    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(5)
        .clamp(1, 20);

    let threshold = arguments
        .get("threshold")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(0.3);

    let language_filter = arguments
        .get("language")
        .and_then(|v| v.as_str())
        .map(|l| {
            l.parse::<crate::parser::Language>()
                .map(|lang| vec![lang])
                .map_err(|_| anyhow::anyhow!("Unknown language '{}'. Supported: rust, python, typescript, javascript, go, c, java", l))
        })
        .transpose()?;

    // Resolve target to chunk
    let (file_filter, name) = parse_target(target);

    let name_results = server.store.search_by_name(name, 20)?;
    if name_results.is_empty() {
        bail!(
            "No function found matching '{}'. Check the name and try again.",
            name
        );
    }

    // Filter by file if specified
    let matched = if let Some(file) = file_filter {
        name_results.iter().find(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };

    let source = matched.unwrap_or(&name_results[0]);
    let source_id = source.chunk.id.clone();
    let source_name = source.chunk.name.clone();

    // Fetch embedding
    let (_, embedding) = server
        .store
        .get_chunk_with_embedding(&source_id)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not load embedding for '{}'. Index may be corrupt.",
                source_name
            )
        })?;

    // Search with embedding as query
    let filter = SearchFilter {
        languages: language_filter,
        chunk_types: None,
        path_pattern: None,
        name_boost: 0.0,
        query_text: String::new(),
        enable_rrf: false,
        note_weight: 0.0,
    };

    let index_guard = server.index.read().unwrap_or_else(|e| {
        tracing::debug!("Index RwLock poisoned, recovering");
        e.into_inner()
    });

    let results = server.store.search_filtered_with_index(
        &embedding,
        &filter,
        limit + 1,
        threshold,
        index_guard.as_deref(),
    )?;

    // Exclude source chunk
    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| r.chunk.id != source_id)
        .take(limit)
        .collect();

    // Format results
    let json_results: Vec<Value> = filtered
        .iter()
        .map(|r| {
            serde_json::json!({
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

    let result = serde_json::json!({
        "target": source_name,
        "results": json_results,
        "total": filtered.len(),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
