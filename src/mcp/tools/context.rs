//! Context tool — module-level understanding

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashSet;

use crate::note::parse_notes;

use super::super::server::McpServer;

pub fn tool_context(server: &McpServer, arguments: Value) -> Result<Value> {
    let path = arguments
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: path"))?;

    // Resolve origin — try both relative and absolute forms
    let abs_path = server.project_root.join(path);
    let origin = abs_path.to_string_lossy().to_string();

    let mut chunks = server.store.get_chunks_by_origin(&origin)?;
    if chunks.is_empty() {
        // Try with the path as-is (might already match origin format)
        chunks = server.store.get_chunks_by_origin(path)?;
    }
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Is the file indexed?",
            path
        );
    }

    // Chunk summaries (signatures only, not full content)
    let chunks_json: Vec<Value> = chunks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "lines": [c.line_start, c.line_end],
                "doc": c.doc,
            })
        })
        .collect();

    // External callers — functions from OTHER files that call functions in this file
    let chunk_names: HashSet<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
    let mut external_callers = Vec::new();
    let mut dependent_files: HashSet<String> = HashSet::new();

    for chunk in &chunks {
        let callers = match server.store.get_callers_full(&chunk.name) {
            Ok(callers) => callers,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to get callers for {}", chunk.name);
                Vec::new()
            }
        };
        for caller in callers {
            let caller_origin = caller.file.to_string_lossy().to_string();
            if caller_origin != origin && !caller_origin.ends_with(path) {
                let rel = caller
                    .file
                    .strip_prefix(&server.project_root)
                    .unwrap_or(&caller.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                external_callers.push(serde_json::json!({
                    "caller": caller.name,
                    "caller_file": &rel,
                    "calls": chunk.name,
                    "line": caller.line,
                }));
                dependent_files.insert(rel);
            }
        }
    }

    // External callees — functions this file calls that live elsewhere
    let mut external_callees = Vec::new();
    for chunk in &chunks {
        let chunk_file = chunk.file.to_string_lossy();
        let callees = match server
            .store
            .get_callees_full(&chunk.name, Some(&chunk_file))
        {
            Ok(callees) => callees,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to get callees for {}", chunk.name);
                Vec::new()
            }
        };
        for (callee_name, _) in callees {
            if !chunk_names.contains(callee_name.as_str()) {
                external_callees.push(serde_json::json!({
                    "callee": callee_name,
                    "called_from": chunk.name,
                }));
            }
        }
    }

    // Deduplicate external callees by name
    let mut seen_callees: HashSet<String> = HashSet::new();
    external_callees.retain(|c| {
        let name = c
            .get("callee")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        seen_callees.insert(name)
    });

    // Related notes
    let mut notes_json = Vec::new();
    let audit_guard = server.audit_mode.lock().unwrap_or_else(|e| e.into_inner());
    let audit_active = audit_guard.is_active();
    drop(audit_guard);

    if !audit_active {
        let notes_path = server.project_root.join("docs/notes.toml");
        if notes_path.exists() {
            if let Ok(notes) = parse_notes(&notes_path) {
                let file_name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                for n in &notes {
                    let relevant = n.mentions.iter().any(|m| {
                        m == file_name
                            || m == path
                            || path.contains(m)
                            || chunk_names.contains(m.as_str())
                    });
                    if relevant {
                        notes_json.push(serde_json::json!({
                            "text": n.text,
                            "sentiment": n.sentiment(),
                        }));
                    }
                }
            }
        }
    }

    let mut dep_files: Vec<String> = dependent_files.into_iter().collect();
    dep_files.sort();

    let result = serde_json::json!({
        "file": path,
        "chunks": chunks_json,
        "external_callers": external_callers,
        "external_callees": external_callees,
        "dependent_files": dep_files,
        "notes": notes_json,
    });

    Ok(
        serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&result)?}]}),
    )
}
