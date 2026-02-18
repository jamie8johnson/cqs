//! Chunk extraction from tree-sitter parse trees

use std::path::Path;

use super::types::{Chunk, ChunkType, Language, ParserError, SignatureStyle};
use super::Parser;

impl Parser {
    pub(crate) fn extract_chunk(
        &self,
        source: &str,
        m: &tree_sitter::QueryMatch<'_, '_>,
        query: &tree_sitter::Query,
        language: Language,
        path: &Path,
    ) -> Result<Chunk, ParserError> {
        // Map capture names to chunk types
        let capture_types: &[(&str, ChunkType)] = &[
            ("function", ChunkType::Function),
            ("struct", ChunkType::Struct),
            ("class", ChunkType::Class),
            ("enum", ChunkType::Enum),
            ("trait", ChunkType::Trait),
            ("interface", ChunkType::Interface),
            ("const", ChunkType::Constant),
        ];

        // Find which definition capture matched and get its node
        let (node, base_chunk_type) = capture_types
            .iter()
            .find_map(|(name, chunk_type)| {
                query
                    .capture_index_for_name(name)
                    .and_then(|idx| m.captures.iter().find(|c| c.index == idx))
                    .map(|c| (c.node, *chunk_type))
            })
            .ok_or_else(|| {
                ParserError::ParseFailed("No definition capture found in match".into())
            })?;

        // Get name capture
        let name_idx = query.capture_index_for_name("name");
        let name_capture = name_idx.and_then(|idx| m.captures.iter().find(|c| c.index == idx));
        let mut name = name_capture
            .map(|c| {
                let raw = source[c.node.byte_range()].to_string();
                // Names should never span multiple lines — error recovery in grammars
                // (especially SQL) can extend nodes past the actual name.
                raw.lines().next().unwrap_or(&raw).trim().to_string()
            })
            .unwrap_or_else(|| "<anonymous>".to_string());

        // Extract content
        let content = source[node.byte_range()].to_string();

        // Validate name position: if the @name capture is far from the definition
        // start, tree-sitter error recovery likely matched the wrong node.
        // Fall back to extracting the name from the content text.
        if let Some(nc) = name_capture {
            let name_line = nc.node.start_position().row;
            let def_line = node.start_position().row;
            if name_line.saturating_sub(def_line) > 5 {
                if let Some(extracted) = extract_name_fallback(&content) {
                    name = extracted;
                }
            }
        }

        // Line numbers (1-indexed for display)
        let line_start = node.start_position().row as u32 + 1;
        let line_end = node.end_position().row as u32 + 1;

        // Extract signature
        let signature = extract_signature(&content, language);

        // Extract doc comments
        let doc = extract_doc_comment(node, source, language);

        // Determine chunk type - only infer for functions (to detect methods)
        let (chunk_type, parent_type_name) = if base_chunk_type == ChunkType::Function {
            infer_chunk_type(node, language, source)
        } else {
            (base_chunk_type, None)
        };

        if let Some(ref ptn) = parent_type_name {
            tracing::debug!(parent_type = %ptn, method = %name, "Extracted parent type for method");
        }

        // Content hash for deduplication (BLAKE3 produces 64 hex chars)
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let hash_prefix = content_hash.get(..8).unwrap_or(&content_hash);
        let id = format!("{}:{}:{}", path.display(), line_start, hash_prefix);

        Ok(Chunk {
            id,
            file: path.to_path_buf(),
            language,
            chunk_type,
            name,
            signature,
            content,
            doc,
            line_start,
            line_end,
            content_hash,
            parent_id: None,
            window_idx: None,
            parent_type_name,
        })
    }
}

pub(crate) fn extract_signature(content: &str, language: Language) -> String {
    let sig_end = match language.def().signature_style {
        SignatureStyle::UntilBrace => content.find('{').unwrap_or(content.len()),
        SignatureStyle::UntilColon => content.find(':').unwrap_or(content.len()),
        SignatureStyle::UntilAs => {
            // Case-insensitive search for AS as a standalone word
            let upper = content.to_uppercase();
            upper
                .find(" AS ")
                .or_else(|| upper.find("\nAS\n"))
                .or_else(|| upper.find("\nAS "))
                .or_else(|| upper.find(" AS\n"))
                .unwrap_or(content.len())
        }
        // Markdown builds its own signatures in the custom parser; this arm
        // satisfies exhaustiveness but is never reached via extract_chunk().
        SignatureStyle::Breadcrumb => content.len(),
    };
    let sig = &content[..sig_end];
    // Normalize whitespace
    sig.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_doc_comment(
    node: tree_sitter::Node,
    source: &str,
    language: Language,
) -> Option<String> {
    let doc_nodes = language.def().doc_nodes;

    // Walk backwards through siblings looking for comments
    let mut comments = Vec::new();
    let mut current = node.prev_sibling();

    while let Some(sibling) = current {
        let kind = sibling.kind();

        if doc_nodes.contains(&kind) {
            let text = &source[sibling.byte_range()];
            comments.push(text.to_string());
            current = sibling.prev_sibling();
        } else if kind.contains("comment") {
            // Keep looking past non-doc comments
            current = sibling.prev_sibling();
        } else {
            break;
        }
    }

    if comments.is_empty() {
        // For Python, also check for docstring as first statement in body
        if language == Language::Python {
            if let Some(body) = node.child_by_field_name("body") {
                if let Some(first) = body.named_child(0) {
                    if first.kind() == "expression_statement" {
                        if let Some(string) = first.named_child(0) {
                            if string.kind() == "string" {
                                return Some(source[string.byte_range()].to_string());
                            }
                        }
                    }
                }
            }
        }
        return None;
    }

    comments.reverse();
    Some(comments.join("\n"))
}

/// Extract a name from chunk content when tree-sitter's @name capture is wrong.
/// Looks for `PROCEDURE name`, `FUNCTION name`, `VIEW name`, or `TRIGGER name`
/// patterns in the first few lines.
fn extract_name_fallback(content: &str) -> Option<String> {
    let upper = content.to_uppercase();
    for keyword in &["PROCEDURE", "FUNCTION", "VIEW", "TRIGGER"] {
        if let Some(pos) = upper.find(keyword) {
            let after_keyword = pos + keyword.len();
            if after_keyword >= content.len() {
                continue;
            }
            let rest = content[after_keyword..].trim_start();
            // Name ends at whitespace, '(', '@', or newline
            let name_end = rest
                .find(|c: char| c.is_whitespace() || c == '(' || c == '@')
                .unwrap_or(rest.len());
            let name = rest[..name_end].trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn infer_chunk_type(
    node: tree_sitter::Node,
    language: Language,
    source: &str,
) -> (ChunkType, Option<String>) {
    let def = language.def();

    // Check if the node itself is a method kind (e.g., Go's "method_declaration")
    if def.method_node_kinds.contains(&node.kind()) {
        let parent_type = extract_method_receiver_type(node, language, source);
        return (ChunkType::Method, parent_type);
    }

    // Walk parents looking for method containers (e.g., impl blocks, class bodies)
    let mut current = node.parent();
    while let Some(parent) = current {
        if def.method_containers.contains(&parent.kind()) {
            let parent_type = extract_container_type_name(parent, language, source);
            return (ChunkType::Method, parent_type);
        }
        current = parent.parent();
    }

    (ChunkType::Function, None)
}

/// Extract type name from a method container node (impl block, class, trait).
fn extract_container_type_name(
    container: tree_sitter::Node,
    language: Language,
    source: &str,
) -> Option<String> {
    match language {
        Language::Rust => {
            if container.kind() == "impl_item" {
                // impl Foo { ... } or impl<T> Foo<T> { ... } or impl Trait for Foo { ... }
                // The "type" field gives us the target type (Foo), not the trait
                container.child_by_field_name("type").and_then(|t| {
                    if t.kind() == "type_identifier" {
                        Some(source[t.byte_range()].to_string())
                    } else {
                        // generic_type wraps type_identifier: Foo<T>
                        find_child_text_by_kind(t, "type_identifier", source)
                    }
                })
            } else {
                // trait_item: trait Drawable { ... }
                container
                    .child_by_field_name("name")
                    .map(|n| source[n.byte_range()].to_string())
            }
        }
        Language::Python => {
            // class_definition → name field
            container
                .child_by_field_name("name")
                .map(|n| source[n.byte_range()].to_string())
        }
        Language::JavaScript | Language::TypeScript | Language::Java => {
            // method_containers include "class_body" and "class_declaration"
            // If matched on class_body, walk up to class_declaration for the name
            let class_node = if container.kind() == "class_body" {
                container.parent()
            } else {
                Some(container)
            };
            class_node.and_then(|cn| {
                cn.child_by_field_name("name")
                    .map(|n| source[n.byte_range()].to_string())
            })
        }
        _ => None, // C, SQL, Markdown — no method containers
    }
}

/// Extract receiver type from a Go method_declaration.
///
/// Go methods: `func (r *Server) Handle()` → "Server"
fn extract_method_receiver_type(
    node: tree_sitter::Node,
    language: Language,
    source: &str,
) -> Option<String> {
    if language != Language::Go {
        return None;
    }
    // method_declaration → receiver (parameter_list) → parameter_declaration → type
    let receiver = node.child_by_field_name("receiver")?;
    let first_param = receiver.named_child(0)?;
    // type_identifier may be nested in pointer_type
    find_type_identifier_recursive(first_param, source)
}

/// Find first direct child with given kind and return its text.
fn find_child_text_by_kind(node: tree_sitter::Node, kind: &str, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return Some(source[child.byte_range()].to_string());
        }
    }
    None
}

/// Recursively find a type_identifier node and return its text.
/// Used for Go where the type may be wrapped in pointer_type.
fn find_type_identifier_recursive(node: tree_sitter::Node, source: &str) -> Option<String> {
    if node.kind() == "type_identifier" {
        return Some(source[node.byte_range()].to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(name) = find_type_identifier_recursive(child, source) {
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    mod signature_tests {
        use super::*;

        #[test]
        fn test_rust_signature_stops_at_brace() {
            let content = "fn process(x: i32) -> Result<(), Error> {\n    body\n}";
            let sig = extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn process(x: i32) -> Result<(), Error>");
        }

        #[test]
        fn test_rust_signature_normalizes_whitespace() {
            let content = "fn   process(  x: i32  )   -> i32 {";
            let sig = extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn process( x: i32 ) -> i32");
        }

        #[test]
        fn test_python_signature_stops_at_colon() {
            let content = "def calculate(x, y):\n    return x + y";
            let sig = extract_signature(content, Language::Python);
            assert_eq!(sig, "def calculate(x, y)");
        }

        #[test]
        fn test_go_signature_stops_at_brace() {
            let content = "func (s *Server) Handle(r Request) error {\n\treturn nil\n}";
            let sig = extract_signature(content, Language::Go);
            assert_eq!(sig, "func (s *Server) Handle(r Request) error");
        }

        #[test]
        fn test_typescript_signature_stops_at_brace() {
            let content = "function processData(input: string): Promise<Result> {\n  return ok;\n}";
            let sig = extract_signature(content, Language::TypeScript);
            assert_eq!(sig, "function processData(input: string): Promise<Result>");
        }

        #[test]
        fn test_signature_without_terminator() {
            let content = "fn abstract_decl()";
            let sig = extract_signature(content, Language::Rust);
            assert_eq!(sig, "fn abstract_decl()");
        }
    }

    fn write_temp_file(content: &str, ext: &str) -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    mod parse_tests {
        use super::*;

        #[test]
        fn test_parse_rust_function() {
            let content = r#"
/// Adds two numbers
fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "add");
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0].doc.as_ref().unwrap().contains("Adds two numbers"));
        }

        #[test]
        fn test_parse_rust_method_in_impl() {
            let content = r#"
struct Counter { value: i32 }

impl Counter {
    fn increment(&mut self) {
        self.value += 1;
    }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let method = chunks.iter().find(|c| c.name == "increment").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_python_class_method() {
            let content = r#"
class Calculator:
    """A simple calculator."""

    def add(self, a, b):
        """Add two numbers."""
        return a + b
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_go_method_vs_function() {
            let content = r#"
package main

func standalone() {
    println("standalone")
}

func (s *Server) method() {
    println("method")
}
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let standalone = chunks.iter().find(|c| c.name == "standalone").unwrap();
            assert_eq!(standalone.chunk_type, ChunkType::Function);

            let method = chunks.iter().find(|c| c.name == "method").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
        }

        #[test]
        fn test_parse_typescript_interface() {
            let content = r#"
interface User {
    name: string;
    age: number;
}
"#;
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "User");
            assert_eq!(chunks[0].chunk_type, ChunkType::Interface);
        }

        #[test]
        fn test_parse_c_function() {
            let content = r#"
/* Adds two integers */
int add(int a, int b) {
    return a + b;
}
"#;
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            assert_eq!(chunks.len(), 1);
            assert_eq!(chunks[0].name, "add");
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0]
                .doc
                .as_ref()
                .unwrap()
                .contains("Adds two integers"));
        }

        #[test]
        fn test_parse_c_struct_and_enum() {
            let content = r#"
struct Point {
    int x;
    int y;
};

enum Color {
    RED,
    GREEN,
    BLUE
};
"#;
            let file = write_temp_file(content, "c");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let point = chunks.iter().find(|c| c.name == "Point").unwrap();
            assert_eq!(point.chunk_type, ChunkType::Struct);

            let color = chunks.iter().find(|c| c.name == "Color").unwrap();
            assert_eq!(color.chunk_type, ChunkType::Enum);
        }

        #[test]
        fn test_parse_java_class_with_method() {
            let content = r#"
public class Calculator {
    /**
     * Adds two numbers
     */
    public int add(int a, int b) {
        return a + b;
    }
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
            assert_eq!(class.chunk_type, ChunkType::Class);

            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert!(method.doc.as_ref().unwrap().contains("Adds two numbers"));
        }

        #[test]
        fn test_parse_java_interface_and_enum() {
            let content = r#"
interface Printable {
    void print();
}

enum Direction {
    NORTH,
    SOUTH,
    EAST,
    WEST
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();

            let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
            assert_eq!(iface.chunk_type, ChunkType::Interface);

            let dir = chunks.iter().find(|c| c.name == "Direction").unwrap();
            assert_eq!(dir.chunk_type, ChunkType::Enum);
        }
    }

    mod parent_type_tests {
        use super::*;

        #[test]
        fn test_rust_method_has_parent_type_name() {
            let content = r#"
struct Counter { value: i32 }
impl Counter {
    fn increment(&mut self) { self.value += 1; }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "increment").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Counter"));
        }

        #[test]
        fn test_rust_trait_method_has_parent_type_name() {
            let content = r#"
trait Drawable {
    fn draw(&self) {}
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "draw").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Drawable"));
        }

        #[test]
        fn test_rust_impl_trait_for_type() {
            let content = r#"
struct Foo;
trait Display { fn fmt(&self) -> String; }
impl Display for Foo {
    fn fmt(&self) -> String { String::new() }
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks
                .iter()
                .find(|c| c.name == "fmt" && c.chunk_type == ChunkType::Method)
                .unwrap();
            // Should extract the target type (Foo), not the trait (Display)
            assert_eq!(method.parent_type_name.as_deref(), Some("Foo"));
        }

        #[test]
        fn test_rust_generic_impl() {
            let content = r#"
struct Container<T> { items: Vec<T> }
impl<T> Container<T> {
    fn push(&mut self, item: T) {}
}
"#;
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "push").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            // Should extract the base type name, not the full generic
            assert_eq!(method.parent_type_name.as_deref(), Some("Container"));
        }

        #[test]
        fn test_python_method_has_parent_type_name() {
            let content = r#"
class Calculator:
    def add(self, a, b):
        return a + b
"#;
            let file = write_temp_file(content, "py");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
        }

        #[test]
        fn test_go_method_pointer_receiver() {
            let content = r#"
package main
type Server struct{}
func (s *Server) Handle() {}
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "Handle").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Server"));
        }

        #[test]
        fn test_go_method_value_receiver() {
            let content = r#"
package main
type Point struct{ x, y int }
func (p Point) Distance() float64 { return 0.0 }
"#;
            let file = write_temp_file(content, "go");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "Distance").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Point"));
        }

        #[test]
        fn test_js_method_has_parent_type_name() {
            let content = r#"
class Cache {
    get(key) { return this.data[key]; }
}
"#;
            let file = write_temp_file(content, "js");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "get").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Cache"));
        }

        #[test]
        fn test_ts_method_has_parent_type_name() {
            let content = r#"
class TypedCache {
    get(key: string): string { return ""; }
}
"#;
            let file = write_temp_file(content, "ts");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "get").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("TypedCache"));
        }

        #[test]
        fn test_java_method_has_parent_type_name() {
            let content = r#"
public class Calculator {
    public int add(int a, int b) { return a + b; }
}
"#;
            let file = write_temp_file(content, "java");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            let method = chunks.iter().find(|c| c.name == "add").unwrap();
            assert_eq!(method.chunk_type, ChunkType::Method);
            assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
        }

        #[test]
        fn test_standalone_function_no_parent() {
            let content = "fn standalone() {}";
            let file = write_temp_file(content, "rs");
            let parser = Parser::new().unwrap();
            let chunks = parser.parse_file(file.path()).unwrap();
            assert_eq!(chunks[0].chunk_type, ChunkType::Function);
            assert!(chunks[0].parent_type_name.is_none());
        }
    }
}
