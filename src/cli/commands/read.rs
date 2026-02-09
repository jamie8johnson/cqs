//! Read command for cqs
//!
//! Reads a file with context from notes injected as comments.
//! Respects audit mode (skips notes if active).

use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::sync::LazyLock;

use cqs::audit::load_audit_state;
use cqs::note::{parse_notes, path_matches_mention};
use cqs::Store;

use crate::cli::find_project_root;

use super::resolve::resolve_target;

/// Handle read command
pub(crate) fn cmd_read(path: &str, focus: Option<&str>, json: bool) -> Result<()> {
    // Focused read mode
    if let Some(focus) = focus {
        return cmd_read_focused(focus, json);
    }

    let root = find_project_root();
    let file_path = root.join(path);

    if !file_path.exists() {
        bail!("File not found: {}", path);
    }

    // Path traversal protection
    let canonical = file_path
        .canonicalize()
        .context("Failed to canonicalize path")?;
    let project_canonical = root
        .canonicalize()
        .context("Failed to canonicalize project root")?;
    #[cfg(not(windows))]
    let (canonical, project_canonical) = (canonical, project_canonical);
    #[cfg(windows)]
    let (canonical, project_canonical) = (
        cqs::strip_unc_prefix(canonical),
        cqs::strip_unc_prefix(project_canonical),
    );
    if !canonical.starts_with(&project_canonical) {
        bail!("Path traversal not allowed: {}", path);
    }

    // File size limit (10MB)
    const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
    let metadata = std::fs::metadata(&file_path).context("Failed to read file metadata")?;
    if metadata.len() > MAX_FILE_SIZE {
        bail!(
            "File too large: {} bytes (max {} bytes)",
            metadata.len(),
            MAX_FILE_SIZE
        );
    }

    let content = std::fs::read_to_string(&file_path).context("Failed to read file")?;

    // Check audit mode
    let cq_dir = root.join(".cq");
    let audit_mode = load_audit_state(&cq_dir);
    let mut context_header = String::new();

    if let Some(status) = audit_mode.status_line() {
        context_header.push_str(&format!("// {}\n//\n", status));
    }

    // Find relevant notes (skip if audit mode active)
    if !audit_mode.is_active() {
        let notes_path = root.join("docs/notes.toml");
        if notes_path.exists() {
            if let Ok(notes) = parse_notes(&notes_path) {
                let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let relevant: Vec<_> = notes
                    .iter()
                    .filter(|n| {
                        n.mentions
                            .iter()
                            .any(|m| m == file_name || m == path || path_matches_mention(path, m))
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

    let enriched = if context_header.is_empty() {
        content
    } else {
        format!("{}{}", context_header, content)
    };

    if json {
        let result = serde_json::json!({
            "path": path,
            "content": enriched,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print!("{}", enriched);
    }

    Ok(())
}

static TYPE_NAME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b([A-Z][a-zA-Z0-9_]+)\b").expect("hardcoded regex"));

static COMMON_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
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
    .collect()
});

/// Extract type names from a function signature
fn extract_type_names(signature: &str) -> Vec<String> {
    let mut names: Vec<String> = TYPE_NAME_RE
        .find_iter(signature)
        .map(|m| m.as_str().to_string())
        .filter(|name| !COMMON_TYPES.contains(name.as_str()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    names.sort();
    names
}

fn cmd_read_focused(focus: &str, json: bool) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    let (chunk, _) = resolve_target(&store, focus)?;

    let rel_file = chunk
        .file
        .strip_prefix(&root)
        .unwrap_or(&chunk.file)
        .to_string_lossy()
        .replace('\\', "/");

    let mut output = String::new();

    // Header
    output.push_str(&format!(
        "// [cqs] Focused read: {} ({}:{}-{})\n",
        chunk.name, rel_file, chunk.line_start, chunk.line_end
    ));

    // Note injection
    let audit_mode = load_audit_state(&cq_dir);
    if let Some(status) = audit_mode.status_line() {
        output.push_str(&format!("// {}\n", status));
    }

    if !audit_mode.is_active() {
        let notes_path = root.join("docs/notes.toml");
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
        if let Ok(results) = store.search_by_name(type_name, 5) {
            let type_def = results.iter().find(|r| {
                r.chunk.name == *type_name
                    && matches!(
                        r.chunk.chunk_type,
                        cqs::parser::ChunkType::Struct
                            | cqs::parser::ChunkType::Enum
                            | cqs::parser::ChunkType::Trait
                            | cqs::parser::ChunkType::Interface
                            | cqs::parser::ChunkType::Class
                    )
            });
            if let Some(r) = type_def {
                let dep_rel = r
                    .chunk
                    .file
                    .strip_prefix(&root)
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

    if json {
        let result = serde_json::json!({
            "focus": focus,
            "content": output,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print!("{}", output);
    }

    Ok(())
}
