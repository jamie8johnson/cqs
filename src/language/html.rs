//! HTML language definition
//!
//! HTML is the foundational markup language for the web and the outer grammar
//! for multi-grammar parsing (Svelte, Vue, Astro). Chunks are semantic elements:
//! headings, landmarks, script/style blocks, and id'd elements.

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting HTML chunks.
///
/// Captures all elements, script blocks, and style blocks.
/// `post_process_html` filters and classifies by semantic role.
const CHUNK_QUERY: &str = r#"
;; Regular elements — name is the tag
(element
  (start_tag
    (tag_name) @name)) @property

;; Self-closing elements
(element
  (self_closing_tag
    (tag_name) @name)) @property

;; Script blocks
(script_element
  (start_tag
    (tag_name) @name)) @property

;; Style blocks
(style_element
  (start_tag
    (tag_name) @name)) @property
"#;

/// Doc comment node types — HTML uses `<!-- ... -->` comments
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "div", "span", "class", "style", "href", "src", "alt", "title", "type", "value", "name",
    "content", "http", "equiv", "charset", "viewport", "width", "height", "rel", "stylesheet",
];

/// Semantic landmark tags that become Section chunks.
const LANDMARK_TAGS: &[&str] = &[
    "nav", "main", "header", "footer", "section", "article", "aside", "form",
];

/// Tags to filter out as structural noise (unless they have an id).
const NOISE_TAGS: &[&str] = &[
    "html", "head", "body", "div", "span", "p", "ul", "ol", "li", "table", "thead", "tbody",
    "tfoot", "tr", "td", "th", "br", "hr", "img", "a", "em", "strong", "b", "i", "u", "small",
    "sub", "sup", "abbr", "code", "pre", "blockquote", "dl", "dt", "dd", "link", "meta",
    "title", "base",
];

/// Post-process HTML chunks: classify by semantic role, filter noise.
fn post_process_html(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let tag = name.as_str();

    // Headings → Section
    if matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        *chunk_type = ChunkType::Section;
        // Try to extract heading text content
        if let Some(text) = extract_element_text(node, source) {
            if !text.is_empty() {
                *name = text;
            }
        }
        return true;
    }

    // Script and style → Module
    if tag == "script" || tag == "style" {
        *chunk_type = ChunkType::Module;
        // Try to get script type/src attribute for a better name
        let start_tag = find_child_by_kind(node, "start_tag");
        if let Some(start) = start_tag {
            if let Some(attr_val) = find_attribute_value(start, "src", source) {
                *name = format!("script:{attr_val}");
            } else if let Some(attr_val) = find_attribute_value(start, "type", source) {
                *name = format!("{tag}:{attr_val}");
            }
        }
        return true;
    }

    // Semantic landmarks → Section
    if LANDMARK_TAGS.contains(&tag) {
        *chunk_type = ChunkType::Section;
        // Check for id or aria-label
        let start_tag = find_child_by_kind(node, "start_tag");
        if let Some(start) = start_tag {
            if let Some(id) = find_attribute_value(start, "id", source) {
                *name = format!("{tag}#{id}");
            } else if let Some(label) = find_attribute_value(start, "aria-label", source) {
                *name = format!("{tag}:{label}");
            }
        }
        return true;
    }

    // Check if this noise tag has an id — keep it as Property
    if NOISE_TAGS.contains(&tag) {
        let start_tag = find_child_by_kind(node, "start_tag");
        if let Some(start) = start_tag {
            if let Some(id) = find_attribute_value(start, "id", source) {
                *name = format!("{tag}#{id}");
                *chunk_type = ChunkType::Property;
                return true;
            }
        }
        // No id — filter out
        return false;
    }

    // Everything else: keep as Property
    true
}

/// Find a direct child node by kind.
#[allow(clippy::manual_find)]
fn find_child_by_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(child);
        }
    }
    None
}

/// Find an attribute's value within a start_tag node.
fn find_attribute_value(start_tag: tree_sitter::Node, attr_name: &str, source: &str) -> Option<String> {
    let mut cursor = start_tag.walk();
    for child in start_tag.children(&mut cursor) {
        if child.kind() == "attribute" {
            // attribute has attribute_name and optionally quoted_attribute_value children
            let mut attr_cursor = child.walk();
            let mut found_name = false;
            for attr_child in child.children(&mut attr_cursor) {
                if attr_child.kind() == "attribute_name" {
                    let name_text = attr_child.utf8_text(source.as_bytes()).unwrap_or("");
                    if name_text == attr_name {
                        found_name = true;
                    }
                } else if found_name
                    && (attr_child.kind() == "quoted_attribute_value"
                        || attr_child.kind() == "attribute_value")
                {
                    let val = attr_child.utf8_text(source.as_bytes()).unwrap_or("");
                    // Strip quotes if present
                    let val = val.trim_matches('"').trim_matches('\'');
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// Extract text content from an element (for heading text).
fn extract_element_text(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "text" {
            let text = child.utf8_text(source.as_bytes()).unwrap_or("").trim();
            if !text.is_empty() {
                // Truncate long heading text
                let truncated = if text.len() > 80 {
                    format!("{}...", &text[..text.floor_char_boundary(77)])
                } else {
                    text.to_string()
                };
                return Some(truncated);
            }
        }
    }
    None
}

fn extract_return(_signature: &str) -> Option<String> {
    // HTML has no functions or return types
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "html",
    grammar: Some(|| tree_sitter_html::LANGUAGE.into()),
    extensions: &["html", "htm", "xhtml"],
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
    post_process_chunk: Some(post_process_html as PostProcessChunkFn),
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
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
    fn parse_html_heading_as_section() {
        let content = r#"<h1>Welcome to My Site</h1>
<p>Some paragraph text</p>
<h2>About</h2>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            chunks.iter().any(|c| c.name == "Welcome to My Site" && c.chunk_type == ChunkType::Section),
            "Expected h1 as Section, got: {:?}",
            names
        );
        assert!(
            chunks.iter().any(|c| c.name == "About" && c.chunk_type == ChunkType::Section),
            "Expected h2 as Section, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_html_script_as_module() {
        let content = r#"<html>
<head><title>Test</title></head>
<body>
<script src="app.js"></script>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let script = chunks.iter().find(|c| c.chunk_type == ChunkType::Module);
        assert!(
            script.is_some(),
            "Expected script as Module, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
        assert!(script.unwrap().name.contains("app.js"));
    }

    #[test]
    fn parse_html_landmark_as_section() {
        let content = r#"<nav id="main-nav">
  <a href="/">Home</a>
</nav>
<main>
  <article>Content here</article>
</main>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let nav = chunks
            .iter()
            .find(|c| c.name.contains("main-nav") && c.chunk_type == ChunkType::Section);
        assert!(
            nav.is_some(),
            "Expected nav#main-nav as Section, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_html_noise_filtered() {
        let content = r#"<div>
  <span>text</span>
  <p>paragraph</p>
</div>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // div, span, p are all noise — should be filtered out
        assert!(
            chunks.is_empty(),
            "Expected noise elements filtered, got: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_html_div_with_id_kept() {
        let content = r#"<div id="app">
  <p>content</p>
</div>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let app = chunks
            .iter()
            .find(|c| c.name == "div#app" && c.chunk_type == ChunkType::Property);
        assert!(
            app.is_some(),
            "Expected div#app as Property, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_html_no_calls() {
        let content = "<h1>Title</h1>\n";
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "HTML should have no calls");
        }
    }

    #[test]
    fn test_extract_return_html() {
        assert_eq!(extract_return("<div>test</div>"), None);
        assert_eq!(extract_return(""), None);
    }
}
