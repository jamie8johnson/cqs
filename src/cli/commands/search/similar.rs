//! Similar command — find code similar to a given function

use anyhow::{bail, Context, Result};

use cqs::{HnswIndex, SearchFilter, Store};

use crate::cli::display;

use crate::cli::commands::resolve::parse_target;

/// Resolve a name to a chunk ID by searching by name and optionally filtering by file
fn resolve_target(store: &Store, name: &str) -> Result<(String, String)> {
    let (file_filter, func_name) = parse_target(name);

    let results = store.search_by_name(func_name, 20)?;
    if results.is_empty() {
        bail!(
            "No function found matching '{}'. Check the name and try again.",
            func_name
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
    ctx: &crate::cli::CommandContext,
    name: &str,
    limit: usize,
    threshold: f32,
    json: bool,
) -> Result<()> {
    crate::cli::validate_finite_f32(threshold, "threshold")?;
    let _span = tracing::info_span!("cmd_similar", name).entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    // Resolve name to chunk
    let (chunk_id, chunk_name) = resolve_target(store, name)?;

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
    let languages = match &ctx.cli.lang {
        Some(l) => Some(vec![l.parse().context(format!(
            "Invalid language. Valid: {}",
            cqs::parser::Language::valid_names_display()
        ))?]),
        None => None,
    };

    let filter = SearchFilter {
        languages,
        path_pattern: ctx.cli.path.clone(),
        ..Default::default()
    };

    // Load vector index
    let index = HnswIndex::try_load_with_ef(cqs_dir, None, Some(store.dim()));

    // Search with the chunk's embedding as query (request one extra to exclude self)
    let results = store.search_filtered_with_index(
        &embedding,
        &filter,
        limit.saturating_add(1),
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
            let obj = serde_json::json!({"results": [], "target": chunk_name, "total": 0});
            println!("{}", obj);
        } else {
            println!("No similar functions found for '{}'.", chunk_name);
        }
        return Ok(());
    }

    if json {
        display::display_similar_results_json(&filtered, &chunk_name)?;
    } else {
        if !ctx.cli.quiet {
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
        display::display_unified_results(
            &unified,
            root,
            ctx.cli.no_content,
            ctx.cli.context,
            None,
        )?;
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
        let (file, name) = parse_target("src/cli/commands/query.rs:cmd_query");
        assert_eq!(file, Some("src/cli/commands/query.rs"));
        assert_eq!(name, "cmd_query");
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
