//! Vue language definition
//!
//! Vue single-file components (`.vue`) combine HTML-like template markup with
//! `<script>` and `<style>` blocks. The grammar extends HTML's grammar with
//! Vue-specific additions (template_element, interpolation, directive_attribute).
//! Script/style injection is identical to HTML's pattern.

use super::{ChunkType, FieldStyle, InjectionRule, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Vue SFC chunks.
///
/// Elements → Property (filtered/reclassified by post-process).
/// Script/style blocks are captured as Module but are replaced by injected
/// JS/CSS chunks during injection phase. Template blocks remain as Module.
const CHUNK_QUERY: &str = r#"
;; Regular elements
(element
  (start_tag (tag_name) @name)) @property

;; Self-closing elements
(element
  (self_closing_tag (tag_name) @name)) @property

;; Script blocks (outer chunk replaced by JS injection)
(script_element) @module

;; Style blocks (outer chunk replaced by CSS injection)
(style_element) @module

;; Template blocks
(template_element
  (start_tag (tag_name) @name)) @property
"#;

// No call query — JS/CSS calls are extracted via injection
// No type query — Vue templates don't have typed references

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "div", "span", "p", "a", "img", "ul", "ol", "li", "table", "tr", "td", "th", "form", "input",
    "button", "label", "select", "option", "textarea", "br", "hr", "head", "body", "html", "meta",
    "link", "title", "script", "style", "class", "id", "href", "src", "alt", "type", "value",
    "name", "slot", "template", "component", "transition", "keep", "alive", "teleport", "suspense",
    "v-if", "v-else", "v-for", "v-show", "v-bind", "v-on", "v-model", "v-slot", "v-html",
    "const", "let", "var", "export", "import", "default", "ref", "reactive", "computed", "watch",
    "defineProps", "defineEmits", "defineExpose", "withDefaults",
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

/// Post-process Vue element chunks.
///
/// Same logic as HTML/Svelte: headings→Section, script/style/template→Module,
/// landmarks→Section, noise→filter unless id, else Property.
fn post_process_vue(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let tag = name.to_lowercase();

    // Headings → Section with text content
    if HEADING_TAGS.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        let content = &source[node.byte_range()];
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

    // Script/style/template → Module
    if tag == "script" || tag == "style" || tag == "template" {
        *chunk_type = ChunkType::Module;
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
            // Vue setup attribute (boolean — no value)
            if super::html::has_attribute(st, "setup", source) {
                *name = "script:setup".to_string();
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

#[cfg(feature = "lang-vue")]
static DEFINITION: LanguageDef = LanguageDef {
    name: "vue",
    grammar: Some(|| tree_sitter_vue::LANGUAGE.into()),
    extensions: &["vue"],
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
    post_process_chunk: Some(post_process_vue),
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
    field_style: FieldStyle::None,
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
    /// Parses a Vue file containing a template and script block, verifying that JavaScript functions are correctly extracted via language injection.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser cannot be initialized, the Vue file cannot be parsed, or if the expected JavaScript functions 'handleClick' and 'formatName' are not found in the parsed chunks.

    #[test]
    fn parse_vue_with_script() {
        let content = r#"<template>
  <div>
    <h1>Hello World</h1>
    <button @click="handleClick">Click me</button>
  </div>
</template>

<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}

function formatName(first, last) {
    return `${first} ${last}`;
}
</script>
"#;
        let file = write_temp_file(content, "vue");
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
    /// Verifies that the parser correctly identifies and extracts TypeScript code blocks from Vue files with `lang="ts"` attributes.
    /// 
    /// This is a test function that validates the parser's ability to handle Vue single-file components containing TypeScript. It creates a temporary Vue file with a TypeScript script block containing a function and interface, parses it, and asserts that the TypeScript function is correctly identified and extracted.
    /// 
    /// # Arguments
    /// 
    /// No parameters.
    /// 
    /// # Returns
    /// 
    /// Returns nothing (unit type).
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the temporary file, or if the expected TypeScript function "greet" is not found in the parsed chunks.

    #[test]
    fn parse_vue_with_typescript() {
        let content = r#"<script lang="ts">
interface User {
    name: string;
    age: number;
}

function greet(user: User): string {
    return `Hello, ${user.name}!`;
}
</script>

<template>
  <p>Content</p>
</template>
"#;
        let file = write_temp_file(content, "vue");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

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
    /// Parses a Vue file containing both template and style blocks, verifying that CSS chunks are correctly extracted from the `<style>` block.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters. It creates its own test data internally by writing a temporary Vue file with template and style sections.
    /// 
    /// # Returns
    /// 
    /// Returns nothing (unit type). This is a test assertion function.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to parse the file, if no CSS language chunks are extracted from the `<style>` block, or if file operations fail.

    #[test]
    fn parse_vue_with_style() {
        let content = r#"<template>
  <div class="app">Hello</div>
</template>

<style>
.app {
    color: red;
    font-size: 16px;
}

.container {
    display: flex;
}
</style>
"#;
        let file = write_temp_file(content, "vue");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

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
    /// Parses a Vue single-file component with a `<script setup>` block and verifies that JavaScript functions are correctly extracted via injection.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded Vue content.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to verify correct parsing behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to extract the `increment` function from the `<script setup>` block, or if the expected JavaScript chunks are not found in the parsed output.

    #[test]
    fn parse_vue_setup_script() {
        let content = r#"<script setup>
function increment() {
    count.value++;
}
</script>

<template>
  <button @click="increment">Count</button>
</template>
"#;
        let file = write_temp_file(content, "vue");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // JS function should be extracted via injection from <script setup>
        // Note: the outer script_element chunk is replaced by injected JS chunks
        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_chunks.iter().any(|c| c.name == "increment"),
            "Expected JS function 'increment' from <script setup>, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
    }
    /// Tests that the parser correctly extracts headings and landmarks from Vue template files.
    /// 
    /// This test verifies that when parsing a Vue file containing an h1 heading and a nav element with an id attribute, the parser produces Section chunks with the expected names: "Welcome Page" for the heading and "nav#main-nav" for the landmark element.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the temporary Vue file, or if the extracted sections do not contain the expected heading "Welcome Page" and landmark "nav#main-nav".

    #[test]
    fn parse_vue_heading_extraction() {
        let content = r#"<template>
  <h1>Welcome Page</h1>
  <nav id="main-nav">
    <a href="/">Home</a>
  </nav>
</template>
"#;
        let file = write_temp_file(content, "vue");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let sections: Vec<_> = chunks
            .iter()
            .filter(|c| c.chunk_type == ChunkType::Section)
            .collect();

        assert!(
            sections.iter().any(|c| c.name == "Welcome Page"),
            "Expected heading 'Welcome Page', got: {:?}",
            sections
                .iter()
                .map(|c| &c.name)
                .collect::<Vec<_>>()
        );
        assert!(
            sections.iter().any(|c| c.name == "nav#main-nav"),
            "Expected landmark 'nav#main-nav'"
        );
    }
    /// Tests that parsing a Vue file containing only a template block produces no JavaScript chunks.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize, file parsing fails, or if JavaScript chunks are unexpectedly found in the parsed output.

    #[test]
    fn parse_vue_no_script() {
        let content = r#"<template>
  <div>Pure template, no script</div>
</template>
"#;
        let file = write_temp_file(content, "vue");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(js_chunks.is_empty(), "Expected no JS chunks without <script>");
    }
}
