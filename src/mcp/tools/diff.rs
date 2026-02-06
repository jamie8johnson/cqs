//! Diff tool — semantic diff between indexed snapshots

use anyhow::{bail, Result};
use serde_json::Value;

use crate::diff::semantic_diff;
use crate::Store;

use super::super::server::McpServer;

pub fn tool_diff(server: &McpServer, arguments: Value) -> Result<Value> {
    let source = arguments
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: source"))?;

    let target = arguments
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("project");

    let threshold = arguments
        .get("threshold")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(0.95);

    let language = arguments.get("language").and_then(|v| v.as_str());

    // Load config to find reference paths
    let config = crate::config::Config::load(&server.project_root);

    // Resolve source store
    let source_cfg = config
        .references
        .iter()
        .find(|r| r.name == source)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                source
            )
        })?;

    let source_db = source_cfg.path.join("index.db");
    if !source_db.exists() {
        bail!(
            "Reference '{}' has no index. Run 'cqs ref update {}' first.",
            source,
            source
        );
    }
    let source_store = Store::open(&source_db)?;

    // Target store
    let target_store_owned;
    let target_store: &Store = if target == "project" {
        &server.store
    } else {
        let target_cfg = config
            .references
            .iter()
            .find(|r| r.name == target)
            .ok_or_else(|| anyhow::anyhow!("Reference '{}' not found.", target))?;
        let target_db = target_cfg.path.join("index.db");
        if !target_db.exists() {
            bail!("Reference '{}' has no index.", target);
        }
        target_store_owned = Store::open(&target_db)?;
        &target_store_owned
    };

    let result = semantic_diff(
        &source_store,
        target_store,
        source,
        target,
        threshold,
        language,
    )?;

    // Format result
    let added: Vec<Value> = result
        .added
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file,
                "type": e.chunk_type,
            })
        })
        .collect();

    let removed: Vec<Value> = result
        .removed
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file,
                "type": e.chunk_type,
            })
        })
        .collect();

    let modified: Vec<Value> = result
        .modified
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "file": e.file,
                "type": e.chunk_type,
                "similarity": e.similarity,
            })
        })
        .collect();

    let output = serde_json::json!({
        "source": result.source,
        "target": result.target,
        "added": added,
        "removed": removed,
        "modified": modified,
        "summary": {
            "added": result.added.len(),
            "removed": result.removed.len(),
            "modified": result.modified.len(),
            "unchanged": result.unchanged_count,
        }
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&output)?
        }]
    }))
}
