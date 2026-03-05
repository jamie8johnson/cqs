//! Gleam language definition

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Gleam code chunks.
///
/// Gleam constructs:
///   - `function` → Function (has named `name` field)
///   - `type_definition` → Enum (custom types with constructors)
///   - `type_alias` → TypeAlias
///   - `constant` → Constant (has named `name` field)
const CHUNK_QUERY: &str = r#"
;; Function definition: pub fn add(x: Int, y: Int) -> Int { ... }
(function
  name: (identifier) @name) @function

;; Custom type definition: pub type Color { Red Green Blue }
(type_definition
  (type_name
    name: (type_identifier) @name)) @struct

;; Type alias: pub type UserId = Int
(type_alias
  (type_name
    name: (type_identifier) @name)) @struct

;; Constant: pub const max_retries: Int = 3
(constant
  name: (identifier) @name) @const
"#;

/// Tree-sitter query for extracting Gleam calls.
///
/// Gleam uses `function_call` with named `function` and `arguments` fields:
///   - Direct: `add(x, y)` → (function_call function: (identifier))
///   - Qualified: `io.println(msg)` → (function_call function: (field_access field: (label)))
const CALL_QUERY: &str = r#"
;; Direct function call: foo(args)
(function_call
  function: (identifier) @callee)

;; Qualified/module call: module.func(args)
(function_call
  function: (field_access
    field: (label) @callee))
"#;

/// Doc comment node types — Gleam uses `///` doc comments
const DOC_NODES: &[&str] = &["module_comment", "statement_comment", "comment"];

const STOPWORDS: &[&str] = &[
    "fn", "pub", "let", "assert", "case", "if", "else", "use", "import", "type", "const",
    "opaque", "external", "todo", "panic", "as", "try", "Ok", "Error", "True", "False", "Nil",
    "Int", "Float", "String", "Bool", "List", "Result", "Option", "BitArray", "Dict",
    "io", "int", "float", "string", "list", "result", "option", "dict", "map",
];

/// Post-process Gleam chunks to set correct chunk types.
fn post_process_gleam(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "function" => *chunk_type = ChunkType::Function,
        "type_definition" => *chunk_type = ChunkType::Enum,
        "type_alias" => *chunk_type = ChunkType::TypeAlias,
        "constant" => *chunk_type = ChunkType::Constant,
        _ => {}
    }
    true
}

/// Extract return type from Gleam function signatures.
///
/// Gleam signatures: `fn add(x: Int, y: Int) -> Int {`
/// Return type is after `->`.
fn extract_return(signature: &str) -> Option<String> {
    let trimmed = signature.trim();

    // fn name(params) -> ReturnType {
    let arrow = trimmed.find("->")?;
    let after = trimmed[arrow + 2..].trim();

    // Remove opening brace
    let ret = after.split('{').next()?.trim();

    if ret.is_empty() {
        return None;
    }

    // Skip Nil (void equivalent)
    if ret == "Nil" {
        return None;
    }

    let words = crate::nl::tokenize_identifier(ret).join(" ");
    Some(format!("Returns {}", words.to_lowercase()))
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "gleam",
    grammar: Some(|| tree_sitter_gleam::LANGUAGE.into()),
    extensions: &["gleam"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_test.gleam")),
    type_query: None,
    common_types: &[
        "Int", "Float", "String", "Bool", "List", "Result", "Option", "Nil", "BitArray", "Dict",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_gleam as PostProcessChunkFn),
    test_markers: &[],
    test_path_patterns: &["%/test/%", "%_test.gleam"],
    structural_matchers: None,
    entry_point_names: &["main"],
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
    fn parse_gleam_function() {
        let content = r#"
pub fn add(x: Int, y: Int) -> Int {
  x + y
}
"#;
        let file = write_temp_file(content, "gleam");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_gleam_type() {
        let content = r#"
pub type Color {
  Red
  Green
  Blue
}
"#;
        let file = write_temp_file(content, "gleam");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let dt = chunks
            .iter()
            .find(|c| c.name == "Color" && c.chunk_type == ChunkType::Enum);
        assert!(dt.is_some(), "Should find 'Color' type as Enum");
    }

    #[test]
    fn parse_gleam_type_alias() {
        let content = r#"
pub type UserId = Int
"#;
        let file = write_temp_file(content, "gleam");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks
            .iter()
            .find(|c| c.name == "UserId" && c.chunk_type == ChunkType::TypeAlias);
        assert!(ta.is_some(), "Should find 'UserId' type alias");
    }

    #[test]
    fn parse_gleam_constant() {
        let content = r#"
pub const max_retries: Int = 3
"#;
        let file = write_temp_file(content, "gleam");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let c = chunks
            .iter()
            .find(|c| c.name == "max_retries" && c.chunk_type == ChunkType::Constant);
        assert!(c.is_some(), "Should find 'max_retries' constant");
    }

    #[test]
    fn parse_gleam_calls() {
        let content = r#"
import gleam/io

pub fn main() {
  let result = add(1, 2)
  io.println("done")
}

fn add(x: Int, y: Int) -> Int {
  x + y
}
"#;
        let file = write_temp_file(content, "gleam");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "main").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"add"),
            "Expected add, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_return_gleam() {
        assert_eq!(
            extract_return("pub fn add(x: Int, y: Int) -> Int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            extract_return("pub fn greet(name: String) -> String {"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            extract_return("pub fn main() -> Nil {"),
            None
        );
        assert_eq!(extract_return("fn do_something() {"), None);
        assert_eq!(extract_return(""), None);
    }
}
