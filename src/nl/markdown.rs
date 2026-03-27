//! Markdown stripping and JSDoc parsing.

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
    result.retain(|c| c != '*' && c != '`');
    let result: Cow<str> = MULTI_WHITESPACE_RE.replace_all(&result, " ");
    let result: Cow<str> = MULTI_NEWLINE_RE.replace_all(&result, "\n\n");
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // Links -> text only
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

        // Backticks -> keep content
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

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        proptest! {
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
        }
    }
}
