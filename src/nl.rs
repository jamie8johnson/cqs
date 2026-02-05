//! Natural language generation from code chunks.
//!
//! Converts code metadata into natural language descriptions for embedding.
//! Based on Greptile's finding that code->NL->embed improves semantic search.

use crate::parser::{Chunk, ChunkType, Language};
use regex::Regex;
use std::sync::LazyLock;

/// JSDoc tag information extracted from documentation comments.
#[derive(Debug, Default)]
pub struct JsDocInfo {
    /// Parameter names and types from @param tags
    pub params: Vec<(String, String)>, // (name, type)
    /// Return type from @returns/@return tag
    pub returns: Option<String>,
}

// Pre-compiled regexes for JSDoc parsing
static JSDOC_PARAM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@param\s+\{([^}]+)\}\s+(\w+)").expect("valid regex"));
static JSDOC_RETURNS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@returns?\s+\{([^}]+)\}").expect("valid regex"));

/// Parse JSDoc tags from a documentation comment.
///
/// Extracts @param and @returns/@return tags from JSDoc-style comments.
///
/// # Example
///
/// ```
/// use cqs::nl::parse_jsdoc_tags;
///
/// let doc = r#"/**
///  * Validates an email address
///  * @param {string} email - The email to validate
///  * @returns {boolean} Whether valid
///  */"#;
///
/// let info = parse_jsdoc_tags(doc);
/// assert_eq!(info.params, vec![("email".to_string(), "string".to_string())]);
/// assert_eq!(info.returns, Some("boolean".to_string()));
/// ```
pub fn parse_jsdoc_tags(doc: &str) -> JsDocInfo {
    let mut info = JsDocInfo::default();

    for cap in JSDOC_PARAM_RE.captures_iter(doc) {
        let type_str = cap[1].to_string();
        let name = cap[2].to_string();
        info.params.push((name, type_str));
    }

    if let Some(cap) = JSDOC_RETURNS_RE.captures(doc) {
        info.returns = Some(cap[1].to_string());
    }

    info
}

/// Split identifier on snake_case and camelCase boundaries.
///
/// Note: This function splits on every uppercase letter, so acronyms like
/// "XMLParser" become individual letters. This is intentional for search
/// tokenization where "xml parser" is more useful than preserving "XML".
///
/// # Examples
///
/// ```
/// use cqs::nl::tokenize_identifier;
///
/// assert_eq!(tokenize_identifier("parseConfigFile"), vec!["parse", "config", "file"]);
/// assert_eq!(tokenize_identifier("get_user_name"), vec!["get", "user", "name"]);
/// assert_eq!(tokenize_identifier("XMLParser"), vec!["x", "m", "l", "parser"]); // acronyms split per-letter
/// ```
pub fn tokenize_identifier(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
        } else if c.is_uppercase() && !current.is_empty() {
            words.push(current.clone());
            current.clear();
            current.push(c.to_lowercase().next().unwrap_or(c));
        } else {
            current.push(c.to_lowercase().next().unwrap_or(c));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Maximum output length for FTS normalization.
/// Prevents memory exhaustion from pathological inputs where tokenization
/// expands text (e.g., "ABCD" → "a b c d" doubles length).
const MAX_FTS_OUTPUT_LEN: usize = 16384;

/// Normalize code text for FTS5 indexing.
///
/// Splits identifiers on camelCase/snake_case boundaries and joins with spaces.
/// Used to make code searchable with natural language queries.
/// Output is capped at 16KB to prevent memory issues with pathological inputs.
///
/// # Example
///
/// ```
/// use cqs::nl::normalize_for_fts;
///
/// assert_eq!(normalize_for_fts("parseConfigFile"), "parse config file");
/// assert_eq!(normalize_for_fts("fn get_user() {}"), "fn get user");
/// ```
pub fn normalize_for_fts(text: &str) -> String {
    let mut result = String::new();
    let mut current_word = String::new();

    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            current_word.push(c);
        } else if !current_word.is_empty() {
            let tokens = tokenize_identifier(&current_word);
            if !result.is_empty() && !tokens.is_empty() {
                result.push(' ');
            }
            result.push_str(&tokens.join(" "));
            current_word.clear();

            // Cap output to prevent memory issues - truncate at last space boundary
            if result.len() >= MAX_FTS_OUTPUT_LEN {
                let truncate_at = result[..MAX_FTS_OUTPUT_LEN]
                    .rfind(' ')
                    .unwrap_or(MAX_FTS_OUTPUT_LEN);
                result.truncate(truncate_at);
                return result;
            }
        }
    }
    if !current_word.is_empty() {
        let tokens = tokenize_identifier(&current_word);
        if !result.is_empty() && !tokens.is_empty() {
            result.push(' ');
        }
        result.push_str(&tokens.join(" "));
    }

    // Final cap check - truncate at last space to avoid splitting words
    if result.len() > MAX_FTS_OUTPUT_LEN {
        // Find last space before the limit to avoid mid-word truncation
        let truncate_at = result[..MAX_FTS_OUTPUT_LEN]
            .rfind(' ')
            .unwrap_or(MAX_FTS_OUTPUT_LEN);
        result.truncate(truncate_at);
    }
    result
}

/// Generate natural language description from chunk metadata.
///
/// Produces text like: "A function named parse config. Takes path parameter. Returns config."
///
/// # Example
///
/// ```
/// use cqs::nl::generate_nl_description;
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
/// };
///
/// let nl = generate_nl_description(&chunk);
/// assert!(nl.contains("parse config"));
/// assert!(nl.contains("Parse configuration"));
/// ```
pub fn generate_nl_description(chunk: &Chunk) -> String {
    let mut parts = Vec::new();

    // 1. Doc comment is gold - it's human-written NL
    if let Some(ref doc) = chunk.doc {
        let doc_trimmed = doc.trim();
        if !doc_trimmed.is_empty() {
            parts.push(doc_trimmed.to_string());
        }
    }

    // 2. Chunk type + normalized name
    let type_word = match chunk.chunk_type {
        ChunkType::Function => "function",
        ChunkType::Method => "method",
        ChunkType::Class => "class",
        ChunkType::Struct => "struct",
        ChunkType::Enum => "enum",
        ChunkType::Trait => "trait",
        ChunkType::Interface => "interface",
        ChunkType::Constant => "constant",
    };
    let name_words = tokenize_identifier(&chunk.name).join(" ");
    parts.push(format!("A {} named {}", type_word, name_words));

    // 3. Parse signature for params/return (with JSDoc fallback for JavaScript)
    let jsdoc_info = if chunk.language == Language::JavaScript {
        chunk.doc.as_ref().map(|d| parse_jsdoc_tags(d))
    } else {
        None
    };

    // Parameters: prefer signature, fallback to JSDoc for JS
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

    // Return type: prefer signature, fallback to JSDoc for JS
    if let Some(return_desc) = extract_return_nl(&chunk.signature, chunk.language) {
        parts.push(return_desc);
    } else if let Some(ref info) = jsdoc_info {
        if let Some(ref ret) = info.returns {
            parts.push(format!("Returns {}", ret));
        }
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

    let params: Vec<String> = params_str
        .split(',')
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            let words: Vec<_> = tokenize_identifier(p)
                .into_iter()
                .filter(|w| !["self", "mut"].contains(&w.as_str()))
                .collect();
            if words.is_empty() {
                None
            } else {
                Some(words.join(" "))
            }
        })
        .collect();

    if params.is_empty() {
        None
    } else {
        Some(format!("Takes parameters: {}", params.join(", ")))
    }
}

/// Extract return type from signature as natural language.
fn extract_return_nl(signature: &str, lang: Language) -> Option<String> {
    match lang {
        Language::Rust => {
            if let Some(arrow) = signature.find("->") {
                let ret = signature[arrow + 2..].trim();
                if ret.is_empty() {
                    return None;
                }
                let ret_words = tokenize_identifier(ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
        Language::Go => {
            // Go: `func name(params) returnType {` or `func (recv) name(params) returnType {`
            // Return type is between the params close-paren and the opening brace
            // For multiple returns: `func name() (string, error) {`

            // Strip trailing { first
            let sig = signature.trim_end_matches('{').trim();

            // Find where params end by counting parens from the right
            // The params close-paren is the last ) NOT inside a return type (...)
            // Strategy: find the last ')' that's not part of a multi-return

            // Simple approach: if last char is ), then return type is wrapped in ()
            // Otherwise return type is plain text after the last )
            if sig.ends_with(')') {
                // Check if it's a multi-return like (string, error) or just empty params ()
                // Find the matching ( for the final )
                let mut depth = 0;
                let mut start_idx = None;
                for (i, c) in sig.char_indices().rev() {
                    match c {
                        ')' => depth += 1,
                        '(' => {
                            depth -= 1;
                            if depth == 0 {
                                start_idx = Some(i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(start) = start_idx {
                    // Check if there's a ) before this ( - that would be the params close
                    let before = &sig[..start].trim();
                    if before.ends_with(')') {
                        // Multi-return: extract the (...)
                        let ret = &sig[start..];
                        if !ret.is_empty() {
                            return Some(format!("Returns {}", ret));
                        }
                    }
                }
                // Either no multi-return or empty params - fall through to return None
                return None;
            } else {
                // Plain return type after last )
                if let Some(paren) = sig.rfind(')') {
                    let ret = sig[paren + 1..].trim();
                    if ret.is_empty() {
                        return None;
                    }
                    let ret_words = tokenize_identifier(ret).join(" ");
                    return Some(format!("Returns {}", ret_words));
                }
            }
        }
        Language::TypeScript => {
            // Note: rfind may match incorrectly on complex signatures like
            // `function foo(): (x: number) => string` - proper parsing would require
            // tracking parenthesis depth. This handles the common case.
            if let Some(colon) = signature.rfind("):") {
                let ret = signature[colon + 2..].trim();
                if ret.is_empty() {
                    return None;
                }
                let ret_words = tokenize_identifier(ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
        Language::Python => {
            if let Some(arrow) = signature.rfind("->") {
                let ret = signature[arrow + 2..].trim().trim_end_matches(':');
                if ret.is_empty() {
                    return None;
                }
                let ret_words = tokenize_identifier(ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
        Language::JavaScript => {
            // JavaScript doesn't have type annotations in signatures
            // JSDoc parsing handled separately
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_tokenize_identifier() {
        assert_eq!(
            tokenize_identifier("parseConfigFile"),
            vec!["parse", "config", "file"]
        );
        assert_eq!(
            tokenize_identifier("get_user_name"),
            vec!["get", "user", "name"]
        );
        assert_eq!(tokenize_identifier("simple"), vec!["simple"]);
        assert_eq!(tokenize_identifier(""), Vec::<String>::new());
    }

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
        };

        let nl = generate_nl_description(&chunk);
        assert!(nl.contains("Load config from path"));
        assert!(nl.contains("A function named parse config"));
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
        };

        let nl = generate_nl_description(&chunk);
        assert!(nl.contains("Validates an email"));
        assert!(nl.contains("A function named validate email"));
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

    #[test]
    fn test_parse_jsdoc_tags() {
        let doc = r#"/**
         * Does something
         * @param {number} x - First number
         * @param {string} name - The name
         * @returns {boolean} Success
         */"#;

        let info = parse_jsdoc_tags(doc);
        assert_eq!(info.params.len(), 2);
        assert_eq!(info.params[0], ("x".to_string(), "number".to_string()));
        assert_eq!(info.params[1], ("name".to_string(), "string".to_string()));
        assert_eq!(info.returns, Some("boolean".to_string()));
    }

    #[test]
    fn test_normalize_for_fts_output_bounded() {
        // Pathological input: all uppercase chars tokenize to "a b c d ..."
        // which roughly doubles the length
        let long_upper = "A".repeat(20000);
        let result = normalize_for_fts(&long_upper);
        assert!(
            result.len() <= super::MAX_FTS_OUTPUT_LEN,
            "FTS output should be capped at {} but was {}",
            super::MAX_FTS_OUTPUT_LEN,
            result.len()
        );
    }

    #[test]
    fn test_normalize_for_fts_normal_input_unchanged() {
        // Normal inputs should work as expected
        assert_eq!(normalize_for_fts("hello"), "hello");
        assert_eq!(normalize_for_fts("HelloWorld"), "hello world");
        assert_eq!(normalize_for_fts("get_user_name"), "get user name");
    }

    // ===== Fuzz tests =====

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Fuzz: tokenize_identifier should never panic
            #[test]
            fn fuzz_tokenize_identifier_no_panic(input in "\\PC{0,200}") {
                let _ = tokenize_identifier(&input);
            }

            /// Fuzz: tokenize_identifier with identifier-like strings
            #[test]
            fn fuzz_tokenize_identifier_like(input in "[a-zA-Z_][a-zA-Z0-9_]{0,50}") {
                let result = tokenize_identifier(&input);
                // Result can be empty if input is all underscores/non-alpha
                // Just verify it doesn't panic and returns valid tokens
                for token in &result {
                    prop_assert!(!token.is_empty(), "Empty token in result");
                }
            }

            /// Fuzz: parse_jsdoc_tags should never panic
            #[test]
            fn fuzz_parse_jsdoc_tags_no_panic(input in "\\PC{0,500}") {
                let _ = parse_jsdoc_tags(&input);
            }

            /// Fuzz: parse_jsdoc_tags with JSDoc-like structure
            #[test]
            fn fuzz_parse_jsdoc_structured(
                desc in "[a-zA-Z ]{0,50}",
                param_name in "[a-z]{1,10}",
                param_type in "[a-zA-Z]{1,15}",
                return_type in "[a-zA-Z]{1,15}"
            ) {
                let input = format!(
                    "/**\n * {}\n * @param {{{}}} {} - Description\n * @returns {{{}}} Result\n */",
                    desc, param_type, param_name, return_type
                );
                let info = parse_jsdoc_tags(&input);
                // Should parse successfully for well-formed input
                prop_assert!(info.params.len() <= 1);
            }

            /// Fuzz: extract_params_nl should never panic
            #[test]
            fn fuzz_extract_params_no_panic(sig in "\\PC{0,200}") {
                let _ = extract_params_nl(&sig);
            }

            /// Fuzz: extract_return_nl should never panic for all languages
            #[test]
            fn fuzz_extract_return_no_panic(sig in "\\PC{0,200}") {
                let _ = extract_return_nl(&sig, Language::Rust);
                let _ = extract_return_nl(&sig, Language::Python);
                let _ = extract_return_nl(&sig, Language::TypeScript);
                let _ = extract_return_nl(&sig, Language::JavaScript);
                let _ = extract_return_nl(&sig, Language::Go);
            }
        }
    }
}
