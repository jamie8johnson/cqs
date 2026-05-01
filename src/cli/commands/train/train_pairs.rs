//! Train-pairs command — extract (NL description, code) pairs from index as JSONL
//!
//! Useful for fine-tuning embedding models on project-specific code.

use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, Result};

use cqs::store::{ChunkSummary, Store};

/// A single training pair: natural language description + code content.
#[derive(serde::Serialize)]
struct TrainPair {
    query: String,
    code: String,
    name: String,
    file: String,
    language: String,
}

/// Build a natural language description for a chunk.
fn build_nl_description(chunk: &ChunkSummary, contrastive_prefix: Option<&str>) -> String {
    let mut desc = String::new();

    if let Some(prefix) = contrastive_prefix {
        desc.push_str(prefix);
        desc.push(' ');
    }

    // Use doc comment if available, otherwise generate from signature
    if let Some(ref doc) = chunk.doc {
        let first_line = doc.lines().next().unwrap_or("");
        if !first_line.is_empty() {
            desc.push_str(first_line);
        } else {
            desc.push_str(&format!(
                "{} {} in {}",
                chunk.chunk_type, chunk.name, chunk.language
            ));
        }
    } else {
        desc.push_str(&format!(
            "{} {} in {}",
            chunk.chunk_type, chunk.name, chunk.language
        ));
    }

    desc
}

/// Build contrastive prefix from callees: "Unlike X and Y, ..."
fn build_contrastive_prefix<Mode>(store: &Store<Mode>, chunk: &ChunkSummary) -> Option<String> {
    let _span = tracing::debug_span!("build_contrastive_prefix", name = %chunk.name).entered();

    let callees = store
        .get_callees_full(&chunk.name, None)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, name = %chunk.name, "Failed to get callees for contrastive");
            Vec::new()
        });

    if callees.is_empty() {
        return None;
    }

    // Take up to 3 callees for the prefix
    let callee_names: Vec<&str> = callees
        .iter()
        .take(3)
        .map(|(name, _)| name.as_str())
        .collect();
    match callee_names.len() {
        1 => Some(format!("Unlike {},", callee_names[0])),
        2 => Some(format!(
            "Unlike {} and {},",
            callee_names[0], callee_names[1]
        )),
        _ => Some(format!(
            "Unlike {}, {}, and {},",
            callee_names[0], callee_names[1], callee_names[2]
        )),
    }
}

pub(crate) fn cmd_train_pairs(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    output: &Path,
    limit: Option<usize>,
    language: Option<&str>,
    contrastive: bool,
) -> Result<()> {
    let _span =
        tracing::info_span!("cmd_train_pairs", output = %output.display(), contrastive).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    // Get all chunks, optionally filtered by language.
    // Use chunk identities to get unique file origins, then batch-fetch full summaries.
    let identities = if let Some(lang) = language {
        store
            .all_chunk_identities_filtered(Some(lang))
            .context("Failed to load chunk identities")?
    } else {
        store
            .all_chunk_identities()
            .context("Failed to load chunk identities")?
    };

    let origins: Vec<String> = identities
        .iter()
        .map(|c| cqs::normalize_path(&c.file))
        .collect();
    let unique_origins: Vec<&str> = {
        let mut seen = std::collections::HashSet::new();
        origins
            .iter()
            .filter(|o: &&String| seen.insert(o.as_str()))
            .map(|o: &String| o.as_str())
            .collect()
    };
    let by_origin = store
        .get_chunks_by_origins_batch(&unique_origins)
        .context("Failed to load chunks by origin")?;
    let chunks: Vec<ChunkSummary> = by_origin.into_values().flatten().collect();

    let total = if let Some(lim) = limit {
        chunks.len().min(lim)
    } else {
        chunks.len()
    };

    // Open output file
    let mut file = std::fs::File::create(output)
        .with_context(|| format!("Failed to create output file: {}", output.display()))?;

    let mut written = 0;
    for chunk in chunks.iter().take(total) {
        // Skip chunks with empty content
        if chunk.content.trim().is_empty() {
            continue;
        }

        let contrastive_prefix = if contrastive {
            build_contrastive_prefix(store, chunk)
        } else {
            None
        };

        let pair = TrainPair {
            query: build_nl_description(chunk, contrastive_prefix.as_deref()),
            code: chunk.content.clone(),
            name: chunk.name.clone(),
            file: cqs::rel_display(&chunk.file, root),
            language: chunk.language.to_string(),
        };

        let line = serde_json::to_string(&pair).context("Failed to serialize training pair")?;
        writeln!(file, "{}", line)?;
        written += 1;
    }

    println!("Wrote {} training pairs to {}", written, output.display());
    if contrastive {
        println!("  (with contrastive prefixes from call graph)");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use cqs::language::{ChunkType, Language};
    use std::path::PathBuf;

    fn mock_chunk(name: &str, doc: Option<&str>) -> ChunkSummary {
        ChunkSummary {
            id: format!("src/lib.rs:{}:abcd", name),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: format!("fn {}() {{ todo!() }}", name),
            doc: doc.map(String::from),
            line_start: 1,
            line_end: 5,
            parent_id: None,
            parent_type_name: None,
            content_hash: format!("hash_{}", name),
            window_idx: None,
            parser_version: 0,
            vendored: false,
        }
    }

    #[test]
    fn build_nl_with_doc() {
        let chunk = mock_chunk("foo", Some("Parses a configuration file."));
        let desc = build_nl_description(&chunk, None);
        assert_eq!(desc, "Parses a configuration file.");
    }

    #[test]
    fn build_nl_without_doc() {
        let chunk = mock_chunk("foo", None);
        let desc = build_nl_description(&chunk, None);
        assert!(desc.contains("foo"));
        assert!(desc.contains("function")); // ChunkType::Function displays as lowercase
    }

    #[test]
    fn build_nl_with_contrastive_prefix() {
        let chunk = mock_chunk("foo", Some("Does something."));
        let desc = build_nl_description(&chunk, Some("Unlike bar and baz,"));
        assert!(desc.starts_with("Unlike bar and baz,"));
        assert!(desc.contains("Does something."));
    }

    #[test]
    fn output_format_is_jsonl() {
        let pair = TrainPair {
            query: "test query".to_string(),
            code: "fn test() {}".to_string(),
            name: "test".to_string(),
            file: "src/lib.rs".to_string(),
            language: "Rust".to_string(),
        };
        let line = serde_json::to_string(&pair).unwrap();
        // Should be valid JSON (single line)
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["query"], "test query");
        assert_eq!(parsed["code"], "fn test() {}");
    }

    #[test]
    fn language_filter_string() {
        // Just verify the language parsing works
        let lang: Language = "Rust".parse().unwrap();
        assert_eq!(lang, Language::Rust);
    }
}
