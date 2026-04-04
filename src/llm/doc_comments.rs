//! Doc comment generation pass — candidate selection, batch submission, result assembly.

use std::collections::HashMap;

use super::batch::BatchPhase2;
use super::{max_content_chars, LlmConfig, LlmError, MAX_BATCH_SIZE};
use crate::store::ChunkSummary;
use crate::Store;

/// Signal words that indicate an intentional doc comment, even if short.
/// These words (case-insensitive) mark comments that carry meaningful safety,
/// maintenance, or deprecation signals. A short doc containing any of these
/// should be preserved rather than replaced by LLM-generated text.
const SIGNAL_WORDS: &[&str] = &[
    "SAFETY",
    "UNSAFE",
    "INVARIANT",
    "TODO",
    "FIXME",
    "HACK",
    "NOTE",
    "XXX",
    "BUG",
    "DEPRECATED",
    "SECURITY",
    "WARN",
];

/// Determine whether a chunk needs an LLM-generated doc comment.
/// Returns `true` when the chunk is a callable (function/method/property/macro),
/// is the first window (or not windowed), and has either no doc comment or a
/// "thin" doc (fewer than 30 characters with no signal words).
/// Check if a chunk should be skipped for doc comment generation.
/// Skips test functions (by name or file path) and non-source files
/// (docs, config, markdown) that may contain code-like chunks but
/// shouldn't have doc comments injected.
/// Delegates to the canonical `crate::is_test_chunk` plus content-based markers (EX-14).
/// The canonical function checks name patterns and file paths. We add content-based
/// checks for test attributes/annotations since doc comments are never useful on tests.
fn is_test_chunk(chunk: &ChunkSummary) -> bool {
    let path = chunk.file.to_string_lossy();
    if crate::is_test_chunk(&chunk.name, &path) {
        return true;
    }
    // Content-based markers: test attributes that the name/path heuristics miss
    chunk.content.contains("#[test]") || chunk.content.contains("#[cfg(test)]")
}

/// Check if a chunk is in a writable source file (not docs, config, etc.).
/// Uses the language registry's supported extensions instead of a hardcoded list (EX-13).
/// Excludes `docs/` directories and data-format languages (JSON, XML, YAML, TOML, INI,
/// Markdown, HTML, CSS, Nix, Make, LaTeX, ASP.NET) that shouldn't have doc comments injected.
fn is_source_file(chunk: &ChunkSummary) -> bool {
    use crate::language::REGISTRY;

    let path = chunk.file.to_string_lossy();

    // Exclude docs/ directories
    if path.starts_with("docs/") || path.contains("/docs/") {
        return false;
    }

    // Extract extension from path
    let ext = match std::path::Path::new(path.as_ref())
        .extension()
        .and_then(|e| e.to_str())
    {
        Some(e) => e,
        None => return false,
    };

    // Check if the registry knows this extension
    let def = match REGISTRY.from_extension(ext) {
        Some(d) => d,
        None => return false,
    };

    // Exclude data-format languages that shouldn't have doc comments
    const DATA_FORMAT_LANGS: &[&str] = &[
        "json", "xml", "yaml", "toml", "ini", "markdown", "html", "css", "nix", "make", "latex",
        "aspx",
    ];
    !DATA_FORMAT_LANGS.contains(&def.name)
}

/// Determines whether a code chunk needs a documentation comment.
/// Returns `true` if the chunk is a callable, non-test item from a source file that either
/// lacks documentation or has inadequate documentation (less than 30 characters and no signal
/// words like "TODO" or "FIXME"). Only the first window of windowed chunks is considered
/// eligible.
/// # Arguments
/// * `chunk` - A reference to the ChunkSummary to evaluate
/// # Returns
/// true if the chunk should receive a generated doc comment, false otherwise
pub fn needs_doc_comment(chunk: &ChunkSummary) -> bool {
    // Only callable types get doc comments
    if !chunk.chunk_type.is_callable() {
        return false;
    }

    // Only first window (or non-windowed)
    if chunk.window_idx.is_some_and(|idx| idx > 0) {
        return false;
    }

    // Skip test functions and non-source files
    if is_test_chunk(chunk) || !is_source_file(chunk) {
        return false;
    }

    match &chunk.doc {
        None => true,
        Some(doc) => {
            let trimmed = doc.trim();
            if trimmed.is_empty() {
                return true;
            }
            // Adequate doc — no replacement needed
            if trimmed.len() >= 30 {
                return false;
            }
            // Thin doc — check for signal words before replacing
            let upper = trimmed.to_uppercase();
            !SIGNAL_WORDS.iter().any(|w| upper.contains(w))
        }
    }
}

/// Run the LLM doc-comment generation pass using the Batches API.
/// Scans all indexed chunks, selects those needing doc comments (via `needs_doc_comment`),
/// checks the cache, submits uncached candidates as a batch to Claude with `build_doc_prompt`,
/// and returns the results. Cached results are returned without an API call.
/// `max_docs` limits how many functions to process (0 = unlimited).
/// `improve_all` regenerates docs for all functions, even those with existing adequate docs.
pub fn doc_comment_pass(
    store: &Store,
    config: &crate::config::Config,
    max_docs: usize,
    improve_all: bool,
    lock_dir: Option<&std::path::Path>,
) -> Result<Vec<crate::doc_writer::DocCommentResult>, LlmError> {
    let _span = tracing::info_span!("doc_comment_pass").entered();

    let llm_config = LlmConfig::resolve(config);
    tracing::debug!(
        api_base = %llm_config.api_base,
        "LLM API base"
    );
    tracing::info!(
        model = %llm_config.model,
        "Doc comment pass starting"
    );

    let client = super::create_client(llm_config)?;

    // Phase 1: Collect candidates
    let mut candidates: Vec<ChunkSummary> = Vec::new();
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    let stats = store.stats()?;
    tracing::info!(
        chunks = stats.total_chunks,
        "Scanning for doc comment candidates"
    );

    loop {
        let (chunks, next) = store.chunks_paged(cursor, PAGE_SIZE)?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        for cs in chunks {
            if improve_all {
                // In improve-all mode, include all callable non-test source chunks
                if cs.chunk_type.is_callable()
                    && cs.window_idx.is_none_or(|idx| idx == 0)
                    && !is_test_chunk(&cs)
                    && is_source_file(&cs)
                {
                    candidates.push(cs);
                }
            } else if needs_doc_comment(&cs) {
                candidates.push(cs);
            }
        }
    }

    tracing::info!(
        candidates = candidates.len(),
        "Doc comment candidates found"
    );

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Check cache — filter out already-generated docs
    let hashes: Vec<&str> = candidates.iter().map(|c| c.content_hash.as_str()).collect();
    let cached = store.get_summaries_by_hashes(&hashes, "doc-comment")?;

    // Split into cached hits and uncached misses
    let mut cached_results: Vec<(&ChunkSummary, String)> = Vec::new();
    let mut uncached: Vec<&ChunkSummary> = Vec::new();

    for c in &candidates {
        if let Some(doc) = cached.get(&c.content_hash) {
            cached_results.push((c, doc.clone()));
        } else {
            uncached.push(c);
        }
    }

    tracing::info!(
        cached = cached_results.len(),
        uncached = uncached.len(),
        "Cache check complete"
    );

    // Sort: no doc first, then thin doc, by content length descending (meatier functions first)
    uncached.sort_by(|a, b| {
        let a_no_doc = a.doc.as_ref().is_none_or(|d| d.trim().is_empty());
        let b_no_doc = b.doc.as_ref().is_none_or(|d| d.trim().is_empty());
        // no-doc before thin-doc
        b_no_doc
            .cmp(&a_no_doc)
            .then_with(|| b.content.len().cmp(&a.content.len()))
    });

    // Apply max_docs cap (across cached + uncached)
    let total_available = cached_results.len() + uncached.len();
    let effective_cap = if max_docs == 0 {
        total_available
    } else {
        max_docs
    };

    // Cached results count toward the cap first
    let cached_to_use = cached_results.len().min(effective_cap);
    let uncached_cap = effective_cap.saturating_sub(cached_to_use);
    uncached.truncate(uncached_cap);

    // Phase 2: Submit batch for uncached candidates (or resume pending)
    let batch_items: Vec<super::provider::BatchSubmitItem> = {
        let mut items = Vec::new();
        let mut queued_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
        for cs in &uncached {
            if queued_hashes.insert(cs.content_hash.clone()) {
                let content = if cs.content.len() > max_content_chars() {
                    cs.content[..cs.content.floor_char_boundary(max_content_chars())].to_string()
                } else {
                    cs.content.clone()
                };
                items.push(super::provider::BatchSubmitItem {
                    custom_id: cs.content_hash.clone(),
                    content,
                    context: cs.chunk_type.to_string(),
                    language: cs.language.to_string(),
                });
                if items.len() >= MAX_BATCH_SIZE {
                    break;
                }
            }
        }
        items
    };

    let phase2 = BatchPhase2 {
        purpose: "doc-comment",
        max_tokens: 800,
        quiet: false,
        lock_dir,
    };
    let api_results: HashMap<String, String> = phase2.submit_or_resume(
        &client,
        store,
        &batch_items,
        &|s| s.get_pending_doc_batch_id(),
        &|s, id| s.set_pending_doc_batch_id(id),
        &|c, items, max_tok| c.submit_doc_batch(items, max_tok),
    )?;

    // Phase 3: Build results from cached + API responses
    // Deduplicate by content_hash: multiple chunks can share the same hash
    // (windowed chunks, same function body). One doc comment per unique function.
    let mut results: Vec<crate::doc_writer::DocCommentResult> = Vec::new();
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (cs, doc) in cached_results.iter().take(cached_to_use) {
        if !seen_hashes.insert(cs.content_hash.clone()) {
            continue;
        }
        results.push(crate::doc_writer::DocCommentResult {
            file: cs.file.clone(),
            function_name: cs.name.clone(),
            content_hash: cs.content_hash.clone(),
            generated_doc: doc.clone(),
            language: cs.language,
            line_start: cs.line_start as usize,
            had_existing_doc: cs.doc.as_ref().is_some_and(|d| !d.trim().is_empty()),
        });
    }

    for cs in &uncached {
        if seen_hashes.contains(&cs.content_hash) {
            continue;
        }
        if let Some(doc) = api_results.get(&cs.content_hash) {
            seen_hashes.insert(cs.content_hash.clone());
            results.push(crate::doc_writer::DocCommentResult {
                file: cs.file.clone(),
                function_name: cs.name.clone(),
                content_hash: cs.content_hash.clone(),
                generated_doc: doc.clone(),
                language: cs.language,
                line_start: cs.line_start as usize,
                had_existing_doc: cs.doc.as_ref().is_some_and(|d| !d.trim().is_empty()),
            });
        }
    }

    tracing::info!(
        total = results.len(),
        cached = cached_to_use,
        api_generated = api_results.len(),
        "Doc comment pass complete"
    );

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ChunkType, Language};

    /// Helper: build a minimal ChunkSummary with the given fields.
    fn make_chunk(
        chunk_type: ChunkType,
        doc: Option<&str>,
        window_idx: Option<i32>,
    ) -> ChunkSummary {
        ChunkSummary {
            id: "mod::my_func".to_string(),
            file: std::path::PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type,
            name: "my_func".to_string(),
            signature: "fn my_func()".to_string(),
            content: "fn my_func() { todo!() }".to_string(),
            doc: doc.map(|s| s.to_string()),
            line_start: 1,
            line_end: 3,
            content_hash: "abc123".to_string(),
            window_idx,
            parent_id: None,
            parent_type_name: None,
        }
    }

    fn make_chunk_for_test(file: &str, language: Language) -> ChunkSummary {
        ChunkSummary {
            id: "test".to_string(),
            file: std::path::PathBuf::from(file),
            language,
            chunk_type: ChunkType::Function,
            name: "test_fn".to_string(),
            signature: String::new(),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 10,
            content_hash: String::new(),
            window_idx: None,
            parent_id: None,
            parent_type_name: None,
        }
    }

    // ===== needs_doc_comment tests =====

    #[test]
    fn test_needs_doc_comment_no_doc() {
        let chunk = make_chunk(ChunkType::Function, None, None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_thin() {
        // Doc under 30 chars with no signal words
        let chunk = make_chunk(ChunkType::Function, Some("A short doc"), None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_signal_words() {
        // Thin doc but contains SAFETY signal word — preserve it
        let chunk = make_chunk(ChunkType::Function, Some("/// SAFETY: requires lock"), None);
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_adequate() {
        // Doc >= 30 chars — no replacement needed
        let chunk = make_chunk(
            ChunkType::Function,
            Some("Parse a configuration file from disk and validate all fields."),
            None,
        );
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_not_callable() {
        // Struct/Enum are not callable — should return false
        let chunk = make_chunk(ChunkType::Struct, None, None);
        assert!(!needs_doc_comment(&chunk));

        let chunk = make_chunk(ChunkType::Enum, None, None);
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_window_idx_nonzero() {
        // Non-first window — skip
        let chunk = make_chunk(ChunkType::Function, None, Some(1));
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_window_idx_zero() {
        // First window — should be considered
        let chunk = make_chunk(ChunkType::Function, None, Some(0));
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_empty_doc() {
        // Empty string doc — same as no doc
        let chunk = make_chunk(ChunkType::Function, Some(""), None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_whitespace_doc() {
        // Whitespace-only doc — same as no doc
        let chunk = make_chunk(ChunkType::Function, Some("   \n  "), None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_method() {
        // Method is callable — should be considered
        let chunk = make_chunk(ChunkType::Method, None, None);
        assert!(needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_signal_word_case_insensitive() {
        // Signal words are case-insensitive
        let chunk = make_chunk(ChunkType::Function, Some("todo: fix this"), None);
        assert!(!needs_doc_comment(&chunk));

        let chunk = make_chunk(ChunkType::Function, Some("Deprecated"), None);
        assert!(!needs_doc_comment(&chunk));

        let chunk = make_chunk(ChunkType::Function, Some("FIXME later"), None);
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_all_signal_words() {
        for word in SIGNAL_WORDS {
            let doc = format!("Has {}", word);
            let chunk = make_chunk(ChunkType::Function, Some(&doc), None);
            assert!(
                !needs_doc_comment(&chunk),
                "Signal word '{}' should prevent replacement",
                word
            );
        }
    }

    // ===== test function skip tests =====

    #[test]
    fn test_needs_doc_comment_skips_test_prefix() {
        let mut chunk = make_chunk(ChunkType::Function, None, None);
        chunk.name = "test_something".to_string();
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_skips_tests_dir() {
        let mut chunk = make_chunk(ChunkType::Function, None, None);
        chunk.file = std::path::PathBuf::from("tests/integration.rs");
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_skips_test_rs_suffix() {
        let mut chunk = make_chunk(ChunkType::Function, None, None);
        chunk.file = std::path::PathBuf::from("src/store_test.rs");
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_skips_test_attr_in_content() {
        let mut chunk = make_chunk(ChunkType::Function, None, None);
        chunk.name = "parse_source_extracts_functions".to_string();
        chunk.content = "#[test]\nfn parse_source_extracts_functions() { }".to_string();
        assert!(!needs_doc_comment(&chunk));
    }

    #[test]
    fn test_needs_doc_comment_skips_cfg_test_in_content() {
        let mut chunk = make_chunk(ChunkType::Function, None, None);
        chunk.name = "my_module_tests".to_string();
        chunk.content = "#[cfg(test)]\nmod tests { }".to_string();
        assert!(!needs_doc_comment(&chunk));
    }

    // ===== is_source_file tests =====

    #[test]
    fn test_is_source_file_rust() {
        let chunk = make_chunk_for_test("src/main.rs", Language::Rust);
        assert!(is_source_file(&chunk), "Rust files should be source files");
    }

    #[test]
    fn test_is_source_file_docs_excluded() {
        let chunk = make_chunk_for_test("docs/guide.rs", Language::Rust);
        assert!(
            !is_source_file(&chunk),
            "docs/ directories should be excluded"
        );
    }

    #[test]
    fn test_is_source_file_non_source_extension() {
        let chunk = make_chunk_for_test("data/config.json", Language::Json);
        assert!(
            !is_source_file(&chunk),
            "JSON files should not be source files"
        );
    }

    #[test]
    fn test_is_source_file_no_extension() {
        let mut chunk = make_chunk_for_test("Makefile", Language::Rust);
        chunk.file = std::path::PathBuf::from("Makefile");
        assert!(
            !is_source_file(&chunk),
            "Files without extensions should not be source files"
        );
    }

    // TC-5 (needs_doc_comment): non-source file should return false
    #[test]
    fn test_needs_doc_comment_non_source_file() {
        let chunk = make_chunk_for_test("docs/example.md", Language::Markdown);
        assert!(
            !needs_doc_comment(&chunk),
            "Non-source file should not need doc comment"
        );
    }

    #[test]
    fn test_needs_doc_comment_non_callable() {
        let mut chunk = make_chunk_for_test("src/lib.rs", Language::Rust);
        chunk.chunk_type = ChunkType::Struct;
        assert!(
            !needs_doc_comment(&chunk),
            "Non-callable chunk type should not need doc comment"
        );
    }
}
