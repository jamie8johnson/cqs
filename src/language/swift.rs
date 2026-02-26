//! Swift language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Swift code chunks.
///
/// Swift's tree-sitter grammar uses `class_declaration` for classes, structs,
/// enums, actors, AND extensions. The `post_process_chunk` hook reclassifies
/// based on keyword children and body type.
const CHUNK_QUERY: &str = r#"
;; Classes, structs, actors (all use class_declaration with direct type_identifier)
;; post_process_chunk reclassifies based on keyword
(class_declaration
  (type_identifier) @name) @class

;; Extensions — name comes from user_type, not direct type_identifier
(class_declaration
  (user_type
    (type_identifier) @name)) @class

;; Protocols
(protocol_declaration
  (type_identifier) @name) @trait

;; Functions (top-level and methods)
(function_declaration
  (simple_identifier) @name) @function

;; Protocol function declarations (signatures without body)
(protocol_function_declaration
  (simple_identifier) @name) @function

;; Typealias
(typealias_declaration
  (type_identifier) @name) @typealias
"#;

/// Tree-sitter query for extracting Swift function calls
const CALL_QUERY: &str = r#"
;; Direct function calls
(call_expression
  (simple_identifier) @callee)

;; Method calls via navigation
(call_expression
  (navigation_expression
    (navigation_suffix
      (simple_identifier) @callee)))
"#;

/// Tree-sitter query for extracting Swift type references
const TYPE_QUERY: &str = r#"
;; Parameter types
(parameter
  (user_type
    (type_identifier) @param_type))

;; Return types (after ->)
(function_declaration
  (user_type
    (type_identifier) @return_type))

;; Property types
(property_declaration
  (user_type
    (type_identifier) @field_type))

;; Protocol conformance / inheritance
(inheritance_specifier
  (user_type
    (type_identifier) @impl_type))

;; Catch-all
(user_type
  (type_identifier) @type_ref)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment", "multiline_comment"];

const STOPWORDS: &[&str] = &[
    "func", "var", "let", "class", "struct", "enum", "protocol", "extension", "actor", "import",
    "return", "if", "else", "guard", "switch", "case", "for", "while", "repeat", "break",
    "continue", "self", "super", "nil", "true", "false", "is", "as", "in", "try", "catch",
    "throw", "throws", "async", "await", "public", "private", "internal", "open", "fileprivate",
    "static", "final", "override", "mutating", "typealias", "where", "some", "any",
];

const COMMON_TYPES: &[&str] = &[
    "String", "Int", "Double", "Float", "Bool", "Character", "UInt", "Int8", "Int16", "Int32",
    "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Optional", "Array", "Dictionary", "Set",
    "Any", "AnyObject", "Void", "Never", "Error", "Codable", "Equatable", "Hashable", "Comparable",
    "Identifiable", "CustomStringConvertible",
];

fn extract_return(signature: &str) -> Option<String> {
    // Swift: func name(params) -> ReturnType {
    // Find "->" and extract the type between it and "{"
    let arrow_pos = signature.find("->")?;
    let after_arrow = &signature[arrow_pos + 2..];

    let end_pos = after_arrow
        .find('{')
        .unwrap_or(after_arrow.len());
    let ret_type = after_arrow[..end_pos].trim();

    if ret_type.is_empty() || ret_type == "Void" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

/// Post-process Swift chunks to reclassify `class_declaration` into the correct type.
///
/// Swift's tree-sitter grammar uses `class_declaration` for all structural types:
/// classes, structs, enums, actors, and extensions. We distinguish them by:
/// - `enum_class_body` child → Enum
/// - Anonymous "struct" keyword → Struct
/// - Anonymous "actor" keyword → Class (actor treated as class)
/// - Anonymous "extension" keyword → Class (extension treated as class)
/// - Anonymous "class" keyword or default → Class
fn post_process_swift(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    if node.kind() != "class_declaration" {
        return true;
    }

    let _span = tracing::debug_span!("post_process_swift", kind = node.kind()).entered();

    let mut cursor = node.walk();
    let mut has_enum_body = false;
    let mut keyword = "";

    for child in node.children(&mut cursor) {
        match child.kind() {
            "enum_class_body" => has_enum_body = true,
            _ if !child.is_named() => {
                let text = &source[child.byte_range()];
                match text {
                    "struct" => keyword = "struct",
                    "class" => keyword = "class",
                    "actor" => keyword = "actor",
                    "extension" => keyword = "extension",
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if has_enum_body {
        *chunk_type = ChunkType::Enum;
        tracing::debug!("Reclassified class_declaration as Enum (has enum_class_body)");
    } else {
        match keyword {
            "struct" => {
                *chunk_type = ChunkType::Struct;
                tracing::debug!("Reclassified class_declaration as Struct");
            }
            "actor" => {
                // Actor → Class (closest semantic match)
                tracing::debug!("Reclassified class_declaration as Class (actor)");
            }
            "extension" => {
                // Extension → Class (closest semantic match)
                tracing::debug!("Reclassified class_declaration as Class (extension)");
            }
            _ => {
                // "class" or unknown — default @class stays
            }
        }
    }

    true
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "swift",
    grammar: Some(|| tree_sitter_swift::LANGUAGE.into()),
    extensions: &["swift"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.swift")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["class_body", "protocol_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_swift),
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
    fn test_extract_return_swift() {
        assert_eq!(
            extract_return("func greet(name: String) -> String {"),
            Some("Returns string".to_string())
        );
        assert_eq!(extract_return("func doSomething() {"), None);
        assert_eq!(
            extract_return("func getCount() -> Int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(extract_return("func nothing() -> Void {"), None);
    }

    #[test]
    fn parse_swift_class() {
        let content = r#"
class Shape {
    var sides: Int = 0

    func describe() -> String {
        return "A shape with \(sides) sides"
    }
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Shape").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_swift_struct() {
        let content = r#"
struct Point {
    var x: Double
    var y: Double
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "Point").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_swift_enum() {
        let content = r#"
enum Direction {
    case north
    case south
    case east
    case west
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks.iter().find(|c| c.name == "Direction").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }

    #[test]
    fn parse_swift_protocol() {
        let content = r#"
protocol Drawable {
    func draw()
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let p = chunks.iter().find(|c| c.name == "Drawable").unwrap();
        assert_eq!(p.chunk_type, ChunkType::Trait);
    }

    #[test]
    fn parse_swift_function() {
        let content = r#"
func greet(name: String) -> String {
    return "Hello, \(name)!"
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_swift_actor() {
        let content = r#"
actor Counter {
    var count: Int = 0

    func increment() {
        count += 1
    }
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let a = chunks.iter().find(|c| c.name == "Counter").unwrap();
        assert_eq!(a.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_swift_extension() {
        let content = r#"
struct Point {
    var x: Double
    var y: Double
}

extension Point {
    func distance() -> Double {
        return (x * x + y * y).squareRoot()
    }
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Both the struct and the extension should have name "Point"
        let point_chunks: Vec<_> = chunks.iter().filter(|c| c.name == "Point").collect();
        assert!(
            point_chunks.len() >= 2,
            "Expected at least 2 Point chunks (struct + extension), got: {}",
            point_chunks.len()
        );
        // The struct should be Struct type
        assert!(
            point_chunks.iter().any(|c| c.chunk_type == ChunkType::Struct),
            "Expected one Point to be Struct"
        );
        // The extension should be Class type
        assert!(
            point_chunks.iter().any(|c| c.chunk_type == ChunkType::Class),
            "Expected one Point to be Class (extension)"
        );
    }

    #[test]
    fn parse_swift_typealias() {
        let content = "typealias StringMap = Dictionary<String, String>\n";
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_swift_calls() {
        let content = r#"
func process(input: String) -> Int {
    let trimmed = input.trimmingCharacters(in: .whitespaces)
    let result = transform(trimmed)
    print(result)
    return result.count
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"transform"),
            "Expected transform call, got: {:?}",
            names
        );
        assert!(
            names.contains(&"print"),
            "Expected print call, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_swift_method_in_class() {
        let content = r#"
class Calculator {
    func add(a: Int, b: Int) -> Int {
        return a + b
    }
}
"#;
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }
}
