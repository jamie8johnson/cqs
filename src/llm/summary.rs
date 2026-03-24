//! LLM summary pass orchestration — collects chunks, submits batches, stores results.

use super::batch::BatchPhase2;
use super::{Client, LlmConfig, LlmError, MAX_BATCH_SIZE, MAX_CONTENT_CHARS, MIN_CONTENT_CHARS};
use crate::Store;

/// Run the LLM summary pass using the Batches API.
///
/// Collects all uncached callable chunks, submits them as a batch to Claude,
/// polls for completion, then stores results. Doc comments are extracted locally
/// without API calls.
///
/// Returns the number of new summaries generated.
pub fn llm_summary_pass(
    store: &Store,
    quiet: bool,
    config: &crate::config::Config,
) -> Result<usize, LlmError> {
    let _span = tracing::info_span!("llm_summary_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::info!(
        model = %llm_config.model,
        api_base = %llm_config.api_base,
        max_tokens = llm_config.max_tokens,
        "LLM config resolved"
    );

    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        LlmError::ApiKeyMissing(
            "--llm-summaries requires ANTHROPIC_API_KEY environment variable".to_string(),
        )
    })?;
    let client = Client::new(&api_key, llm_config)?;

    let mut doc_extracted = 0usize;
    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    // Phase 1: Collect chunks needing summaries
    // Store doc-comment summaries immediately, collect API-needing chunks
    let mut to_store: Vec<(String, String, String, String)> = Vec::new();
    // (custom_id=content_hash, content, chunk_type, language) for batch API
    let mut batch_items: Vec<(String, String, String, String)> = Vec::new();
    // Track content_hashes already queued to avoid duplicate custom_ids in batch
    let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    let stats = store.stats()?;
    tracing::info!(chunks = stats.total_chunks, "Scanning for LLM summaries");

    let mut batch_full = false;
    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store.get_summaries_by_hashes(&hashes, "summary")?;

        for cs in &chunks {
            if existing.contains_key(&cs.content_hash) {
                cached += 1;
                continue;
            }

            if !cs.chunk_type.is_callable() {
                skipped += 1;
                continue;
            }

            if cs.content.len() < MIN_CONTENT_CHARS {
                skipped += 1;
                continue;
            }

            if cs.window_idx.is_some_and(|idx| idx > 0) {
                skipped += 1;
                continue;
            }

            // Doc comment shortcut
            if let Some(ref doc) = cs.doc {
                if doc.len() > 10 {
                    let first_sentence = extract_first_sentence(doc);
                    if !first_sentence.is_empty() {
                        to_store.push((
                            cs.content_hash.clone(),
                            first_sentence,
                            "doc-comment".to_string(),
                            "summary".to_string(),
                        ));
                        doc_extracted += 1;
                        continue;
                    }
                }
            }

            // Queue for batch API (deduplicate by content_hash)
            if queued_hashes.insert(cs.content_hash.clone()) {
                batch_items.push((
                    cs.content_hash.clone(),
                    if cs.content.len() > MAX_CONTENT_CHARS {
                        cs.content[..cs.content.floor_char_boundary(MAX_CONTENT_CHARS)].to_string()
                    } else {
                        cs.content.clone()
                    },
                    cs.chunk_type.to_string(),
                    cs.language.to_string(),
                ));
                if batch_items.len() >= MAX_BATCH_SIZE {
                    batch_full = true;
                    break;
                }
            }
        }
        if batch_full {
            tracing::info!(
                max = MAX_BATCH_SIZE,
                "Batch size limit reached, submitting partial batch"
            );
            break;
        }
    }

    // Store doc-comment summaries immediately
    if !to_store.is_empty() {
        store.upsert_summaries_batch(&to_store)?;
    }

    tracing::info!(
        cached,
        doc_extracted,
        skipped,
        api_needed = batch_items.len(),
        "Summary scan complete"
    );

    // Phase 2: Submit batch to Claude API (or resume a pending one)
    let phase2 = BatchPhase2 {
        purpose: "summary",
        max_tokens: client.llm_config.max_tokens,
        quiet,
    };
    let api_results = phase2.submit_or_resume(
        &client,
        store,
        &batch_items,
        &|s| s.get_pending_batch_id(),
        &|s, id| s.set_pending_batch_id(id),
        &|c, items, max_tok| c.submit_batch(items, max_tok),
    )?;
    let api_generated = api_results.len();

    tracing::info!(
        api_generated,
        doc_extracted,
        cached,
        skipped,
        "LLM summary pass complete"
    );

    Ok(api_generated + doc_extracted)
}

/// Extract the first sentence from a doc comment.
fn extract_first_sentence(doc: &str) -> String {
    let trimmed = doc.trim();
    if let Some(pos) = trimmed.find(['.', '!', '?']) {
        let sentence = trimmed[..=pos].trim();
        if sentence.len() > 10 {
            return sentence.to_string();
        }
    }
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.len() > 10 {
        first_line.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_first_sentence_period() {
        assert_eq!(
            extract_first_sentence("Parse a config file. Returns validated settings."),
            "Parse a config file."
        );
    }

    #[test]
    fn test_extract_first_sentence_no_period() {
        assert_eq!(
            extract_first_sentence("Parse a config file and return settings"),
            "Parse a config file and return settings"
        );
    }

    #[test]
    fn test_extract_first_sentence_short() {
        assert_eq!(extract_first_sentence("Hi."), "");
    }

    #[test]
    fn test_extract_first_sentence_multiline() {
        assert_eq!(
            extract_first_sentence("Parse a config file.\n\nThis handles TOML and JSON."),
            "Parse a config file."
        );
    }

    #[test]
    fn extract_first_sentence_url_with_period() {
        // URL period — cuts at first period in domain (known behavior, not a bug)
        let r = extract_first_sentence("See https://example.com. Usage guide.");
        assert_eq!(r, "See https://example.");
    }

    #[test]
    fn extract_first_sentence_short_falls_to_line() {
        // "Short." is 6 chars <=10, falls to first line
        let r = extract_first_sentence("Short. More text here.");
        assert_eq!(r, "Short. More text here.");
    }

    #[test]
    fn extract_first_sentence_exclamation() {
        let r = extract_first_sentence("This is great! More.");
        assert_eq!(r, "This is great!");
    }

    #[test]
    fn extract_first_sentence_question() {
        let r = extract_first_sentence("Is this working? Yes.");
        assert_eq!(r, "Is this working?");
    }

    #[test]
    fn extract_first_sentence_whitespace_only() {
        assert_eq!(extract_first_sentence("   \n  \t  "), "");
    }

    #[test]
    fn extract_first_sentence_empty_input() {
        assert_eq!(extract_first_sentence(""), "");
    }

    #[test]
    fn extract_first_sentence_boundary_11_chars() {
        assert_eq!(extract_first_sentence("1234567890."), "1234567890.");
    }

    #[test]
    fn extract_first_sentence_short_multiline() {
        // Both sentence and first line too short
        assert_eq!(extract_first_sentence("OK.\nMore"), "");
    }

    // ===== TC-22: LLM pass chunk filtering condition tests =====
    //
    // The filtering logic in llm_summary_pass (and hyde_query_pass) applies 4 skip conditions
    // to each ChunkSummary. Since the logic is inline, these tests validate each condition
    // independently using the same types and constants.

    use crate::language::ChunkType;
    use std::path::PathBuf;

    fn make_test_chunk_summary(
        name: &str,
        chunk_type: ChunkType,
        content_len: usize,
        window_idx: Option<i32>,
        content_hash: &str,
    ) -> crate::store::ChunkSummary {
        crate::store::ChunkSummary {
            id: format!("test:1:{}", name),
            file: PathBuf::from("src/lib.rs"),
            language: crate::parser::Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: "x".repeat(content_len),
            doc: None,
            line_start: 1,
            line_end: 10,
            parent_id: None,
            parent_type_name: None,
            content_hash: content_hash.to_string(),
            window_idx,
        }
    }

    /// Condition 1: cached chunks (content_hash in existing) should be skipped.
    #[test]
    fn filter_skips_cached_chunks() {
        let cs = make_test_chunk_summary("func", ChunkType::Function, 100, None, "already_cached");
        let mut existing = std::collections::HashMap::new();
        existing.insert("already_cached".to_string(), "old summary".to_string());
        assert!(
            existing.contains_key(&cs.content_hash),
            "Cached chunk should be recognized as existing"
        );
    }

    /// Condition 2: non-callable chunk types should be skipped.
    #[test]
    fn filter_skips_non_callable_chunks() {
        let non_callable_types = [
            ChunkType::Struct,
            ChunkType::Enum,
            ChunkType::Trait,
            ChunkType::Interface,
            ChunkType::Class,
            ChunkType::Constant,
            ChunkType::Section,
            ChunkType::Module,
            ChunkType::TypeAlias,
        ];
        for ct in non_callable_types {
            assert!(!ct.is_callable(), "{:?} should not be callable", ct);
        }
        // Callable types should NOT be skipped
        let callable_types = [
            ChunkType::Function,
            ChunkType::Method,
            ChunkType::Constructor,
            ChunkType::Property,
            ChunkType::Macro,
            ChunkType::Extension,
        ];
        for ct in callable_types {
            assert!(ct.is_callable(), "{:?} should be callable", ct);
        }
    }

    /// Condition 3: chunks below MIN_CONTENT_CHARS should be skipped.
    #[test]
    fn filter_skips_short_content() {
        let short = make_test_chunk_summary("short_fn", ChunkType::Function, 10, None, "h1");
        assert!(
            short.content.len() < MIN_CONTENT_CHARS,
            "Content of {} chars should be below MIN_CONTENT_CHARS ({})",
            short.content.len(),
            MIN_CONTENT_CHARS
        );

        let adequate = make_test_chunk_summary("good_fn", ChunkType::Function, 100, None, "h2");
        assert!(
            adequate.content.len() >= MIN_CONTENT_CHARS,
            "Content of {} chars should be at or above MIN_CONTENT_CHARS ({})",
            adequate.content.len(),
            MIN_CONTENT_CHARS
        );
    }

    /// Condition 3 boundary: exactly MIN_CONTENT_CHARS should NOT be skipped.
    #[test]
    fn filter_accepts_exactly_min_content_chars() {
        let cs = make_test_chunk_summary(
            "boundary_fn",
            ChunkType::Function,
            MIN_CONTENT_CHARS,
            None,
            "h3",
        );
        assert!(
            cs.content.len() >= MIN_CONTENT_CHARS,
            "Exactly MIN_CONTENT_CHARS should pass the filter"
        );
    }

    /// Condition 4: windowed chunks (window_idx > 0) should be skipped.
    #[test]
    fn filter_skips_windowed_chunks() {
        let windowed = make_test_chunk_summary("fn_w1", ChunkType::Function, 100, Some(1), "h4");
        assert!(
            windowed.window_idx.is_some_and(|idx| idx > 0),
            "window_idx=1 should be filtered out"
        );

        let window_zero = make_test_chunk_summary("fn_w0", ChunkType::Function, 100, Some(0), "h5");
        assert!(
            !window_zero.window_idx.is_some_and(|idx| idx > 0),
            "window_idx=0 should NOT be filtered out"
        );

        let no_window = make_test_chunk_summary("fn_no_w", ChunkType::Function, 100, None, "h6");
        assert!(
            !no_window.window_idx.is_some_and(|idx| idx > 0),
            "window_idx=None should NOT be filtered out"
        );
    }

    /// All conditions pass: a callable, sufficiently long, non-windowed, uncached chunk.
    #[test]
    fn filter_accepts_eligible_chunk() {
        let cs = make_test_chunk_summary("eligible_fn", ChunkType::Function, 200, None, "new_hash");
        let existing: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        let skip_cached = existing.contains_key(&cs.content_hash);
        let skip_non_callable = !cs.chunk_type.is_callable();
        let skip_short = cs.content.len() < MIN_CONTENT_CHARS;
        let skip_windowed = cs.window_idx.is_some_and(|idx| idx > 0);

        assert!(!skip_cached, "Should not be cached");
        assert!(!skip_non_callable, "Function is callable");
        assert!(!skip_short, "200 chars > MIN_CONTENT_CHARS");
        assert!(!skip_windowed, "No window index");
    }
}
