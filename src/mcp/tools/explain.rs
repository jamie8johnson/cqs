//! Explain tool — generate a function card

use anyhow::{bail, Result};
use serde_json::Value;

use crate::store::SearchFilter;

use super::super::server::McpServer;

/// Parse target into (optional_file, name)
fn parse_target(target: &str) -> (Option<&str>, &str) {
    if let Some(pos) = target.rfind(':') {
        let file = &target[..pos];
        let name = &target[pos + 1..];
        if !file.is_empty() && !name.is_empty() {
            return (Some(file), name);
        }
    }
    (None, target)
}

pub fn tool_explain(server: &McpServer, arguments: Value) -> Result<Value> {
    let target = arguments
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: name"))?;

    // Resolve target
    let (file_filter, name) = parse_target(target);
    let name_results = server.store.search_by_name(name, 20)?;
    if name_results.is_empty() {
        bail!(
            "No function found matching '{}'. Check the name and try again.",
            name
        );
    }

    let matched = if let Some(file) = file_filter {
        name_results.iter().find(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };

    let source = matched.unwrap_or(&name_results[0]);
    let chunk = &source.chunk;

    // Get callers
    let callers = server
        .store
        .get_callers_full(&chunk.name)
        .unwrap_or_default();

    // Get callees
    let callees = server
        .store
        .get_callees_full(&chunk.name)
        .unwrap_or_default();

    // Get similar (top 3)
    let similar = match server.store.get_chunk_with_embedding(&chunk.id)? {
        Some((_, embedding)) => {
            let filter = SearchFilter {
                languages: None,
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
            let sim_results = server.store.search_filtered_with_index(
                &embedding,
                &filter,
                4,
                0.3,
                index_guard.as_deref(),
            )?;
            sim_results
                .into_iter()
                .filter(|r| r.chunk.id != chunk.id)
                .take(3)
                .collect::<Vec<_>>()
        }
        None => vec![],
    };

    // Build card
    let callers_json: Vec<Value> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.strip_prefix(&server.project_root)
                    .unwrap_or(&c.file)
                    .to_string_lossy()
                    .replace('\\', "/"),
                "line": c.line,
            })
        })
        .collect();

    let callees_json: Vec<Value> = callees
        .iter()
        .map(|(name, line)| {
            serde_json::json!({
                "name": name,
                "line": line,
            })
        })
        .collect();

    let similar_json: Vec<Value> = similar
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.chunk.name,
                "file": r.chunk.file.strip_prefix(&server.project_root)
                    .unwrap_or(&r.chunk.file)
                    .to_string_lossy()
                    .replace('\\', "/"),
                "score": r.score,
            })
        })
        .collect();

    let rel_file = chunk
        .file
        .strip_prefix(&server.project_root)
        .unwrap_or(&chunk.file)
        .to_string_lossy()
        .replace('\\', "/");

    let card = serde_json::json!({
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

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&card)?
        }]
    }))
}
