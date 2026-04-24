//! Similar command — find code similar to a given function
//!
//! TODO(json-schema): Extract typed SimilarOutput struct. Depends on display
//! module refactoring — similar results use display::display_similar_results_json
//! which builds JSON inline. Blocked until display module has typed output structs.

use anyhow::{Context, Result};

use cqs::{HnswIndex, SearchFilter};

use crate::cli::display;

pub(crate) fn cmd_similar(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
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
    // CQ-V1.25-2: clamp via shared constant so CLI and batch return the
    // same number of results. Previously CLI had no clamp; `cqs -n 10000`
    // cascaded into unbounded allocation in search_filtered_with_index.
    let limit = limit.clamp(1, crate::cli::SIMILAR_LIMIT_MAX);

    // CQ-V1.29-3: resolve via the shared `cqs::resolve_target` helper (already
    // used by `cmd_neighbors` and `dispatch_similar`). The previous local
    // `resolve_target` picked `results[0]`, which surfaced test chunks first
    // when names collided — CLI and batch/daemon returned different answers
    // for the same target.
    let resolved = cqs::resolve_target(store, name)?;
    let chunk_id = resolved.chunk.id.clone();
    let chunk_name = resolved.chunk.name.clone();

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
    let index = HnswIndex::try_load_with_ef(cqs_dir, None, store.dim());

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
            crate::cli::json_envelope::emit_json(&obj)?;
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
    // CQ-V1.29-3: `parse_target` is re-exported from the library via
    // `cqs::parse_target`. These coverage tests stayed in this file with the
    // removal of the CLI-local `resolve_target` helper — no equivalent direct
    // `parse_target` tests exist in `src/search/mod.rs` yet (only
    // `resolve_target` is covered).
    use cqs::parse_target;

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
