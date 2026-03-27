//! Natural language generation from code chunks.
//!
//! Converts code metadata into natural language descriptions for embedding.
//! Based on Greptile's finding that code->NL->embed improves semantic search.

mod fields;
pub mod fts;
mod markdown;

pub use fields::extract_body_keywords;
pub use fts::{normalize_for_fts, tokenize_identifier};
#[allow(unused_imports)]
pub use markdown::{parse_jsdoc_tags, strip_markdown_noise, JsDocInfo};

use crate::parser::{Chunk, ChunkType, Language};

use fields::{extract_field_names, extract_member_method_names};

/// Template variants for NL description generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NlTemplate {
    /// No prefix + body keywords (production template)
    Compact,
    /// Doc-first: minimal metadata when doc exists, full template when missing
    DocFirst,
}

/// Call graph context for enriching NL descriptions.
///
/// Provided during the second indexing pass, after the call graph is built.
#[derive(Debug, Default)]
pub struct CallContext {
    /// Names of functions that call this chunk (most specific discrimination signal).
    pub callers: Vec<String>,
    /// Names of functions this chunk calls (less discriminating, often shared utilities).
    pub callees: Vec<String>,
}

/// Generate NL description enriched with call graph context.
///
/// Used in the second indexing pass. Appends caller/callee names to the base
/// Compact description, filtered by IDF to suppress high-frequency utilities.
pub fn generate_nl_with_call_context(
    chunk: &Chunk,
    ctx: &CallContext,
    callee_doc_freq: &std::collections::HashMap<String, f32>,
    max_callers: usize,
    max_callees: usize,
) -> String {
    generate_nl_with_call_context_and_summary(
        chunk,
        ctx,
        callee_doc_freq,
        max_callers,
        max_callees,
        None,
        None,
    )
}

/// Generate NL with call context and optional LLM summary (SQ-6).
///
/// If a summary is provided, it's prepended to the NL for maximum embedding weight.
/// If hyde predictions are provided, they're appended as query terms (SQ-12).
pub fn generate_nl_with_call_context_and_summary(
    chunk: &Chunk,
    ctx: &CallContext,
    callee_doc_freq: &std::collections::HashMap<String, f32>,
    max_callers: usize,
    max_callees: usize,
    summary: Option<&str>,
    hyde: Option<&str>,
) -> String {
    let base = generate_nl_description(chunk);

    let mut extras = Vec::new();

    // Callers: most discriminating signal. Tokenize names for embedding.
    if !ctx.callers.is_empty() {
        let caller_words: Vec<String> = ctx
            .callers
            .iter()
            .take(max_callers)
            .map(|c| tokenize_identifier(c).join(" "))
            .collect();
        if !caller_words.is_empty() {
            extras.push(format!("Called by: {}", caller_words.join(", ")));
        }
    }

    // Callees: filter high-frequency utilities (IDF threshold).
    // A callee appearing in >10% of chunks is likely a utility (log, unwrap, etc.).
    // DS-22: Cast to f64 for boundary comparison to avoid f32 non-determinism.
    if !ctx.callees.is_empty() {
        let callee_words: Vec<String> = ctx
            .callees
            .iter()
            .filter(|c| {
                !callee_doc_freq
                    .get(c.as_str())
                    .is_some_and(|&freq| (freq as f64) >= 0.10_f64)
            })
            .take(max_callees)
            .map(|c| tokenize_identifier(c).join(" "))
            .collect();
        if !callee_words.is_empty() {
            extras.push(format!("Calls: {}", callee_words.join(", ")));
        }
    }

    let nl = if extras.is_empty() {
        base
    } else {
        format!("{}. {}", base, extras.join(". "))
    };

    // Prepend LLM summary if available (SQ-6)
    let nl = match summary {
        Some(s) if !s.is_empty() => format!("{} {}", s, nl),
        _ => nl,
    };

    // Append hyde query predictions (SQ-12)
    match hyde {
        Some(h) if !h.is_empty() => {
            let queries: String = h
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if queries.is_empty() {
                nl
            } else {
                format!("{}. Queries: {}", nl, queries)
            }
        }
        _ => nl,
    }
}

/// Generate natural language description from chunk metadata.
///
/// Produces text like: "parse config. Takes path parameter. Returns config. Keywords: path, config."
///
/// # Example
///
/// ```
/// use cqs::generate_nl_description;
/// use cqs::parser::{Chunk, ChunkType, Language};
/// use std::path::PathBuf;
///
/// let chunk = Chunk {
///     id: "test.rs:1:abcd1234".to_string(),
///     file: PathBuf::from("test.rs"),
///     language: Language::Rust,
///     chunk_type: ChunkType::Function,
///     name: "parseConfig".to_string(),
///     signature: "fn parseConfig(path: &str) -> Config".to_string(),
///     content: "fn parseConfig(path: &str) -> Config { ... }".to_string(),
///     line_start: 1,
///     line_end: 5,
///     doc: Some("/// Parse configuration from file".to_string()),
///     content_hash: "abcd1234".to_string(),
///     parent_id: None,
///     window_idx: None,
///     parent_type_name: None,
/// };
///
/// let nl = generate_nl_description(&chunk);
/// assert!(nl.contains("parse config"));
/// assert!(nl.contains("Parse configuration"));
/// ```
pub fn generate_nl_description(chunk: &Chunk) -> String {
    generate_nl_with_template(chunk, NlTemplate::Compact)
}

/// Generate NL description using a specific template variant.
pub fn generate_nl_with_template(chunk: &Chunk, template: NlTemplate) -> String {
    // Section chunks (markdown): breadcrumb + name + content preview.
    // Markdown IS natural language, so we embed more content than code chunks.
    // E5-base-v2 handles ~512 tokens (~2000 chars). Budget:
    //   breadcrumb ~25 tokens + name ~12 tokens + preview ~450 tokens = ~487 tokens.
    if chunk.chunk_type == ChunkType::Section {
        let mut parts = Vec::new();
        if !chunk.signature.is_empty() {
            parts.push(chunk.signature.clone());
        }
        parts.push(chunk.name.clone());
        let preview: String = strip_markdown_noise(&chunk.content)
            .chars()
            .take(1800)
            .collect();
        parts.push(preview);
        return parts.join(". ");
    }

    let mut parts = Vec::new();

    // Compact enrichment: file path + module context for discrimination.
    // Includes directory components and filename stem (SQ-5).
    // Generic stems (mod, index, lib, utils) are filtered.
    if template == NlTemplate::Compact {
        let file_context = extract_file_context(&chunk.file);
        if !file_context.is_empty() {
            parts.push(file_context);
        }
    }

    // Shared: doc comment
    let has_doc = if let Some(ref doc) = chunk.doc {
        let doc_trimmed = doc.trim();
        if !doc_trimmed.is_empty() {
            parts.push(doc_trimmed.to_string());
            true
        } else {
            false
        }
    } else {
        false
    };

    // Shared: tokenized name
    let name_words = tokenize_identifier(&chunk.name).join(" ");

    // DocFirst: minimal metadata when doc exists
    if template == NlTemplate::DocFirst && has_doc {
        parts.push(name_words);
        return parts.join(". ");
    }

    // Parent type context for methods (e.g., "circuit breaker method")
    if chunk.chunk_type == ChunkType::Method {
        if let Some(ref parent_name) = chunk.parent_type_name {
            let parent_words = tokenize_identifier(parent_name).join(" ");
            parts.push(format!("{} method", parent_words));
        }
    }

    // Constructor: "constructor for {parent}" or "constructor that initializes {name}"
    if chunk.chunk_type == ChunkType::Constructor {
        if let Some(ref parent_name) = chunk.parent_type_name {
            let parent_words = tokenize_identifier(parent_name).join(" ");
            parts.push(format!("constructor for {}", parent_words));
        } else {
            let name_tokens = tokenize_identifier(&chunk.name).join(" ");
            parts.push(format!("constructor that initializes {}", name_tokens));
        }
    }

    // Name line (no prefix)
    parts.push(name_words);

    // Extension: "extension of {name}" label
    if chunk.chunk_type == ChunkType::Extension {
        let name_tokens = tokenize_identifier(&chunk.name).join(" ");
        parts.push(format!("extension of {}", name_tokens));
    }

    // Struct/enum/class/extension field names
    if matches!(
        chunk.chunk_type,
        ChunkType::Struct | ChunkType::Enum | ChunkType::Class | ChunkType::Extension
    ) {
        let fields = extract_field_names(&chunk.content, chunk.language);
        if !fields.is_empty() {
            parts.push(format!("Fields: {}", fields.join(", ")));
        }
    }

    // Class/struct/interface/extension: extract member method names for richer NL
    if matches!(
        chunk.chunk_type,
        ChunkType::Class | ChunkType::Struct | ChunkType::Interface | ChunkType::Extension
    ) {
        let methods = extract_member_method_names(&chunk.content, chunk.language);
        if !methods.is_empty() {
            let method_words: Vec<String> = methods
                .iter()
                .take(10)
                .map(|m| tokenize_identifier(m).join(" "))
                .collect();
            parts.push(format!("Methods: {}", method_words.join(", ")));
        }
    }

    // Parameters + return type
    let jsdoc_info = if chunk.language == Language::JavaScript {
        chunk.doc.as_ref().map(|d| parse_jsdoc_tags(d))
    } else {
        None
    };

    if let Some(params_desc) = extract_params_nl(&chunk.signature) {
        parts.push(params_desc);
    } else if let Some(ref info) = jsdoc_info {
        if !info.params.is_empty() {
            let param_strs: Vec<String> = info
                .params
                .iter()
                .map(|(name, ty)| format!("{} ({})", name, ty))
                .collect();
            parts.push(format!("Takes parameters: {}", param_strs.join(", ")));
        }
    }

    if let Some(return_desc) = extract_return_nl(&chunk.signature, chunk.language) {
        parts.push(return_desc);
    } else if let Some(ref info) = jsdoc_info {
        if let Some(ref ret) = info.returns {
            parts.push(format!("Returns {}", ret));
        }
    }

    // Body keywords
    {
        let keywords = extract_body_keywords(&chunk.content, chunk.language);
        if !keywords.is_empty() {
            let kw_strs: Vec<&str> = keywords.iter().map(|s| s.as_str()).collect();
            parts.push(format!("Uses: {}", kw_strs.join(", ")));
        }
    }

    // Type-aware: append full signature for richer type discrimination (SQ-11).
    // Placed last so doc/name tokens retain positional priority in embedding.
    // The full signature captures generic bounds (T: Ord), lifetimes, and
    // complete parameter types that the extracted params/return lose.
    if !chunk.signature.is_empty() {
        parts.push(format!("Signature: {}", chunk.signature));
    }

    parts.join(". ")
}

/// Extract parameter information from signature as natural language.
fn extract_params_nl(signature: &str) -> Option<String> {
    let start = signature.find('(')?;
    let end = signature.rfind(')')?;
    if start >= end {
        return None;
    }
    let params_str = &signature[start + 1..end];

    if params_str.trim().is_empty() {
        return Some("Takes no parameters".to_string());
    }

    // Use iterator chain to avoid intermediate Vec per parameter.
    // Collects once at the end with join (which internally uses a single String buffer).
    let params: String = params_str
        .split(',')
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            // Filter tokens inline without intermediate collect
            let filtered: String = tokenize_identifier(p)
                .into_iter()
                .filter(|w| !["self", "mut"].contains(&w.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            if filtered.is_empty() {
                None
            } else {
                Some(filtered)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    if params.is_empty() {
        None
    } else {
        Some(format!("Takes parameters: {}", params))
    }
}

/// Extract return type from signature as natural language.
///
/// Delegates to the language-specific `extract_return_nl` function pointer
/// stored in each language's `LanguageDef`.
fn extract_return_nl(signature: &str, lang: Language) -> Option<String> {
    (lang.def().extract_return_nl)(signature)
}

/// Extract module context from a file path, including filename stem (SQ-5).
///
/// Strips common prefixes (src/, lib/) and file extension, tokenizes all
/// remaining path components. Generic stems (mod, index, lib, utils, helpers)
/// are filtered. E.g., `src/store/calls.rs` -> `"store calls"`.
fn extract_file_context(path: &std::path::Path) -> String {
    // PB-27: Use Path::components() for cross-platform path splitting
    use std::path::Component;
    let skip = [
        "src",
        "lib",
        ".",
        "test",
        "tests",
        "spec",
        "specs",
        "fixtures",
        "fixture",
        "testdata",
        "internal",
        "pkg",
        "cmd",
        "app",
        "eval",
        "bench",
        "benches",
        "examples",
        "example",
        "vendor",
        "third_party",
    ];
    let components: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .filter(|c| !c.is_empty() && !skip.contains(c))
        .collect();
    // Include filename stem for module-level discrimination (SQ-5).
    // Strip file extension from last component. Skip generic stems that add
    // noise rather than signal.
    let generic_stems = [
        "mod",
        "index",
        "lib",
        "main",
        "utils",
        "helpers",
        "common",
        "types",
        "config",
        "constants",
        "init",
    ];
    if components.is_empty() {
        return String::new();
    }
    let mut result: Vec<String> = Vec::new();
    for (i, c) in components.iter().enumerate() {
        let c = if i == components.len() - 1 {
            // Last component: strip extension, skip generic stems
            let stem = c.rsplit_once('.').map_or(*c, |(s, _)| s);
            if generic_stems.contains(&stem) {
                continue;
            }
            stem
        } else {
            c
        };
        result.extend(tokenize_identifier(c));
    }
    if result.is_empty() {
        return String::new();
    }
    result.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_extract_params_nl() {
        // Note: colons are preserved as they're not word separators in tokenize_identifier
        assert_eq!(
            extract_params_nl("fn foo(x: i32, y: String)"),
            Some("Takes parameters: x: i32, y: string".to_string())
        );
        assert_eq!(
            extract_params_nl("fn bar()"),
            Some("Takes no parameters".to_string())
        );
        // &self is tokenized as one word and filtered out because it contains "self"
        // but in practice the & prefix means it won't match - this is a known limitation
        assert_eq!(
            extract_params_nl("fn baz(self, x: i32)"),
            Some("Takes parameters: x: i32".to_string())
        );
    }

    #[test]
    fn test_extract_return_nl() {
        assert_eq!(
            extract_return_nl("fn foo() -> String", Language::Rust),
            Some("Returns string".to_string())
        );
        assert_eq!(
            extract_return_nl("function foo(): string", Language::TypeScript),
            Some("Returns string".to_string())
        );
        assert_eq!(
            extract_return_nl("def foo() -> str:", Language::Python),
            Some("Returns str".to_string())
        );
        assert_eq!(
            extract_return_nl("function foo()", Language::JavaScript),
            None
        );
    }

    #[test]
    fn test_extract_return_nl_go() {
        // Go: return type between ) and {
        assert_eq!(
            extract_return_nl("func foo() string {", Language::Go),
            Some("Returns string".to_string())
        );
        // Multiple return values
        assert_eq!(
            extract_return_nl("func foo() (string, error) {", Language::Go),
            Some("Returns (string, error)".to_string())
        );
        // No return type
        assert_eq!(extract_return_nl("func foo() {", Language::Go), None);
        // Method with receiver
        assert_eq!(
            extract_return_nl("func (s *Server) Start() error {", Language::Go),
            Some("Returns error".to_string())
        );
    }

    #[test]
    fn test_generate_nl_description() {
        let chunk = Chunk {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "parseConfig".to_string(),
            signature: "fn parseConfig(path: &str) -> Config".to_string(),
            content: "{}".to_string(),
            line_start: 1,
            line_end: 1,
            doc: Some("/// Load config from path".to_string()),
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };

        let nl = generate_nl_description(&chunk);
        assert!(nl.contains("Load config from path"));
        assert!(nl.contains("parse config"));
        assert!(nl.contains("Takes parameters:"));
        assert!(nl.contains("Returns config"));
    }

    #[test]
    fn test_generate_nl_with_jsdoc() {
        // JavaScript function with JSDoc - params from signature, return from JSDoc
        let chunk = Chunk {
            id: "test.js:1:abcd1234".to_string(),
            file: PathBuf::from("test.js"),
            language: Language::JavaScript,
            chunk_type: ChunkType::Function,
            name: "validateEmail".to_string(),
            signature: "function validateEmail(email)".to_string(),
            content: "{}".to_string(),
            line_start: 1,
            line_end: 1,
            doc: Some(
                r#"/**
                 * Validates an email address
                 * @param {string} email - The email to check
                 * @returns {boolean} True if valid
                 */"#
                .to_string(),
            ),
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };

        let nl = generate_nl_description(&chunk);
        assert!(nl.contains("Validates an email"));
        assert!(nl.contains("validate email"));
        // Params come from signature (no types in JS), return type from JSDoc
        assert!(
            nl.contains("Takes parameters: email"),
            "Should have param from signature: {}",
            nl
        );
        assert!(
            nl.contains("Returns boolean"),
            "Should have JSDoc return: {}",
            nl
        );
    }

    // ===== Markdown NL tests =====

    /// Creates a test Chunk representing a markdown section with the specified content, signature, and name.
    fn make_section_chunk(content: &str, signature: &str, name: &str) -> Chunk {
        Chunk {
            id: "test.md:1:abcd1234".to_string(),
            file: PathBuf::from("test.md"),
            language: Language::Markdown,
            chunk_type: ChunkType::Section,
            name: name.to_string(),
            signature: signature.to_string(),
            content: content.to_string(),
            line_start: 1,
            line_end: 10,
            doc: None,
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    #[test]
    fn test_markdown_nl_uses_full_content() {
        // 3000 chars of content — should use 1800 char preview
        let content = "a".repeat(3000);
        let chunk = make_section_chunk(&content, "Title > Section", "Section");
        let nl = generate_nl_description(&chunk);
        // Breadcrumb + name + 1800 chars of content
        assert!(nl.contains("Title > Section"));
        assert!(nl.contains("Section"));
        // Should be much longer than the old 200 char limit
        assert!(nl.len() > 500, "NL should be >500 chars, got {}", nl.len());
        // But not include all 3000 chars
        assert!(
            nl.len() < 2500,
            "NL should be <2500 chars, got {}",
            nl.len()
        );
    }

    #[test]
    fn test_markdown_nl_short_content() {
        let chunk = make_section_chunk("Short section content here.", "Guide > Intro", "Intro");
        let nl = generate_nl_description(&chunk);
        assert!(nl.contains("Guide > Intro"));
        assert!(nl.contains("Intro"));
        assert!(nl.contains("Short section content here."));
    }

    // ===== Parent type context tests =====

    #[test]
    fn test_method_nl_includes_parent_type() {
        let chunk = Chunk {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Method,
            name: "should_allow".to_string(),
            signature: "fn should_allow(&self) -> bool".to_string(),
            content: "{}".to_string(),
            line_start: 1,
            line_end: 1,
            doc: Some("/// Check if calls should be allowed".to_string()),
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: Some("CircuitBreaker".to_string()),
        };
        let nl = generate_nl_description(&chunk);
        assert!(
            nl.contains("circuit breaker method"),
            "NL should contain tokenized parent type: {}",
            nl
        );
        assert!(nl.contains("Check if calls should be allowed"));
    }

    #[test]
    fn test_method_nl_without_parent_type() {
        let chunk = Chunk {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Method,
            name: "process".to_string(),
            signature: "fn process(&self)".to_string(),
            content: "{}".to_string(),
            line_start: 1,
            line_end: 1,
            doc: None,
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };
        let nl = generate_nl_description(&chunk);
        // Compact: no "A method named" prefix, just tokenized name
        assert!(nl.contains("process"));
        // Without parent_type_name, should not have any "X method" prefix
        assert!(
            !nl.starts_with("method"),
            "Should not start with orphan 'method' prefix: {}",
            nl
        );
    }

    #[test]
    fn test_function_ignores_parent_type() {
        let chunk = Chunk {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: "standalone".to_string(),
            signature: "fn standalone()".to_string(),
            content: "{}".to_string(),
            line_start: 1,
            line_end: 1,
            doc: None,
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        };
        let nl = generate_nl_description(&chunk);
        assert!(nl.contains("standalone"));
    }

    #[test]
    fn test_docfirst_template_skips_parent_type() {
        let chunk = Chunk {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Method,
            name: "should_allow".to_string(),
            signature: "fn should_allow(&self) -> bool".to_string(),
            content: "{}".to_string(),
            line_start: 1,
            line_end: 1,
            doc: Some("/// Check if allowed".to_string()),
            content_hash: "abcd1234".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: Some("CircuitBreaker".to_string()),
        };
        let nl = generate_nl_with_template(&chunk, NlTemplate::DocFirst);
        // DocFirst returns early: doc + name only, no parent type context
        assert!(
            !nl.contains("circuit breaker"),
            "DocFirst should skip parent type: {}",
            nl
        );
        assert!(nl.contains("Check if allowed"));
    }

    // ===== Fuzz tests =====

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Fuzz: extract_params_nl should never panic
            #[test]
            fn fuzz_extract_params_no_panic(sig in "\\PC{0,200}") {
                let _ = extract_params_nl(&sig);
            }

            /// Fuzz: extract_return_nl should never panic for all languages
            #[test]
            fn fuzz_extract_return_no_panic(sig in "\\PC{0,200}") {
                // Exercise all language variants via all_variants() — automatically
                // covers new languages when added to define_languages!
                for lang in Language::all_variants() {
                    let _ = extract_return_nl(&sig, *lang);
                }
            }
        }
    }

    fn test_chunk(name: &str) -> Chunk {
        Chunk {
            id: name.to_string(),
            file: PathBuf::from("src/test.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: String::new(),
            doc: None,
            line_start: 1,
            line_end: 10,
            content_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    #[test]
    fn test_call_context_callers_only() {
        let chunk = test_chunk("handle_request");
        let ctx = CallContext {
            callers: vec!["main".to_string(), "serve".to_string()],
            callees: vec![],
        };
        let freq = std::collections::HashMap::new();
        let nl = generate_nl_with_call_context(&chunk, &ctx, &freq, 5, 5);
        assert!(nl.contains("Called by: main, serve"), "got: {}", nl);
        assert!(!nl.contains("Calls:"), "got: {}", nl);
    }

    #[test]
    fn test_call_context_callees_with_idf_filter() {
        let chunk = test_chunk("process");
        let ctx = CallContext {
            callers: vec![],
            callees: vec![
                "validate".to_string(),
                "log".to_string(),
                "save".to_string(),
            ],
        };
        let mut freq = std::collections::HashMap::new();
        freq.insert("log".to_string(), 0.15_f32); // above 10% threshold — filtered
        freq.insert("validate".to_string(), 0.05_f32); // below — kept
        freq.insert("save".to_string(), 0.02_f32); // below — kept
        let nl = generate_nl_with_call_context(&chunk, &ctx, &freq, 5, 5);
        assert!(nl.contains("Calls: validate, save"), "got: {}", nl);
        assert!(!nl.contains("log"), "log should be filtered, got: {}", nl);
    }

    #[test]
    fn test_call_context_max_callers_truncation() {
        let chunk = test_chunk("f");
        let ctx = CallContext {
            callers: vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
            callees: vec![],
        };
        let freq = std::collections::HashMap::new();
        let nl = generate_nl_with_call_context(&chunk, &ctx, &freq, 2, 5);
        assert!(nl.contains("Called by: a, b"), "got: {}", nl);
        assert!(!nl.contains(", c"), "c should be truncated, got: {}", nl);
    }

    #[test]
    fn test_call_context_empty_returns_base() {
        let chunk = test_chunk("lonely");
        let ctx = CallContext::default();
        let freq = std::collections::HashMap::new();
        let base = generate_nl_description(&chunk);
        let enriched = generate_nl_with_call_context(&chunk, &ctx, &freq, 5, 5);
        assert_eq!(base, enriched);
    }

    // TC-26: generate_nl_with_call_context_and_summary
    #[test]
    fn test_call_context_and_summary_prepends_summary_appends_hyde() {
        let chunk = test_chunk("process_data");
        let ctx = CallContext {
            callers: vec!["main".to_string()],
            callees: vec!["validate".to_string()],
        };
        let freq = std::collections::HashMap::new();
        let summary = "Processes raw data into structured output";
        let hyde = "how to process data\ntransform raw input";

        let nl = generate_nl_with_call_context_and_summary(
            &chunk,
            &ctx,
            &freq,
            5,
            5,
            Some(summary),
            Some(hyde),
        );

        // Summary should be prepended (appears before the base NL)
        assert!(
            nl.starts_with(summary),
            "Summary should be prepended, got: {}",
            nl
        );
        // HyDE queries should be appended
        assert!(
            nl.contains("Queries: how to process data, transform raw input"),
            "HyDE queries should be appended, got: {}",
            nl
        );
        // Callers and callees still present
        assert!(nl.contains("Called by: main"), "got: {}", nl);
        assert!(nl.contains("Calls: validate"), "got: {}", nl);
    }

    // TC-30: IDF callee filtering threshold
    #[test]
    fn test_callee_idf_filtering_above_threshold() {
        let chunk = test_chunk("my_func");
        let ctx = CallContext {
            callers: vec![],
            callees: vec!["log".to_string(), "rare_fn".to_string()],
        };
        // "log" appears in 15% of chunks (above 10% threshold), "rare_fn" in 2%
        let mut freq = std::collections::HashMap::new();
        freq.insert("log".to_string(), 0.15);
        freq.insert("rare_fn".to_string(), 0.02);

        let nl = generate_nl_with_call_context(&chunk, &ctx, &freq, 5, 5);

        // "log" should be filtered out (>= 0.10 threshold)
        assert!(
            !nl.contains("Calls: log"),
            "High-frequency callee 'log' should be filtered, got: {}",
            nl
        );
        // "rare_fn" should be present
        assert!(
            nl.contains("rare fn"),
            "Low-frequency callee 'rare_fn' should be kept, got: {}",
            nl
        );
    }

    // ===== enrichment NL output with call context (#665) =====

    #[test]
    fn enrichment_nl_includes_callers_and_callees() {
        let chunk = test_chunk("process_data");
        let ctx = CallContext {
            callers: vec!["handle_request".to_string(), "run_pipeline".to_string()],
            callees: vec!["validate_input".to_string(), "transform_record".to_string()],
        };
        let freq = std::collections::HashMap::new();

        let nl = generate_nl_with_call_context_and_summary(&chunk, &ctx, &freq, 5, 5, None, None);

        // Callers appear (tokenized: snake_case split into words)
        assert!(
            nl.contains("Called by:"),
            "NL must contain 'Called by:' section, got: {nl}"
        );
        assert!(
            nl.contains("handle request"),
            "Caller 'handle_request' should appear tokenized, got: {nl}"
        );
        assert!(
            nl.contains("run pipeline"),
            "Caller 'run_pipeline' should appear tokenized, got: {nl}"
        );

        // Callees appear
        assert!(
            nl.contains("Calls:"),
            "NL must contain 'Calls:' section, got: {nl}"
        );
        assert!(
            nl.contains("validate input"),
            "Callee 'validate_input' should appear tokenized, got: {nl}"
        );
        assert!(
            nl.contains("transform record"),
            "Callee 'transform_record' should appear tokenized, got: {nl}"
        );
    }

    #[test]
    fn enrichment_nl_filters_high_freq_callees() {
        let chunk = test_chunk("my_func");
        let ctx = CallContext {
            callers: vec!["caller_a".to_string()],
            callees: vec![
                "log".to_string(),
                "rare_fn".to_string(),
                "unwrap".to_string(),
            ],
        };
        // "log" at 15%, "unwrap" at 12% — both above 10% threshold
        let mut freq = std::collections::HashMap::new();
        freq.insert("log".to_string(), 0.15);
        freq.insert("unwrap".to_string(), 0.12);
        freq.insert("rare_fn".to_string(), 0.02);

        let nl = generate_nl_with_call_context_and_summary(&chunk, &ctx, &freq, 5, 5, None, None);

        // High-frequency callees should be filtered
        assert!(
            !nl.contains("log"),
            "High-freq callee 'log' (15%) must be filtered, got: {nl}"
        );
        assert!(
            !nl.contains("unwrap"),
            "High-freq callee 'unwrap' (12%) must be filtered, got: {nl}"
        );
        // Low-frequency callee should be kept
        assert!(
            nl.contains("rare fn"),
            "Low-freq callee 'rare_fn' (2%) should be kept, got: {nl}"
        );
    }

    #[test]
    fn enrichment_nl_with_summary_and_call_context() {
        // Verifies the full enrichment pipeline: summary + callers + callees + hyde
        let chunk = test_chunk("search_index");
        let ctx = CallContext {
            callers: vec!["query_handler".to_string()],
            callees: vec!["embed_text".to_string()],
        };
        let freq = std::collections::HashMap::new();
        let summary = "Searches the HNSW index for nearest neighbors";
        let hyde = "find similar code\nsemantic search";

        let nl = generate_nl_with_call_context_and_summary(
            &chunk,
            &ctx,
            &freq,
            5,
            5,
            Some(summary),
            Some(hyde),
        );

        // Summary prepended
        assert!(
            nl.starts_with(summary),
            "Summary must be prepended, got: {nl}"
        );
        // Call context present
        assert!(
            nl.contains("Called by: query handler"),
            "Caller must appear, got: {nl}"
        );
        assert!(
            nl.contains("Calls: embed text"),
            "Callee must appear, got: {nl}"
        );
        // HyDE appended
        assert!(
            nl.contains("Queries: find similar code, semantic search"),
            "HyDE queries must be appended, got: {nl}"
        );
    }
}
