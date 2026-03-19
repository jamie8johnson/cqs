//! CSS language definition
//!
//! CSS is a styling language. Chunks are rule sets (selectors),
//! keyframes, and media statements. No meaningful call graph.

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting CSS chunks.
///
/// CSS constructs:
///   - `rule_set` → Property (selector with declarations)
///   - `keyframes_statement` → Property (animation, post-processed to Section)
///   - `media_statement` → Property (media query, post-processed to Section)
const CHUNK_QUERY: &str = r#"
;; Rule set: .class { color: red; }
(rule_set
  (selectors) @name) @property

;; Keyframes: @keyframes spin { ... }
(keyframes_statement
  (keyframes_name) @name) @property

;; Media query: @media (max-width: 600px) { ... }
(media_statement) @property
"#;

/// Doc comment node types — CSS uses `/* ... */` comments
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "auto", "inherit", "initial", "unset", "none", "block", "inline", "flex", "grid", "absolute",
    "relative", "fixed", "sticky", "hidden", "visible", "solid", "dashed", "dotted", "normal",
    "bold", "italic", "center", "left", "right", "top", "bottom", "transparent", "currentColor",
    "important", "media", "keyframes", "from", "to",
];

/// Post-process CSS chunks to set correct types.
fn post_process_css(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "rule_set" => *chunk_type = ChunkType::Property,
        "keyframes_statement" => *chunk_type = ChunkType::Section,
        "media_statement" => {
            *chunk_type = ChunkType::Section;
            // Media statements don't have a named child captured as @name,
            // so extract a summary from the source text
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            // Extract the condition: @media (max-width: 600px) → "(max-width: 600px)"
            if let Some(brace) = text.find('{') {
                // Extract everything between @media and { as the query
                let after_media = if text.starts_with("@media") { 6 } else { 0 };
                if after_media < brace {
                    let query = text[after_media..brace].trim();
                    if !query.is_empty() {
                        *name = format!("@media {query}");
                        return true;
                    }
                }
            }
            *name = "@media".to_string();
        }
        _ => {}
    }
    true
}

fn extract_return(_signature: &str) -> Option<String> {
    // CSS has no functions or return types
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "css",
    grammar: Some(|| tree_sitter_css::LANGUAGE.into()),
    extensions: &["css"],
    chunk_query: CHUNK_QUERY,
    call_query: None,
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_css as PostProcessChunkFn),
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
    injections: &[],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{ChunkType, Parser};
    use std::io::Write;

    fn write_temp_file(content: &str, ext: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn parse_css_rule_set() {
        let content = r#"
.container {
    display: flex;
    padding: 16px;
}
"#;
        let file = write_temp_file(content, "css");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let rule = chunks
            .iter()
            .find(|c| c.name.contains("container") && c.chunk_type == ChunkType::Property);
        assert!(rule.is_some(), "Should find '.container' rule set");
    }

    #[test]
    fn parse_css_keyframes() {
        let content = r#"
@keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
}
"#;
        let file = write_temp_file(content, "css");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let kf = chunks
            .iter()
            .find(|c| c.name == "spin" && c.chunk_type == ChunkType::Section);
        assert!(kf.is_some(), "Should find 'spin' keyframes as Section");
    }

    #[test]
    fn parse_css_no_calls() {
        let content = r#"
body {
    margin: 0;
    font-family: sans-serif;
}
"#;
        let file = write_temp_file(content, "css");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "CSS should have no calls");
        }
    }

    #[test]
    fn test_extract_return_css() {
        assert_eq!(extract_return(".class { color: red; }"), None);
        assert_eq!(extract_return(""), None);
    }
}
