//! Similar command — find code similar to a given function

use anyhow::{bail, Context, Result};

use cqs::{HnswIndex, SearchFilter, Store};

use crate::cli::{display, find_project_root, Cli};

use super::resolve::parse_target;

/// Resolve a target to a chunk ID by searching by name and optionally filtering by file
fn resolve_target(store: &Store, target: &str) -> Result<(String, String)> {
    let (file_filter, name) = parse_target(target);

    let results = store.search_by_name(name, 20)?;
    if results.is_empty() {
        bail!(
            "No function found matching '{}'. Check the name and try again.",
            name
        );
    }

    // Filter by file if specified
    let matched = if let Some(file) = file_filter {
        results.iter().find(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };

    let result = matched.unwrap_or(&results[0]);
    Ok((result.chunk.id.clone(), result.chunk.name.clone()))
}

pub(crate) fn cmd_similar(
    cli: &Cli,
    target: &str,
    limit: usize,
    threshold: f32,
    json: bool,
) -> Result<()> {
    let root = find_project_root();
    let cq_dir = root.join(".cq");
    let index_path = cq_dir.join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;

    // Resolve target to chunk
    let (chunk_id, chunk_name) = resolve_target(&store, target)?;

    // Fetch embedding for the target chunk
    let (source_chunk, embedding) =
        store
            .get_chunk_with_embedding(&chunk_id)?
            .with_context(|| {
                format!(
                    "Could not load embedding for '{}'. Index may be corrupt.",
                    chunk_name
                )
            })?;

    // Build search filter (code only, no notes)
    let languages = match &cli.lang {
        Some(l) => Some(vec![l.parse().context(
            "Invalid language. Valid: rust, python, typescript, javascript, go, c, java",
        )?]),
        None => None,
    };

    let filter = SearchFilter {
        languages,
        chunk_types: None,
        path_pattern: cli.path.clone(),
        name_boost: 0.0, // Pure embedding similarity
        query_text: String::new(),
        enable_rrf: false, // No RRF — direct embedding comparison
        note_weight: 0.0,  // Code only
        note_only: false,
    };

    // Load vector index
    let index = HnswIndex::try_load(&cq_dir);

    // Search with the chunk's embedding as query (request one extra to exclude self)
    let results = store.search_filtered_with_index(
        &embedding,
        &filter,
        limit + 1,
        threshold,
        index.as_deref(),
    )?;

    // Exclude the source chunk
    let filtered: Vec<_> = results
        .into_iter()
        .filter(|r| r.chunk.id != source_chunk.id)
        .take(limit)
        .collect();

    if filtered.is_empty() {
        if json {
            println!(r#"{{"results":[],"target":"{}","total":0}}"#, chunk_name);
        } else {
            println!("No similar functions found for '{}'.", chunk_name);
        }
        return Ok(());
    }

    if json {
        display::display_similar_results_json(&filtered, &chunk_name)?;
    } else {
        if !cli.quiet {
            println!(
                "Similar to '{}' ({}):",
                chunk_name,
                source_chunk.file.display()
            );
            println!();
        }
        let unified: Vec<cqs::store::UnifiedResult> = filtered
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect();
        display::display_unified_results(&unified, &root, cli.no_content, cli.context)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_target_name_only() {
        let (file, name) = parse_target("search_filtered");
        assert_eq!(file, None);
        assert_eq!(name, "search_filtered");
    }

    #[test]
    fn test_parse_target_file_and_name() {
        let (file, name) = parse_target("src/search.rs:search_filtered");
        assert_eq!(file, Some("src/search.rs"));
        assert_eq!(name, "search_filtered");
    }

    #[test]
    fn test_parse_target_nested_path() {
        let (file, name) = parse_target("src/mcp/tools/search.rs:tool_search");
        assert_eq!(file, Some("src/mcp/tools/search.rs"));
        assert_eq!(name, "tool_search");
    }

    #[test]
    fn test_parse_target_empty_name_fallback() {
        // Trailing colon — stripped per P1 F11 fix
        let (file, name) = parse_target("something:");
        assert_eq!(file, None);
        assert_eq!(name, "something");
    }

    #[test]
    fn test_parse_target_leading_colon_fallback() {
        // Leading colon — treat entire string as name
        let (file, name) = parse_target(":name");
        assert_eq!(file, None);
        assert_eq!(name, ":name");
    }
}
