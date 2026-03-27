//! Field and method name extraction from code chunks.

use crate::parser::{FieldStyle, Language};

use super::fts::tokenize_identifier;

/// Returns true if a trimmed line should be skipped during field extraction.
///
/// Universal skips (comments, braces) apply to all languages. Language-specific
/// skips (struct/class/enum headers, decorators) come from `LanguageDef::skip_line_prefixes`.
fn should_skip_line(trimmed: &str, lang: Language) -> bool {
    // Universal skips (all languages)
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed == "{"
        || trimmed == "}"
    {
        return true;
    }
    // Language-specific skips
    let lang_def = lang.def();
    for prefix in lang_def.skip_line_prefixes {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Validates and returns a field name, or `None` if it looks like a keyword,
/// variant with data, or is too short.
fn validate_field_name(name: Option<&str>) -> Option<&str> {
    let name = name?.trim();
    if name.is_empty()
        || name.len() <= 1
        || name.contains('(')
        || name.contains('{')
        || !name.starts_with(|c: char| c.is_alphabetic() || c == '_')
    {
        return None;
    }
    Some(name)
}

/// Strip space-separated prefixes from a line.
///
/// Each prefix in `prefixes` (split on whitespace) is tried with a trailing
/// space. Longer prefixes are tried first to avoid partial matches (e.g.,
/// "pub" matching inside "pub(crate)").
fn strip_prefixes<'a>(line: &'a str, prefixes: &str) -> &'a str {
    let mut result = line;
    // Sort prefixes longest-first so "pub(crate)" is tried before "pub"
    let mut plist: Vec<String> = prefixes
        .split_whitespace()
        .map(|p| format!("{} ", p))
        .collect();
    plist.sort_by_key(|s| std::cmp::Reverse(s.len()));
    // Apply repeatedly — a line like "public static final int x" needs multiple passes
    let mut changed = true;
    let mut iters = 0;
    while changed && iters < 20 {
        iters += 1;
        changed = false;
        for with_space in &plist {
            if let Some(rest) = result.strip_prefix(with_space.as_str()) {
                result = rest;
                changed = true;
                break; // restart from longest prefix
            }
        }
    }
    result
}

/// Extract field/variant names from struct, enum, or class content.
///
/// Uses `FieldStyle` from the language definition to determine extraction
/// strategy. Supports `NameFirst` (name before separator) and `TypeFirst`
/// (type before name) patterns across all 51 languages.
pub(super) fn extract_field_names(content: &str, language: Language) -> Vec<String> {
    let _span = tracing::debug_span!("extract_field_names", %language).entered();

    let field_style = language.def().field_style;
    if field_style == FieldStyle::None {
        return Vec::new();
    }

    let mut fields = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if should_skip_line(trimmed, language) {
            continue;
        }

        let field = match field_style {
            FieldStyle::NameFirst {
                separators,
                strip_prefixes: prefixes,
            } => {
                let clean = strip_prefixes(trimmed, prefixes);
                let sep_chars: Vec<char> = separators.chars().collect();
                clean
                    .split(sep_chars.as_slice())
                    .next()
                    .map(|s| s.trim().trim_end_matches(','))
            }
            FieldStyle::TypeFirst {
                strip_prefixes: prefixes,
            } => {
                let clean = strip_prefixes(trimmed, prefixes);
                // Split on terminators, take first segment: "int maxSize" from "int maxSize;"
                let before_term = clean
                    .split([';', ',', '=', '{'])
                    .next()
                    .unwrap_or("")
                    .trim();
                // Last whitespace-delimited token is the field name
                let name = before_term.rsplit_once(char::is_whitespace).map(|(_, n)| n);
                // Strip pointer/reference markers (C/C++)
                name.map(|n| n.trim_start_matches(['*', '&']))
            }
            FieldStyle::None => unreachable!(),
        };

        if let Some(name) = validate_field_name(field) {
            let tokenized = tokenize_identifier(name).join(" ");
            if !tokenized.is_empty() {
                fields.push(tokenized);
            }
        }

        if fields.len() >= 15 {
            break;
        }
    }

    if fields.is_empty() && !content.is_empty() {
        tracing::trace!(%language, "No fields extracted from content");
    }

    fields
}

/// Extract member method/function names from class/struct/interface content.
///
/// Scans lines for common method declaration patterns across languages.
/// Returns raw method names (not tokenized) — caller tokenizes for NL.
pub(super) fn extract_member_method_names(content: &str, language: Language) -> Vec<String> {
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
            if let Some(rest) = work.strip_prefix("fun ") {
                // Kotlin
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
            if let Some(rest) = work.strip_prefix("sub ") {
                // Perl, VB.NET
                return rest.split(['(', ' ']).next().map(|s| s.trim().to_string());
            }
            if let Some(rest) = work.strip_prefix("proc ") {
                // Elixir (defp), Nim, Tcl
                return rest.split('(').next().map(|s| s.trim().to_string());
            }
            if let Some(rest) = work.strip_prefix("method ") {
                // Raku, some OOP
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

    // ===== extract_field_names regression tests =====

    #[test]
    fn test_extract_field_names_rust() {
        let content = "pub struct Config {\n    pub name: String,\n    pub(crate) max_size: usize,\n    enabled: bool,\n}";
        let result = extract_field_names(content, Language::Rust);
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_go() {
        let content = "type Config struct {\n    Name string\n    MaxSize int\n    Enabled bool\n}";
        let result = extract_field_names(content, Language::Go);
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_python() {
        let content = "class Config:\n    name: str\n    max_size: int = 100\n    enabled = True";
        let result = extract_field_names(content, Language::Python);
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_typescript() {
        let content = "class Config {\n    public name: string;\n    private maxSize: number;\n    readonly enabled: boolean;\n}";
        let result = extract_field_names(content, Language::TypeScript);
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_javascript() {
        let content = "class Config {\n    name = 'default';\n    maxSize = 100;\n}";
        let result = extract_field_names(content, Language::JavaScript);
        assert_eq!(result, vec!["name", "max size"]);
    }

    #[test]
    fn test_extract_field_names_java() {
        // Java fields are `type name;` — TypeFirst extraction strips access modifiers,
        // splits on terminators, and takes the last whitespace token (the field name).
        let content = "class Config {\n    private String name;\n    protected int maxSize;\n    public boolean enabled;\n}";
        let result = extract_field_names(content, Language::Java);
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_empty_content() {
        let result = extract_field_names("", Language::Rust);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_extract_field_names_only_comments() {
        let content = "// this is a comment\n// another comment\n/* block comment */";
        let result = extract_field_names(content, Language::Rust);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_extract_field_names_header_and_brace_only() {
        let content = "pub struct Empty {\n}";
        let result = extract_field_names(content, Language::Rust);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_extract_field_names_unicode_no_panic() {
        let content = "class Config {\n    café: string;\n}";
        let result = extract_field_names(content, Language::TypeScript);
        // Just verify no panic; check actual output
        assert_eq!(result, vec!["café"]);
    }

    #[test]
    fn test_extract_field_names_capped_at_15() {
        let mut lines = vec!["pub struct Big {".to_string()];
        for i in 0..20 {
            lines.push(format!("    pub field_{}: i32,", i));
        }
        lines.push("}".to_string());
        let content = lines.join("\n");
        let result = extract_field_names(&content, Language::Rust);
        assert_eq!(result.len(), 15);
    }

    #[test]
    fn test_extract_field_names_unsupported_language() {
        let content = "NAME=\"default\"\nMAX_SIZE=100";
        let result = extract_field_names(content, Language::Bash);
        assert_eq!(result, Vec::<String>::new());
    }

    // ===== extract_field_names: TypeFirst languages =====

    #[test]
    fn test_extract_field_names_c() {
        let content =
            "struct Config {\n    const char *name;\n    int max_size;\n    bool enabled;\n};";
        let result = extract_field_names(content, Language::C);
        // TypeFirst: strips "const", takes last token before ;, strips pointer marker *
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_cpp() {
        let content =
            "class Widget {\n    std::string title;\n    int width;\n    bool visible;\n};";
        let result = extract_field_names(content, Language::Cpp);
        assert_eq!(result, vec!["title", "width", "visible"]);
    }

    #[test]
    fn test_extract_field_names_csharp() {
        let content = "class Config {\n    public string Name;\n    private int MaxSize;\n    protected bool Enabled;\n}";
        let result = extract_field_names(content, Language::CSharp);
        // TypeFirst: strips access modifiers, takes last token before ;
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    // ===== extract_field_names: NameFirst languages with keyword prefixes =====

    #[test]
    fn test_extract_field_names_kotlin() {
        let content = "data class Config(\n    val name: String,\n    var maxSize: Int,\n    private val enabled: Boolean\n)";
        let result = extract_field_names(content, Language::Kotlin);
        // NameFirst: strips val/var/private, splits on :, tokenizes camelCase
        assert_eq!(result, vec!["name", "max size", "enabled"]);
    }

    #[test]
    fn test_extract_field_names_swift() {
        let content = "struct Config {\n    let name: String\n    var maxSize: Int\n    weak var delegate: Delegate?\n}";
        let result = extract_field_names(content, Language::Swift);
        // NameFirst: strips let/var/weak, splits on :
        assert_eq!(result, vec!["name", "max size", "delegate"]);
    }

    #[test]
    fn test_extract_field_names_scala() {
        let content = "case class Config(\n    val name: String,\n    var maxSize: Int\n)";
        let result = extract_field_names(content, Language::Scala);
        // NameFirst: strips val/var, splits on :
        assert_eq!(result, vec!["name", "max size"]);
    }

    #[test]
    fn test_extract_field_names_php() {
        // PHP fields use $ prefix which fails validate_field_name (not alphabetic start).
        // This is a known limitation: NameFirst extraction keeps "$name" intact,
        // and validate_field_name rejects it because '$' is not alphabetic or '_'.
        let content =
            "class Config {\n    public $name = 'default';\n    private $maxSize = 100;\n}";
        let result = extract_field_names(content, Language::Php);
        assert_eq!(
            result,
            Vec::<String>::new(),
            "PHP $ fields rejected by validator: {result:?}"
        );
    }

    #[test]
    fn test_extract_field_names_ruby() {
        // Ruby attr_accessor lines yield ":name" after stripping the prefix.
        // The colon prefix fails validate_field_name (not alphabetic start).
        // However, "end" passes validation (alphabetic, len > 1).
        // Known limitation: actual field names are not extracted, only the "end" keyword leaks through.
        let content = "class Config\n  attr_accessor :name\n  attr_reader :max_size\nend";
        let result = extract_field_names(content, Language::Ruby);
        assert!(
            !result
                .iter()
                .any(|f| f.contains("name") || f.contains("max")),
            "Ruby : fields should not extract actual field names: {result:?}"
        );
    }

    // ===== extract_field_names: NameFirst assignment languages =====

    #[test]
    fn test_extract_field_names_lua() {
        // Lua table assignment: "Config.name = ..." extracts "Config.name" as a single token
        // because dot is not a tokenizer delimiter. The table name prefix is included.
        let content = "local Config = {}\nConfig.name = 'default'\nConfig.max_size = 100";
        let result = extract_field_names(content, Language::Lua);
        // First line: strip "local" -> "Config = {}" -> split on = -> "Config" -> "config"
        // Second: "Config.name" -> tokenize -> "config.name" (dot not a delimiter)
        // Third: "Config.max_size" -> tokenize -> "config.max" + "size" (underscore splits)
        assert_eq!(result, vec!["config", "config.name", "config.max size"]);
    }

    #[test]
    fn test_extract_field_names_protobuf() {
        // Protobuf uses "type name = N;" syntax but NameFirst with space separator
        // extracts the first space-delimited token, which is the type name.
        // This is a known limitation: protobuf gets type names instead of field names.
        let content = "message Config {\n    string name = 1;\n    int32 max_size = 2;\n    bool enabled = 3;\n}";
        let result = extract_field_names(content, Language::Protobuf);
        // "message" line not skipped (no skip rule for it), extracts "message"
        // Subsequent lines extract type names: "string", "int32", "bool"
        assert!(
            !result.is_empty(),
            "protobuf should extract something (even if type names): {result:?}"
        );
        // Verify it extracts the type tokens (not field names — known limitation)
        assert!(
            result
                .iter()
                .any(|f| f == "message" || f == "string" || f == "bool"),
            "protobuf extracts type tokens with space separator: {result:?}"
        );
    }

    #[test]
    fn extract_method_name_kotlin_fun() {
        let name = extract_method_name_from_line(
            "fun processData(input: String): Result",
            Language::Kotlin,
        );
        assert_eq!(name.as_deref(), Some("processData"));
    }

    #[test]
    fn extract_method_name_perl_sub() {
        let name = extract_method_name_from_line("sub calculate_total {", Language::Perl);
        assert_eq!(name.as_deref(), Some("calculate_total"));
    }
}
