//! Read tool - file reading with context injection

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashSet;

use crate::note::parse_notes;

use super::super::server::McpServer;
use super::super::validation::strip_unc_prefix;

/// Read a file with context from notes
pub fn tool_read(server: &McpServer, arguments: Value) -> Result<Value> {
    // Check for focused read mode
    if let Some(focus) = arguments.get("focus").and_then(|v| v.as_str()) {
        return tool_read_focused(server, focus, &arguments);
    }

    let path = arguments
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!("Missing 'path' argument (or use 'focus' for focused read)")
        })?;

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

/// Extract type names from a function signature
fn extract_type_names(signature: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\b([A-Z][a-zA-Z0-9_]+)\b").expect("hardcoded regex");
    let common: HashSet<&str> = [
        "String",
        "Vec",
        "Result",
        "Option",
        "Box",
        "Arc",
        "Rc",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "Path",
        "PathBuf",
        "Value",
        "Error",
        "Self",
        "None",
        "Some",
        "Ok",
        "Err",
        "Mutex",
        "RwLock",
        "Cow",
        "Pin",
        "Future",
        "Iterator",
        "Display",
        "Debug",
        "Clone",
        "Default",
        "Send",
        "Sync",
        "Sized",
        "Copy",
        "From",
        "Into",
        "AsRef",
        "AsMut",
        "Deref",
        "DerefMut",
        "Read",
        "Write",
        "Seek",
        "BufRead",
        "ToString",
        "Serialize",
        "Deserialize",
    ]
    .into_iter()
    .collect();

    let mut names: Vec<String> = re
        .find_iter(signature)
        .map(|m| m.as_str().to_string())
        .filter(|name| !common.contains(name.as_str()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    names.sort();
    names
}

fn tool_read_focused(server: &McpServer, focus: &str, _arguments: &Value) -> Result<Value> {
    use super::resolve::resolve_target;

    let (chunk, _) = resolve_target(&server.store, focus)?;

    let rel_file = chunk
        .file
        .strip_prefix(&server.project_root)
        .unwrap_or(&chunk.file)
        .to_string_lossy()
        .replace('\\', "/");

    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "// [cqs] Focused read: {} ({}:{}-{})\n",
        chunk.name, rel_file, chunk.line_start, chunk.line_end
    ));

    // Note injection (same as regular read, but for this function)
    let audit_guard = server.audit_mode.lock().unwrap_or_else(|e| e.into_inner());
    let audit_active = audit_guard.is_active();
    if let Some(status) = audit_guard.status_line() {
        output.push_str(&format!("// {}\n", status));
    }
    drop(audit_guard);

    if !audit_active {
        let notes_path = server.project_root.join("docs/notes.toml");
        if notes_path.exists() {
            if let Ok(notes) = parse_notes(&notes_path) {
                let relevant: Vec<_> = notes
                    .iter()
                    .filter(|n| {
                        n.mentions
                            .iter()
                            .any(|m| m == &chunk.name || m == &rel_file)
                    })
                    .collect();
                for n in &relevant {
                    let label = if n.sentiment() < -0.3 {
                        "WARNING"
                    } else if n.sentiment() > 0.3 {
                        "PATTERN"
                    } else {
                        "NOTE"
                    };
                    if let Some(first_line) = n.text.lines().next() {
                        output.push_str(&format!("// [{}] {}\n", label, first_line.trim()));
                    }
                }
                if !relevant.is_empty() {
                    output.push_str("//\n");
                }
            }
        }
    }

    // Target function
    output.push_str("\n// --- Target ---\n");
    if let Some(ref doc) = chunk.doc {
        output.push_str(doc);
        output.push('\n');
    }
    output.push_str(&chunk.content);
    output.push('\n');

    // Type dependencies
    let type_names = extract_type_names(&chunk.signature);
    for type_name in &type_names {
        if let Ok(results) = server.store.search_by_name(type_name, 5) {
            // Find exact match that's a type definition (struct, enum, trait, interface)
            let type_def = results.iter().find(|r| {
                r.chunk.name == *type_name
                    && matches!(
                        r.chunk.chunk_type,
                        crate::parser::ChunkType::Struct
                            | crate::parser::ChunkType::Enum
                            | crate::parser::ChunkType::Trait
                            | crate::parser::ChunkType::Interface
                            | crate::parser::ChunkType::Class
                    )
            });
            if let Some(r) = type_def {
                let dep_rel = r
                    .chunk
                    .file
                    .strip_prefix(&server.project_root)
                    .unwrap_or(&r.chunk.file)
                    .to_string_lossy()
                    .replace('\\', "/");
                output.push_str(&format!(
                    "\n// --- Type: {} ({}:{}-{}) ---\n",
                    r.chunk.name, dep_rel, r.chunk.line_start, r.chunk.line_end
                ));
                output.push_str(&r.chunk.content);
                output.push('\n');
            }
        }
    }

    Ok(serde_json::json!({"content": [{"type": "text", "text": output}]}))
}
