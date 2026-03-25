//! Svelte language definition
//!
//! Svelte components combine HTML-like template markup with `<script>` and
//! `<style>` blocks. The grammar mirrors HTML's structure closely, so we
//! reuse HTML's `detect_script_language`, `find_child_by_kind`, and
//! `find_attribute_value` helpers.

use super::{ChunkType, InjectionRule, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Svelte component chunks.
///
/// Elements → Property (filtered/reclassified by post-process),
/// Script blocks → Module, Style blocks → Module.
const CHUNK_QUERY: &str = r#"
;; Regular elements
(element
  (start_tag (tag_name) @name)) @property

;; Self-closing elements
(element
  (self_closing_tag (tag_name) @name)) @property

;; Script blocks
(script_element
  (start_tag (tag_name) @name)) @property

;; Style blocks
(style_element
  (start_tag (tag_name) @name)) @property
"#;

// No call query — JS/CSS calls are extracted via injection
// No type query — Svelte templates don't have typed references

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "div", "span", "p", "a", "img", "ul", "ol", "li", "table", "tr", "td", "th", "form", "input",
    "button", "label", "select", "option", "textarea", "br", "hr", "head", "body", "html", "meta",
    "link", "title", "script", "style", "class", "id", "href", "src", "alt", "type", "value",
    "name", "slot", "each", "if", "else", "await", "then", "catch", "key", "let", "const",
    "export", "import", "bind", "on", "use", "transition", "animate", "in", "out",
];

/// HTML heading tags
const HEADING_TAGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// HTML landmark tags — always kept in output
const LANDMARK_TAGS: &[&str] = &[
    "nav", "main", "header", "footer", "aside", "section", "article", "form",
];

/// Tags that are noise unless they have an `id` attribute
const NOISE_TAGS: &[&str] = &[
    "div", "span", "p", "ul", "ol", "li", "table", "tr", "td", "th", "dl", "dt", "dd", "figure",
    "figcaption", "details", "summary", "blockquote", "pre", "code", "a", "img", "button", "input",
    "label", "select", "textarea", "option",
];

/// Post-process Svelte element chunks.
///
/// Same logic as HTML: headings→Section, script/style→Module,
/// landmarks→Section, noise→filter unless id, else Property.
fn post_process_svelte(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let tag = name.to_lowercase();

    // Headings → Section with text content
    if HEADING_TAGS.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        // Extract text content from heading
        let content = &source[node.byte_range()];
        // Strip tags for the name
        let text = content
            .split('>')
            .nth(1)
            .and_then(|s| s.split('<').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !text.is_empty() {
            *name = text;
        }
        return true;
    }

    // Script/style → Module
    if tag == "script" || tag == "style" {
        *chunk_type = ChunkType::Module;
        // Try to name from src or type attribute
        let start_tag = super::html::find_child_by_kind(node, "start_tag");
        if let Some(st) = start_tag {
            if let Some(src_val) = super::html::find_attribute_value(st, "src", source) {
                *name = format!("script:{src_val}");
                return true;
            }
            if let Some(lang_val) = super::html::find_attribute_value(st, "lang", source) {
                *name = format!("{tag}:{lang_val}");
                return true;
            }
        }
        return true;
    }

    // Landmarks → Section with id/aria-label
    if LANDMARK_TAGS.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        let start_tag = super::html::find_child_by_kind(node, "start_tag");
        if let Some(st) = start_tag {
            if let Some(id) = super::html::find_attribute_value(st, "id", source) {
                *name = format!("{tag}#{id}");
                return true;
            }
            if let Some(label) = super::html::find_attribute_value(st, "aria-label", source) {
                *name = format!("{tag}:{label}");
                return true;
            }
        }
        return true;
    }

    // Noise tags → filter unless they have an id
    if NOISE_TAGS.contains(&tag.as_str()) {
        let start_tag = super::html::find_child_by_kind(node, "start_tag")
            .or_else(|| super::html::find_child_by_kind(node, "self_closing_tag"));
        if let Some(st) = start_tag {
            if let Some(id) = super::html::find_attribute_value(st, "id", source) {
                *name = format!("{tag}#{id}");
                *chunk_type = ChunkType::Property;
                return true;
            }
        }
        return false; // Filter out
    }

    true
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "svelte",
    grammar: Some(|| tree_sitter_svelte::LANGUAGE.into()),
    extensions: &["svelte"],
    chunk_query: CHUNK_QUERY,
    call_query: None,
    signature_style: SignatureStyle::Breadcrumb,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_svelte),
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
    injections: &[
        // <script> blocks → JavaScript (or TypeScript via detect_script_language)
        InjectionRule {
            container_kind: "script_element",
            content_kind: "raw_text",
            target_language: "javascript",
            detect_language: Some(super::html::detect_script_language),
            content_scoped_lines: false,
        },
        // <style> blocks → CSS
        InjectionRule {
            container_kind: "style_element",
            content_kind: "raw_text",
            target_language: "css",
            detect_language: None,
            content_scoped_lines: false,
        },
    ],
    doc_format: "default",
    doc_convention: "",
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
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
    /// Parses a Svelte file containing script and markup sections, verifying that JavaScript functions within the `<script>` block are correctly extracted as separate code chunks with their function names and JavaScript language classification.
    /// 
    /// This is a test function that validates the parser's ability to handle Svelte component files by injecting and extracting embedded JavaScript code blocks.
    /// 
    /// # Arguments
    /// 
    /// None. This function is a standalone test with hardcoded Svelte content.
    /// 
    /// # Returns
    /// 
    /// Nothing. This function asserts on parser output and panics if assertions fail.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to extract the expected JavaScript functions `handleClick` and `formatName` from the Svelte script block, or if file creation/parsing operations fail.

    #[test]
    fn parse_svelte_with_script() {
        let content = r#"<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}

function formatName(first, last) {
    return `${first} ${last}`;
}
</script>

<h1>Hello World</h1>
<button on:click={handleClick}>Click me</button>
"#;
        let file = write_temp_file(content, "svelte");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // JS functions should be extracted via injection
        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_chunks.iter().any(|c| c.name == "handleClick"),
            "Expected JS function 'handleClick', got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
        assert!(
            js_chunks.iter().any(|c| c.name == "formatName"),
            "Expected JS function 'formatName'"
        );
    }
    /// Parses a Svelte file containing a TypeScript script block and verifies that TypeScript code is correctly identified and extracted.
    /// 
    /// This test function creates a temporary Svelte file with a `<script lang="ts">` block containing TypeScript code (interface and function definitions), parses it using the Parser, and asserts that the TypeScript function `greet` is correctly detected and categorized as TypeScript language.
    /// 
    /// # Returns
    /// 
    /// Nothing. This function panics if the assertion fails, indicating that the TypeScript function was not properly parsed or identified.
    /// 
    /// # Panics
    /// 
    /// Panics if the parsed chunks do not contain a TypeScript chunk with the function name "greet", or if file creation or parsing operations fail.

    #[test]
    fn parse_svelte_with_typescript() {
        let content = r#"<script lang="ts">
interface User {
    name: string;
    age: number;
}

function greet(user: User): string {
    return `Hello, ${user.name}!`;
}
</script>

<p>Content</p>
"#;
        let file = write_temp_file(content, "svelte");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // TypeScript should be detected via lang="ts"
        let ts_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::TypeScript)
            .collect();
        assert!(
            ts_chunks.iter().any(|c| c.name == "greet"),
            "Expected TS function 'greet' from <script lang=\"ts\">, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
    }
    /// Parses a Svelte file containing script, template, and style blocks, and verifies that CSS chunks are correctly extracted from the `<style>` block.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts that CSS chunks are present in the parsed output.
    /// 
    /// # Panics
    /// 
    /// Panics if no CSS chunks are found in the parsed Svelte file, indicating that the style block was not properly parsed or extracted.

    #[test]
    fn parse_svelte_with_style() {
        let content = r#"<script>
function init() { return 1; }
</script>

<div class="container">Hello</div>

<style>
.container {
    max-width: 1200px;
    margin: 0 auto;
}

body {
    font-family: sans-serif;
}
</style>
"#;
        let file = write_temp_file(content, "svelte");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // CSS chunks from style block
        let css_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Css)
            .collect();
        assert!(
            !css_chunks.is_empty(),
            "Expected CSS chunks from <style> block, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
    }
    /// Parses a Svelte template file and verifies that semantic HTML elements are correctly extracted as chunks.
    /// 
    /// This test function creates a temporary Svelte file containing a script block, heading, navigation landmark, and main content area. It then parses the file and asserts that the parser correctly identifies and extracts the heading as a Section chunk and the nav element as a landmark with the expected name format.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (unit test function)
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following assertions fail:
    /// - The "Counter App" heading is not found in the extracted sections
    /// - The nav landmark with id "main-nav" is not found in the extracted sections

    #[test]
    fn parse_svelte_template_elements() {
        let content = r#"<script>
let count = 0;
</script>

<h1>Counter App</h1>

<nav id="main-nav">
  <a href="/">Home</a>
  <a href="/about">About</a>
</nav>

<main>
  <p>Count: {count}</p>
</main>
"#;
        let file = write_temp_file(content, "svelte");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Heading should be extracted as Section
        let sections: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Section)
            .collect();
        assert!(
            sections.iter().any(|c| c.name == "Counter App"),
            "Expected heading 'Counter App' as Section, got: {:?}",
            sections.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        // Nav landmark
        assert!(
            sections.iter().any(|c| c.name == "nav#main-nav"),
            "Expected nav landmark, got: {:?}",
            sections.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }
    /// Parses a Svelte component file and verifies that the call graph correctly identifies function relationships.
    /// 
    /// This test function creates a temporary Svelte file containing nested function calls, parses it using the Parser to extract the call graph, and asserts that the `handleSubmit` function is properly recorded as calling `fetchData`.
    /// 
    /// # Arguments
    /// 
    /// None. This function operates on hardcoded Svelte source code.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the call graph does not contain an entry for `handleSubmit` or if `handleSubmit` does not list `fetchData` as a callee.

    #[test]
    fn parse_svelte_call_graph() {
        let content = r#"<script>
function fetchData() {
    return fetch('/api/data');
}

function handleSubmit(event) {
    const data = fetchData();
    process(data);
}
</script>

<button on:click={handleSubmit}>Submit</button>
"#;
        let file = write_temp_file(content, "svelte");
        let parser = Parser::new().unwrap();
        let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

        let handler = calls.iter().find(|c| c.name == "handleSubmit");
        assert!(
            handler.is_some(),
            "Expected call graph for 'handleSubmit', got: {:?}",
            calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        let callee_names: Vec<_> = handler
            .unwrap()
            .calls
            .iter()
            .map(|c| c.callee_name.as_str())
            .collect();
        assert!(
            callee_names.contains(&"fetchData"),
            "Expected handleSubmit→fetchData, got: {:?}",
            callee_names
        );
    }
    /// Verifies that a template-only Svelte component (without script or style blocks) is parsed correctly with all chunks identified as Svelte language and expected content preserved.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded Svelte template content.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if any parsed chunk has a language other than Svelte, or if the expected heading "Simple Page" is not found in the parsed chunks.

    #[test]
    fn parse_svelte_no_script_unchanged() {
        // Template-only Svelte component — no injection should fire
        let content = r#"<h1>Simple Page</h1>
<nav id="sidebar">
  <ul>
    <li><a href="/">Home</a></li>
  </ul>
</nav>
<main>
  <p>Hello world</p>
</main>
"#;
        let file = write_temp_file(content, "svelte");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // All chunks should be Svelte (no JS/CSS)
        for chunk in &chunks {
            assert_eq!(
                chunk.language,
                crate::parser::Language::Svelte,
                "Template-only Svelte should only have Svelte chunks, found {:?} for '{}'",
                chunk.language,
                chunk.name
            );
        }

        assert!(
            chunks.iter().any(|c| c.name == "Simple Page"),
            "Expected heading 'Simple Page'"
        );
    }
}
