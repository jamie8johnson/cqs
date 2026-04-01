//! Reconstruct source file from index chunks.
//!
//! Assembles a file's content from indexed chunks, ordered by line number.
//! Works even without the original source file on disk — useful for remote
//! agents with index-only access.

use anyhow::{bail, Result};

pub(crate) fn cmd_reconstruct(path: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_reconstruct", %path).entered();
    let (store, root, _) = crate::cli::open_project_store_readonly()?;

    // Normalize the path relative to project root
    let rel_path = if std::path::Path::new(path).is_absolute() {
        match std::path::Path::new(path).strip_prefix(&root) {
            Ok(rel) => cqs::normalize_path(rel),
            Err(_) => cqs::normalize_path(std::path::Path::new(path)),
        }
    } else {
        cqs::normalize_path(std::path::Path::new(path))
    };

    let chunks = store.get_chunks_by_origin(&rel_path)?;
    if chunks.is_empty() {
        bail!(
            "No indexed chunks found for '{}'. Run `cqs index` first.",
            path
        );
    }

    if json {
        let output = serde_json::json!({
            "file": rel_path,
            "chunks": chunks.len(),
            "lines": chunks.last().map(|c| c.line_end).unwrap_or(0),
            "content": assemble(&chunks),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print!("{}", assemble(&chunks));
    }

    Ok(())
}

/// Assemble source from chunks, noting gaps between them.
fn assemble(chunks: &[cqs::store::ChunkSummary]) -> String {
    let mut out = String::new();
    let mut last_end: u32 = 0;

    for chunk in chunks {
        if chunk.line_start > last_end + 1 && last_end > 0 {
            let gap = chunk.line_start - last_end - 1;
            out.push_str(&format!(
                "\n// ... ({} line{} not indexed, lines {}-{}) ...\n\n",
                gap,
                if gap == 1 { "" } else { "s" },
                last_end + 1,
                chunk.line_start - 1,
            ));
        }
        out.push_str(&chunk.content);
        if !chunk.content.ends_with('\n') {
            out.push('\n');
        }
        last_end = chunk.line_end;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::store::ChunkSummary;
    use std::path::PathBuf;

    fn make_chunk(name: &str, content: &str, start: u32, end: u32) -> ChunkSummary {
        ChunkSummary {
            id: name.to_string(),
            name: name.to_string(),
            file: PathBuf::from("test.rs"),
            language: cqs::parser::Language::Rust,
            chunk_type: cqs::parser::ChunkType::Function,
            signature: String::new(),
            content: content.to_string(),
            doc: None,
            line_start: start,
            line_end: end,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
        }
    }

    #[test]
    fn test_assemble_no_gaps() {
        let chunks = vec![
            make_chunk("foo", "fn foo() {}\n", 1, 1),
            make_chunk("bar", "fn bar() {}\n", 2, 2),
        ];
        let result = assemble(&chunks);
        assert_eq!(result, "fn foo() {}\nfn bar() {}\n");
    }

    #[test]
    fn test_assemble_with_gap() {
        let chunks = vec![
            make_chunk("foo", "fn foo() {}\n", 1, 3),
            make_chunk("bar", "fn bar() {}\n", 10, 12),
        ];
        let result = assemble(&chunks);
        assert!(result.contains("6 lines not indexed"));
        assert!(result.contains("lines 4-9"));
    }

    #[test]
    fn test_assemble_empty() {
        let result = assemble(&[]);
        assert_eq!(result, "");
    }
}
