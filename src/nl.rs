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
/// assert_eq!(tokenize_identifier("获取用户"), vec!["获", "取", "用", "户"]); // CJK: one token per character
/// ```
pub fn tokenize_identifier(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    for c in s.chars() {
        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                // Use std::mem::take to avoid clone - moves String out and leaves empty String
                words.push(std::mem::take(&mut current));
            }
        } else if is_cjk(c) {
            // CJK characters become individual tokens (no word boundaries in CJK)
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            words.push(c.to_string());
        } else if c.is_uppercase() && !current.is_empty() {
            words.push(std::mem::take(&mut current));
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

/// Returns true for CJK Unified Ideographs and common CJK ranges.
/// Covers Chinese, Japanese kanji, Korean hanja, and extensions.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{3000}'..='\u{303F}' // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
    )
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
/// # Security: FTS5 Injection Protection
///
/// This function provides implicit protection against FTS5 injection attacks.
/// By only emitting alphanumeric tokens joined by spaces, special FTS5 operators
/// like `OR`, `AND`, `NOT`, `NEAR`, `*`, `"`, `(`, `)` are neutralized:
/// - Operators in the input become separate tokens (e.g., "foo OR bar" -> "foo or bar")
/// - Quotes and parentheses are stripped entirely (only alphanumeric + underscore pass)
/// - The resulting output is safe for direct use in FTS5 MATCH queries
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
            // Stream tokens directly to result instead of creating intermediate Vec<String>
            let mut first_token = true;
            for token in tokenize_identifier_iter(&current_word) {
                if !result.is_empty() || !first_token {
                    result.push(' ');
                }
                result.push_str(&token);
                first_token = false;
            }
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
        // Stream final word's tokens
        let mut first_token = true;
        for token in tokenize_identifier_iter(&current_word) {
            if !result.is_empty() || !first_token {
                result.push(' ');
            }
            result.push_str(&token);
            first_token = false;
        }
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

/// Iterator-based tokenize_identifier for streaming - avoids intermediate Vec allocation
fn tokenize_identifier_iter(s: &str) -> impl Iterator<Item = String> + '_ {
    TokenizeIdentifierIter {
        chars: s.chars().peekable(),
        current: String::new(),
        done: false,
    }
}

struct TokenizeIdentifierIter<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    current: String,
    done: bool,
}

impl<'a> Iterator for TokenizeIdentifierIter<'a> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        loop {
            match self.chars.next() {
                Some(c) if c == '_' || c == '-' || c == ' ' => {
                    if !self.current.is_empty() {
                        return Some(std::mem::take(&mut self.current));
                    }
                }
                Some(c) if is_cjk(c) => {
                    // CJK characters become individual tokens
                    if !self.current.is_empty() {
                        // Stash the CJK char for next iteration by pushing to current
                        // after yielding — but simpler to just yield current first,
                        // then handle CJK on next call. Use peekable workaround:
                        // Actually, we already consumed c. Flush current, return it,
                        // but we need to also emit c. Push c to current so it's yielded next.
                        let result = std::mem::take(&mut self.current);
                        self.current.push(c);
                        return Some(result);
                    }
                    return Some(c.to_string());
                }
                Some(c) if c.is_uppercase() && !self.current.is_empty() => {
                    let result = std::mem::take(&mut self.current);
                    self.current.push(c.to_lowercase().next().unwrap_or(c));
                    return Some(result);
                }
                Some(c) => {
                    self.current.push(c.to_lowercase().next().unwrap_or(c));
                }
                None => {
                    self.done = true;
                    if !self.current.is_empty() {
                        return Some(std::mem::take(&mut self.current));
                    }
                    return None;
                }
            }
        }
    }
}

/// Template variants for NL description generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NlTemplate {
    /// Current production template: doc + "A {type} named {name}" + params + returns
    Standard,
    /// No structural prefix: doc + name + params + returns
    NoPrefix,
    /// Standard + body keywords extracted from function content
    BodyKeywords,
    /// No prefix + body keywords
    Compact,
    /// Doc-first: minimal metadata when doc exists, full template when missing
    DocFirst,
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
    generate_nl_with_template(chunk, NlTemplate::Standard)
}

/// Generate NL description using a specific template variant.
pub fn generate_nl_with_template(chunk: &Chunk, template: NlTemplate) -> String {
    let mut parts = Vec::new();

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

    // Shared: type word
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

    // DocFirst: minimal metadata when doc exists
    if template == NlTemplate::DocFirst && has_doc {
        parts.push(name_words);
        return parts.join(". ");
    }

    // Name line: with or without "A {type} named" prefix
    match template {
        NlTemplate::NoPrefix | NlTemplate::Compact => {
            parts.push(name_words);
        }
        _ => {
            parts.push(format!("A {} named {}", type_word, name_words));
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

    // Body keywords for variants that use them
    if matches!(template, NlTemplate::BodyKeywords | NlTemplate::Compact) {
        let keywords = extract_body_keywords(&chunk.content, chunk.language);
        if !keywords.is_empty() {
            parts.push(format!("Uses: {}", keywords.join(", ")));
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

/// Extract meaningful keywords from function body, filtering language noise.
///
/// Returns up to 10 unique keywords sorted by frequency (descending).
pub fn extract_body_keywords(content: &str, language: Language) -> Vec<String> {
    use std::collections::HashMap;

    let stopwords: &[&str] = language.def().stopwords;

    // Count word frequencies
    let mut freq: HashMap<String, usize> = HashMap::new();
    for token in tokenize_identifier(content) {
        if token.len() >= 3 && !stopwords.contains(&token.as_str()) {
            *freq.entry(token).or_insert(0) += 1;
        }
    }

    // Sort by frequency descending, take top 10
    let mut keywords: Vec<(String, usize)> = freq.into_iter().collect();
    keywords.sort_by(|a, b| b.1.cmp(&a.1));
    keywords.into_iter().take(10).map(|(w, _)| w).collect()
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
    fn test_tokenize_identifier_cjk() {
        // Pure CJK: each character becomes its own token
        assert_eq!(
            tokenize_identifier("获取用户名"),
            vec!["获", "取", "用", "户", "名"]
        );
        // Mixed Latin + CJK
        assert_eq!(
            tokenize_identifier("get用户Name"),
            vec!["get", "用", "户", "name"]
        );
        // Japanese hiragana
        assert_eq!(
            tokenize_identifier("こんにちは"),
            vec!["こ", "ん", "に", "ち", "は"]
        );
        // Korean hangul
        assert_eq!(tokenize_identifier("사용자"), vec!["사", "용", "자"]);
        // CJK with underscores
        assert_eq!(
            tokenize_identifier("get_用户_name"),
            vec!["get", "用", "户", "name"]
        );
    }

    #[test]
    fn test_normalize_for_fts_cjk() {
        // CJK characters split into individual tokens
        assert_eq!(normalize_for_fts("获取用户名"), "获 取 用 户 名");
        // Mixed: CJK in a code context
        assert_eq!(normalize_for_fts("fn get_用户()"), "fn get 用 户");
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
                // Exercise all language variants via all_variants() — automatically
                // covers new languages when added to define_languages!
                for lang in Language::all_variants() {
                    let _ = extract_return_nl(&sig, *lang);
                }
            }
        }
    }
}
