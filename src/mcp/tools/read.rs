//! Read tool - file reading with context injection

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::note::parse_notes;

use super::super::server::McpServer;
use super::super::validation::strip_unc_prefix;

/// Read a file with context from notes
pub fn tool_read(server: &McpServer, arguments: Value) -> Result<Value> {
    let path = arguments
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;

    let file_path = server.project_root.join(path);
    if !file_path.exists() {
        bail!("File not found: {}", path);
    }

    // Path traversal protection (strip UNC prefix on Windows for consistent comparison)
    let canonical = strip_unc_prefix(
        file_path
            .canonicalize()
            .context("Failed to canonicalize path")?,
    );
    let project_canonical = strip_unc_prefix(
        server
            .project_root
            .canonicalize()
            .context("Failed to canonicalize project root")?,
    );
    if !canonical.starts_with(&project_canonical) {
        bail!("Path traversal not allowed: {}", path);
    }

    // File size limit to prevent memory exhaustion (10MB)
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    let metadata = std::fs::metadata(&file_path).context("Failed to read file metadata")?;
    if metadata.len() > MAX_FILE_SIZE {
        bail!(
            "File too large: {} bytes (max {} bytes)",
            metadata.len(),
            MAX_FILE_SIZE
        );
    }

    // Read file content
    let content = std::fs::read_to_string(&file_path).context("Failed to read file")?;

    // Check audit mode - if active, skip note injection
    let audit_guard = server.audit_mode.lock().unwrap_or_else(|e| {
        tracing::debug!("Audit mode lock poisoned (prior panic), recovering");
        e.into_inner()
    });
    let audit_active = audit_guard.is_active();
    let mut context_header = String::new();

    // Add audit mode status line if active
    if let Some(status) = audit_guard.status_line() {
        context_header.push_str(&format!("// {}\n//\n", status));
    }
    drop(audit_guard); // Release lock before file I/O

    // Find relevant notes by searching for this file path (skip if audit mode active)
    if !audit_active {
        let notes_path = server.project_root.join("docs/notes.toml");

        if notes_path.exists() {
            if let Ok(notes) = parse_notes(&notes_path) {
                // Find notes that mention this file
                let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                let relevant: Vec<_> = notes
                    .iter()
                    .filter(|n| {
                        n.mentions
                            .iter()
                            .any(|m| m == file_name || m == path || path.contains(m))
                    })
                    .collect();

                if !relevant.is_empty() {
                    context_header.push_str(
                        "// ┌─────────────────────────────────────────────────────────────┐\n",
                    );
                    context_header.push_str(
                        "// │ [cqs] Context from notes.toml                              │\n",
                    );
                    context_header.push_str(
                        "// └─────────────────────────────────────────────────────────────┘\n",
                    );

                    for n in relevant {
                        let sentiment_label = if n.sentiment() < -0.3 {
                            "WARNING"
                        } else if n.sentiment() > 0.3 {
                            "PATTERN"
                        } else {
                            "NOTE"
                        };
                        // First line of text only
                        if let Some(first_line) = n.text.lines().next() {
                            context_header.push_str(&format!(
                                "// [{}] {}\n",
                                sentiment_label,
                                first_line.trim()
                            ));
                        }
                    }
                    context_header.push_str("//\n");
                }
            }
        }
    }

    let enriched_content = if context_header.is_empty() {
        content
    } else {
        format!("{}{}", context_header, content)
    };

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": enriched_content
        }]
    }))
}
