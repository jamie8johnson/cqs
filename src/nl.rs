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
/// ```ignore
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
/// ```ignore
/// use cqs::nl::tokenize_identifier;
///
/// assert_eq!(tokenize_identifier("parseConfigFile"), vec!["parse", "config", "file"]);
/// assert_eq!(tokenize_identifier("get_user_name"), vec!["get", "user", "name"]);
/// assert_eq!(tokenize_identifier("XMLParser"), vec!["x", "m", "l", "parser"]); // acronyms split per-letter
/// assert_eq!(tokenize_identifier("获取用户"), vec!["获", "取", "用", "户"]); // CJK: one token per character
/// ```
pub fn tokenize_identifier(s: &str) -> Vec<String> {
    tokenize_identifier_iter(s).collect()
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
/// use cqs::normalize_for_fts;
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
                let boundary = result.floor_char_boundary(MAX_FTS_OUTPUT_LEN);
                let truncate_at = result[..boundary].rfind(' ').unwrap_or(boundary);
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
        let boundary = result.floor_char_boundary(MAX_FTS_OUTPUT_LEN);
        let truncate_at = result[..boundary].rfind(' ').unwrap_or(boundary);
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
    if !ctx.callees.is_empty() {
        let callee_words: Vec<String> = ctx
            .callees
            .iter()
            .filter(|c| {
                !callee_doc_freq
                    .get(c.as_str())
                    .is_some_and(|&freq| freq >= 0.10)
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

    // Name line (no prefix)
    parts.push(name_words);

    // Struct/enum field names
    if matches!(
        chunk.chunk_type,
        ChunkType::Struct | ChunkType::Enum | ChunkType::Class
    ) {
        let fields = extract_field_names(&chunk.content, chunk.language);
        if !fields.is_empty() {
            parts.push(format!("Fields: {}", fields.join(", ")));
        }
    }

    // Class/struct/interface: extract member method names for richer NL
    if matches!(
        chunk.chunk_type,
        ChunkType::Class | ChunkType::Struct | ChunkType::Interface
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

// Pre-compiled regexes for markdown noise stripping
static MD_HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^#{1,6}\s+").expect("valid regex"));
static MD_IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\([^)]*\)").expect("valid regex"));
static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").expect("valid regex"));
static HTML_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid regex"));
static MULTI_WHITESPACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]{2,}").expect("valid regex"));
static MULTI_NEWLINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("valid regex"));

/// Strip markdown formatting noise for cleaner embedding text.
///
/// Removes heading prefixes, image syntax, simplifies links to just text,
/// strips bold/italic markers, HTML tags, and collapses whitespace.
/// Keeps inline code content (strips backticks but preserves text).
pub fn strip_markdown_noise(content: &str) -> String {
    // Fast path: skip regex work if the input has no markdown characters.
    let has_markdown = content.contains('#')
        || content.contains('[')
        || content.contains('*')
        || content.contains('`')
        || content.contains('<');
    if !has_markdown {
        use std::borrow::Cow;
        let result: Cow<str> = MULTI_WHITESPACE_RE.replace_all(content, " ");
        let result: Cow<str> = MULTI_NEWLINE_RE.replace_all(&result, "\n\n");
        return result.trim().to_string();
    }

    use std::borrow::Cow;
    let result: Cow<str> = MD_HEADING_RE.replace_all(content, "");
    let result: Cow<str> = MD_IMAGE_RE.replace_all(&result, "");
    let result: Cow<str> = MD_LINK_RE.replace_all(&result, "$1");
    let result: Cow<str> = HTML_TAG_RE.replace_all(&result, "");
    let mut result = result.into_owned();
    result = result.replace("***", "");
    result = result.replace("**", "");
    result = result.replace('*', "");
    result = result.replace("```", "");
    result = result.replace('`', "");
    let result: Cow<str> = MULTI_WHITESPACE_RE.replace_all(&result, " ");
    let result: Cow<str> = MULTI_NEWLINE_RE.replace_all(&result, "\n\n");
    result.trim().to_string()
}

/// Extract module context from a file path, including filename stem (SQ-5).
///
/// Strips common prefixes (src/, lib/) and file extension, tokenizes all
/// remaining path components. Generic stems (mod, index, lib, utils, helpers)
/// are filtered. E.g., `src/store/calls.rs` → `"store calls"`.
fn extract_file_context(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    // Normalize separators
    let s = s.replace('\\', "/");
    // Strip leading ./ or common root dirs
    let s = s.strip_prefix("./").unwrap_or(&s);
    // Split into components, skip common non-informative segments
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
    let components: Vec<&str> = s
        .split('/')
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

/// Extract field/variant names from struct, enum, or class content.
///
/// Parses field declarations from the chunk's source code.
/// Returns field names (without types) for embedding.
fn extract_field_names(content: &str, language: Language) -> Vec<String> {
    let mut fields = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Skip empty lines, comments, braces, decorators
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed == "{"
            || trimmed == "}"
            || trimmed.starts_with("pub struct")
            || trimmed.starts_with("struct")
            || trimmed.starts_with("pub enum")
            || trimmed.starts_with("enum")
            || trimmed.starts_with("class")
            || trimmed.starts_with("type ")
            || trimmed.starts_with("export")
        {
            continue;
        }

        // Extract field name based on language
        let field = match language {
            Language::Rust | Language::Go => {
                // `pub name: Type,` or `name Type` (Go)
                trimmed
                    .trim_start_matches("pub ")
                    .trim_start_matches("pub(crate) ")
                    .split([':', ' '])
                    .next()
                    .map(|s| s.trim_end_matches(','))
            }
            Language::Python => {
                // `name: type` or `name = value`
                trimmed.split([':', '=']).next().map(|s| s.trim())
            }
            Language::TypeScript | Language::JavaScript | Language::Java => {
                // `name: type;` or `private name: type;`
                let clean = trimmed
                    .trim_start_matches("public ")
                    .trim_start_matches("private ")
                    .trim_start_matches("protected ")
                    .trim_start_matches("readonly ");
                clean.split([':', '=', ';']).next().map(|s| s.trim())
            }
            _ => None,
        };

        if let Some(name) = field {
            let name = name.trim();
            // Skip if it looks like a variant with data, keyword, or too short
            if !name.is_empty()
                && name.len() > 1
                && !name.contains('(')
                && !name.contains('{')
                && name.starts_with(|c: char| c.is_alphabetic() || c == '_')
            {
                let tokenized = tokenize_identifier(name).join(" ");
                if !tokenized.is_empty() {
                    fields.push(tokenized);
                }
            }
        }

        if fields.len() >= 15 {
            break; // Cap at 15 fields
        }
    }
    fields
}

/// Extract member method/function names from class/struct/interface content.
///
/// Scans lines for common method declaration patterns across languages.
/// Returns raw method names (not tokenized) — caller tokenizes for NL.
fn extract_member_method_names(content: &str, language: Language) -> Vec<String> {
    let mut methods = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(name) = extract_method_name_from_line(trimmed, language) {
            if !name.is_empty() && name.len() > 1 {
                methods.push(name);
            }
            if methods.len() >= 15 {
                break;
            }
        }
    }
    methods
}

/// Try to extract a method name from a single line of code.
fn extract_method_name_from_line(line: &str, language: Language) -> Option<String> {
    // Skip comments, empty, decorators
    if line.is_empty()
        || line.starts_with("//")
        || line.starts_with('#')
        || line.starts_with("/*")
        || line.starts_with('*')
        || line.starts_with('@')
    {
        return None;
    }

    // Rust: fn name(, pub fn name(, pub(crate) fn name(
    // Go: func (r *T) Name(, func Name(
    // Python: def name(
    // JS/TS: methodName(, async methodName(, public methodName(
    // Java/C#/Kotlin: visibility type methodName(
    // Ruby: def name
    let work = line
        .trim_start_matches("pub(crate) ")
        .trim_start_matches("pub(super) ")
        .trim_start_matches("pub ")
        .trim_start_matches("private ")
        .trim_start_matches("protected ")
        .trim_start_matches("public ")
        .trim_start_matches("internal ")
        .trim_start_matches("override ")
        .trim_start_matches("virtual ")
        .trim_start_matches("abstract ")
        .trim_start_matches("static ")
        .trim_start_matches("async ")
        .trim_start_matches("final ");

    match language {
        Language::Rust => {
            if let Some(rest) = work.strip_prefix("fn ") {
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
        }
        Language::Python | Language::Ruby => {
            if let Some(rest) = work.strip_prefix("def ") {
                return rest
                    .split('(')
                    .next()
                    .or_else(|| rest.split_whitespace().next())
                    .map(|s| s.trim().to_string());
            }
        }
        Language::Go => {
            if let Some(rest) = work.strip_prefix("func ") {
                // func (r *T) Name( or func Name(
                let rest = if rest.starts_with('(') {
                    // Skip receiver: func (r *T) Name(
                    rest.find(") ").map(|i| &rest[i + 2..]).unwrap_or(rest)
                } else {
                    rest
                };
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
        }
        _ => {
            // Generic: look for fn/def/func prefix, or name( pattern
            if let Some(rest) = work.strip_prefix("fn ") {
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
            if let Some(rest) = work.strip_prefix("def ") {
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
            if let Some(rest) = work.strip_prefix("func ") {
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
            // JS/TS/Java/C#: word( pattern after stripping modifiers
            // But need to distinguish from field declarations, so require (
            if let Some(paren_pos) = work.find('(') {
                let before = work[..paren_pos].trim();
                // Could be "returnType methodName" or just "methodName"
                let name = before.split_whitespace().last().unwrap_or(before);
                if !name.is_empty()
                    && name.starts_with(|c: char| c.is_alphabetic() || c == '_')
                    && !name.contains('{')
                    && !name.contains('}')
                    && !name.contains('=')
                    && name != "if"
                    && name != "for"
                    && name != "while"
                    && name != "switch"
                    && name != "catch"
                    && name != "return"
                    && name != "new"
                    && name != "class"
                    && name != "interface"
                    && name != "struct"
                    && name != "enum"
                {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
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

    #[test]
    fn test_normalize_for_fts_cjk_truncation_no_panic() {
        // CJK characters are 3 bytes each in UTF-8. Build a string of CJK chars
        // that exceeds MAX_FTS_OUTPUT_LEN so truncation triggers inside multi-byte chars.
        // Each CJK char becomes a separate token with spaces: "X Y Z ..." so
        // output length ~ 2*num_chars. Need enough to exceed 16384.
        let cjk_heavy: String = "获".repeat(10000);
        let result = normalize_for_fts(&cjk_heavy);
        assert!(
            result.len() <= super::MAX_FTS_OUTPUT_LEN,
            "CJK FTS output should be capped but was {}",
            result.len()
        );
        // Verify the result is valid UTF-8 (implicit — it's a String)
        // and doesn't end mid-character
        assert!(result.is_char_boundary(result.len()));
    }

    // ===== Markdown NL tests =====

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

    #[test]
    fn test_strip_markdown_noise() {
        // Bold/italic
        assert_eq!(strip_markdown_noise("**bold** text"), "bold text");
        assert_eq!(strip_markdown_noise("*italic* text"), "italic text");
        assert_eq!(strip_markdown_noise("***both*** text"), "both text");

        // Headings
        assert_eq!(
            strip_markdown_noise("## Heading\nContent"),
            "Heading\nContent"
        );
        assert_eq!(strip_markdown_noise("### Deep\nStuff"), "Deep\nStuff");

        // Links → text only
        assert_eq!(
            strip_markdown_noise("[Click here](https://example.com)"),
            "Click here"
        );
        assert_eq!(
            strip_markdown_noise("[Config](config.md#section)"),
            "Config"
        );

        // Images removed entirely
        assert_eq!(strip_markdown_noise("![alt text](image.png)"), "");

        // HTML tags
        assert_eq!(strip_markdown_noise("<br>line<br/>break"), "linebreak");
        assert_eq!(
            strip_markdown_noise("<table><tr><td>data</td></tr></table>"),
            "data"
        );

        // Backticks → keep content
        assert_eq!(strip_markdown_noise("`code_here`"), "code_here");
        assert_eq!(
            strip_markdown_noise("```rust\nlet x = 1;\n```"),
            "rust\nlet x = 1;"
        );

        // Whitespace collapse
        assert_eq!(strip_markdown_noise("a   b\t\tc"), "a b c");
        assert_eq!(strip_markdown_noise("a\n\n\n\nb"), "a\n\nb");
    }

    #[test]
    fn test_strip_markdown_noise_empty() {
        assert_eq!(strip_markdown_noise(""), "");
        assert_eq!(strip_markdown_noise("   "), "");
        assert_eq!(strip_markdown_noise("\n\n\n"), "");
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
}
