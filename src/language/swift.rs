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

/// Extracts the return type from a Swift function signature and formats it as documentation text.
/// 
/// Parses a Swift function signature to find the return type annotation (the part after `->` and before `{`), then formats it as a documentation string. Void and empty return types are treated as no return value.
/// 
/// # Arguments
/// 
/// * `signature` - A Swift function signature string containing a `->` return type annotation
/// 
/// # Returns
/// 
/// Returns `Some(String)` containing formatted return documentation if a non-void return type is found, or `None` if the signature has no `->` marker, an empty return type, or a `Void` return type.
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
/// - Anonymous "extension" keyword → Extension
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
                *chunk_type = ChunkType::Extension;
                tracing::debug!("Reclassified class_declaration as Extension");
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
    test_markers: &["func test"],
    test_path_patterns: &["%/Tests/%", "%Tests.swift"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "hash", "encode", "init", "deinit", "description",
    ],
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
    /// Parses a Swift class definition from a temporary file and verifies the parser correctly identifies it as a Class chunk type.
    /// 
    /// This is an integration test that creates a temporary Swift file containing a Shape class with a property and method, then uses the Parser to extract code chunks and validates that the Shape class is correctly identified with the Class chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This function is a self-contained test with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function is a test that asserts parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to parse the file, the Shape class is not found in the parsed chunks, or the chunk type is not Class.

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
    /// Parses a Swift struct definition and verifies the parser correctly identifies it as a Struct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to parse the file
    /// - A chunk named "Point" is not found in the parsed chunks
    /// - The Point chunk type is not identified as `ChunkType::Struct`

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
    /// Tests that the parser correctly identifies and classifies Swift enum declarations.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded Swift source code.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts conditions and panics if any assertion fails.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - Writing the temporary file fails
    /// - Parser initialization fails
    /// - Parsing the file fails
    /// - The "Direction" enum chunk is not found in the parsed results
    /// - The parsed chunk type is not `ChunkType::Enum`

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
    /// Parses a Swift protocol definition and verifies it is correctly identified as a Trait chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on a hardcoded Swift protocol definition.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parsing behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - A chunk named "Drawable" is not found in the parsed output
    /// - The parsed chunk type does not equal `ChunkType::Trait`

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
    /// Parses a Swift function definition and verifies its metadata.
    /// 
    /// This test function creates a temporary Swift file containing a function definition, parses it using the Parser, and asserts that the resulting chunk is correctly identified as a Function type with the expected name.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded Swift source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize or parse the file, the "greet" function chunk is not found in the parsed results, or the chunk type is not ChunkType::Function.

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
    /// Parses a Swift actor declaration and verifies it is correctly identified as a class chunk type.
    /// 
    /// This test function creates a temporary Swift file containing an actor definition with a property and method, parses it using the Parser, and asserts that the resulting chunk for the actor is correctly classified as a ChunkType::Class.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - Parsing the Swift file fails
    /// - No chunk named "Counter" is found in the parsed results
    /// - The Counter chunk is not of type ChunkType::Class

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
    /// Verifies that the parser correctly identifies and separates Swift struct definitions from their extensions.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded Swift source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The parser fails to parse the temporary Swift file
    /// - Fewer than 2 chunks named "Point" are found (struct + extension)
    /// - No chunk with type `ChunkType::Struct` is found
    /// - No chunk with type `ChunkType::Class` (representing the extension) is found

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
        // The extension should be Extension type
        assert!(
            point_chunks.iter().any(|c| c.chunk_type == ChunkType::Extension),
            "Expected one Point to be Extension"
        );
    }
    /// Parses a Swift typealias declaration and verifies it is correctly identified as a TypeAlias chunk.
    /// 
    /// This test function creates a temporary Swift file containing a typealias definition, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns nothing.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file creation fails, parsing fails, the "StringMap" chunk is not found, or the chunk type assertion fails.

    #[test]
    fn parse_swift_typealias() {
        let content = "typealias StringMap = Dictionary<String, String>\n";
        let file = write_temp_file(content, "swift");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }
    /// Parses a Swift source file and verifies that function calls within a function are correctly extracted.
    /// 
    /// This test function creates a temporary Swift file containing a `process` function, parses it using the `Parser`, locates the `process` function chunk, and extracts all function calls from it. It then asserts that the expected function calls (`transform` and `print`) are present in the extracted calls.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded Swift source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file creation fails, parsing fails, the `process` function is not found in the parsed chunks, or if the expected function calls (`transform` or `print`) are not found in the extracted calls.

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
    /// Parses a Swift method defined within a class and verifies correct extraction of method metadata.
    /// 
    /// # Arguments
    /// 
    /// This function takes no arguments.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. This is a test function that asserts on parsed results.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - The "add" method chunk is not found in the parsed results
    /// - The parsed method's chunk type is not `ChunkType::Method`
    /// - The parsed method's parent type name is not "Calculator"

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
