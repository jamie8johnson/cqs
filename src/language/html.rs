//! HTML language definition
//!
//! HTML is the foundational markup language for the web. Chunks are semantic
//! elements: headings, landmarks, and id'd elements. Inline `<script>` blocks
//! extract JS/TS functions and `<style>` blocks extract CSS rules via
//! multi-grammar injection.

use super::{ChunkType, InjectionRule, LanguageDef, PostProcessChunkFn, SignatureStyle};

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
fn find_child_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    crate::parser::find_child_by_kind(node, kind)
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

/// Detect script language from `<script>` element attributes.
///
/// Checks for `lang="ts"`, `type="text/typescript"`, or similar attributes
/// that indicate TypeScript instead of the default JavaScript.
fn detect_script_language(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    // Find the start_tag child
    let start_tag = find_child_by_kind(node, "start_tag")?;

    // Check lang attribute: <script lang="ts">
    if let Some(lang_val) = find_attribute_value(start_tag, "lang", source) {
        let lower = lang_val.to_lowercase();
        if lower == "ts" || lower == "typescript" {
            tracing::debug!("Detected TypeScript from lang attribute");
            return Some("typescript");
        }
    }

    // Check type attribute: <script type="text/typescript">
    if let Some(type_val) = find_attribute_value(start_tag, "type", source) {
        let lower = type_val.to_lowercase();
        if lower.contains("typescript") {
            tracing::debug!("Detected TypeScript from type attribute");
            return Some("typescript");
        }
        // Skip non-JS script types (JSON-LD, templates, shaders, etc.)
        if !lower.is_empty()
            && !matches!(
                lower.as_str(),
                "text/javascript"
                    | "application/javascript"
                    | "module"
                    | "text/ecmascript"
                    | "application/ecmascript"
            )
        {
            tracing::debug!(r#type = %type_val, "Skipping non-JS script type");
            return Some("_skip"); // sentinel: caller will skip injection
        }
    }

    None // Use default (javascript)
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
    injections: &[
        InjectionRule {
            container_kind: "script_element",
            content_kind: "raw_text",
            target_language: "javascript",
            detect_language: Some(detect_script_language),
        },
        InjectionRule {
            container_kind: "style_element",
            content_kind: "raw_text",
            target_language: "css",
            detect_language: None,
        },
    ],
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

    // --- Multi-grammar injection tests ---

    #[test]
    fn parse_html_with_script_extracts_js_functions() {
        let content = r#"<html>
<body>
<h1>Title</h1>
<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}

function setupListeners() {
    handleClick(null);
}
</script>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Should have JS function chunks
        let js_funcs: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_funcs.iter().any(|c| c.name == "handleClick"),
            "Expected JS function 'handleClick', got: {:?}",
            js_funcs.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        assert!(
            js_funcs.iter().any(|c| c.name == "setupListeners"),
            "Expected JS function 'setupListeners', got: {:?}",
            js_funcs.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        // JS functions should have correct language
        for f in &js_funcs {
            assert_eq!(f.language, crate::parser::Language::JavaScript);
            assert_eq!(f.chunk_type, ChunkType::Function);
        }

        // HTML heading should still be present
        assert!(
            chunks.iter().any(|c| c.name == "Title" && c.chunk_type == ChunkType::Section),
            "Expected HTML heading 'Title'"
        );

        // The script Module chunk should have been replaced by JS functions
        assert!(
            !chunks.iter().any(|c| c.chunk_type == ChunkType::Module && c.name == "script"),
            "Script Module chunk should be replaced by JS functions"
        );
    }

    #[test]
    fn parse_html_with_style_extracts_css_rules() {
        let content = r#"<html>
<head>
<style>
.container {
    display: flex;
    gap: 1rem;
}
</style>
</head>
<body><h1>Page</h1></body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // CSS chunks should be extracted (if CSS query captures rules)
        let css_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Css)
            .collect();

        // CSS injection must produce chunks — if this fails, CSS injection is broken
        assert!(
            !css_chunks.is_empty(),
            "CSS injection should extract chunks from <style> block"
        );
        // Style Module chunk should be replaced by CSS chunks
        assert!(
            !chunks.iter().any(|c| c.chunk_type == ChunkType::Module && c.name == "style"),
            "Style Module chunk should be replaced by CSS chunks"
        );

        // HTML heading should still be present
        assert!(
            chunks.iter().any(|c| c.name == "Page" && c.chunk_type == ChunkType::Section),
            "Expected HTML heading 'Page'"
        );
    }

    #[test]
    fn parse_html_with_typescript_script() {
        let content = r#"<html>
<body>
<script lang="ts">
function typedFunction(x: number): string {
    return x.toString();
}
</script>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let ts_funcs: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::TypeScript)
            .collect();
        assert!(
            ts_funcs.iter().any(|c| c.name == "typedFunction"),
            "Expected TypeScript function, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_html_with_empty_script_keeps_module() {
        // <script src="..."> has no raw_text child — should keep outer Module
        let content = r#"<html>
<body>
<script src="app.js"></script>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let module = chunks.iter().find(|c| c.chunk_type == ChunkType::Module);
        assert!(
            module.is_some(),
            "Empty script should keep Module chunk, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_html_with_multiple_scripts() {
        let content = r#"<html>
<body>
<script>
function first() { return 1; }
</script>
<script>
function second() { return 2; }
</script>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let js_names: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .map(|c| c.name.as_str())
            .collect();
        assert!(
            js_names.contains(&"first"),
            "Expected 'first' from first script, got: {:?}",
            js_names
        );
        assert!(
            js_names.contains(&"second"),
            "Expected 'second' from second script, got: {:?}",
            js_names
        );
    }

    #[test]
    fn parse_html_with_whitespace_only_script_keeps_module() {
        let content = "<html><body>\n<script>  \n  </script>\n</body></html>\n";
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Whitespace-only script produces zero inner chunks — should keep outer
        let has_module = chunks.iter().any(|c| c.chunk_type == ChunkType::Module);
        assert!(
            has_module,
            "Whitespace-only script should keep Module chunk, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_html_without_script_unchanged() {
        // HTML with only headings/nav — no injections should fire
        let content = r#"<html>
<body>
<nav id="main-nav"><a href="/">Home</a></nav>
<h1>Welcome</h1>
<h2>About</h2>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Should have only HTML chunks
        for chunk in &chunks {
            assert_eq!(
                chunk.language,
                crate::parser::Language::Html,
                "All chunks should be HTML, found {:?} for '{}'",
                chunk.language,
                chunk.name
            );
        }

        // Verify expected chunks
        assert!(chunks.iter().any(|c| c.name == "Welcome"));
        assert!(chunks.iter().any(|c| c.name == "About"));
        assert!(chunks.iter().any(|c| c.name.contains("main-nav")));
    }

    #[test]
    fn injection_call_graph() {
        let content = r#"<html>
<body>
<script>
function caller() {
    helper();
    other();
}

function helper() {
    return 42;
}
</script>
</body>
</html>
"#;
        let file = write_temp_file(content, "html");
        let parser = Parser::new().unwrap();
        let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

        let caller_calls = calls.iter().find(|c| c.name == "caller");
        assert!(
            caller_calls.is_some(),
            "Expected call graph entry for 'caller', got: {:?}",
            calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        let call_names: Vec<_> = caller_calls
            .unwrap()
            .calls
            .iter()
            .map(|c| c.callee_name.as_str())
            .collect();
        assert!(
            call_names.contains(&"helper"),
            "Expected caller → helper, got: {:?}",
            call_names
        );
        assert!(
            call_names.contains(&"other"),
            "Expected caller → other, got: {:?}",
            call_names
        );
    }

    #[test]
    fn injection_ranges_empty_for_non_injection_language() {
        // Rust files have no injection rules — should return empty
        let content = "fn main() {}\n";
        let file = write_temp_file(content, "rs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].language, crate::parser::Language::Rust);
    }
}
