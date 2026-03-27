//! Kotlin language definition

use super::{ChunkType, FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Kotlin code chunks.
///
/// The kotlin-ng grammar uses `class_declaration` for both classes and interfaces,
/// distinguished by an anonymous keyword child ("class" vs "interface").
/// Enum classes have a `class_modifier` with text "enum" inside `modifiers`.
/// The `post_process_chunk` hook reclassifies these after extraction.
const CHUNK_QUERY: &str = r#"
;; Classes (regular, data, sealed, abstract) and interfaces
;; post_process_chunk reclassifies interfaces and enum classes
(class_declaration
  (identifier) @name) @class

;; Object declarations (singletons)
(object_declaration
  (identifier) @name) @object

;; Functions
(function_declaration
  (identifier) @name) @function

;; Secondary constructors — post_process_chunk reclassifies to Constructor
(secondary_constructor) @function

;; Init blocks — post_process_chunk reclassifies to Constructor
(anonymous_initializer) @function

;; Property declarations (val/var)
(property_declaration
  (variable_declaration
    (identifier) @name)) @property

;; Type aliases
(type_alias
  (identifier) @name) @typealias
"#;

/// Tree-sitter query for extracting Kotlin function calls
const CALL_QUERY: &str = r#"
;; Direct function calls
(call_expression
  (identifier) @callee)

;; Method calls (object.method())
(call_expression
  (navigation_expression
    (identifier) @callee))
"#;

/// Tree-sitter query for extracting Kotlin type references
const TYPE_QUERY: &str = r#"
;; Parameter types
(parameter
  (user_type (identifier) @param_type))

;; Return types
(function_declaration
  (user_type (identifier) @return_type))

;; Property types
(property_declaration
  (user_type (identifier) @field_type))

;; Superclass / interface implementations
(delegation_specifier
  (user_type (identifier) @impl_type))

;; Type alias right-hand side
(type_alias
  (user_type (identifier) @alias_type))

;; Generic type arguments
(type_arguments
  (type_projection
    (user_type (identifier) @type_ref)))

;; Catch-all
(user_type (identifier) @type_ref)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["line_comment", "multiline_comment"];

const STOPWORDS: &[&str] = &[
    "fun", "val", "var", "class", "interface", "object", "companion", "data", "sealed", "enum",
    "abstract", "open", "override", "private", "protected", "public", "internal", "return", "if",
    "else", "when", "for", "while", "do", "break", "continue", "this", "super", "import",
    "package", "is", "as", "in", "null", "true", "false", "typealias", "const", "lateinit",
    "suspend", "inline", "reified",
];

const COMMON_TYPES: &[&str] = &[
    "String", "Int", "Long", "Double", "Float", "Boolean", "Byte", "Short", "Char", "Unit",
    "Nothing", "Any", "List", "ArrayList", "Map", "HashMap", "Set", "HashSet", "Collection",
    "MutableList", "MutableMap", "MutableSet", "Sequence", "Array", "Pair", "Triple", "Comparable",
    "Iterable",
];

/// Post-process Kotlin chunks to reclassify `class_declaration` nodes.
///
/// The kotlin-ng grammar uses `class_declaration` for both classes and interfaces.
/// This hook checks:
/// 1. If an anonymous "interface" keyword child exists -> Interface
/// 2. If `modifiers` contains a `class_modifier` with text "enum" -> Enum
fn post_process_kotlin(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    // Reclassify secondary_constructor and anonymous_initializer (init blocks)
    match node.kind() {
        "secondary_constructor" => {
            *chunk_type = ChunkType::Constructor;
            *name = "constructor".to_string();
            return true;
        }
        "anonymous_initializer" => {
            *chunk_type = ChunkType::Constructor;
            *name = "init".to_string();
            return true;
        }
        _ => {}
    }

    // Only reclassify class_declarations below
    if node.kind() != "class_declaration" {
        return true;
    }

    let mut has_enum_modifier = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "modifiers" => {
                let mut mod_cursor = child.walk();
                for modifier in child.children(&mut mod_cursor) {
                    if modifier.kind() == "class_modifier" {
                        let text = &source[modifier.byte_range()];
                        if text == "enum" {
                            has_enum_modifier = true;
                        }
                    }
                }
            }
            "interface" => {
                *chunk_type = ChunkType::Interface;
                return true;
            }
            _ => {}
        }
    }

    if has_enum_modifier {
        *chunk_type = ChunkType::Enum;
    }
    // else: stays as Class
    true
}

/// Extracts the return type from a Kotlin function signature and formats it as a documentation string.
/// 
/// # Arguments
/// 
/// * `signature` - A Kotlin function signature string to parse for return type information
/// 
/// # Returns
/// 
/// Returns `Some(String)` containing a formatted return type description (e.g., "Returns SomeType") if a non-Unit return type is found after the closing parenthesis and colon. Returns `None` if no closing parenthesis exists, no colon is present, the return type is empty, or the return type is "Unit".
fn extract_return(signature: &str) -> Option<String> {
    // Kotlin: fun name(params): ReturnType { ... }
    // Look for `: ReturnType` after last `)` and before `{` or `=`
    let paren_pos = signature.rfind(')')?;
    let after_paren = &signature[paren_pos + 1..];

    // Find the terminator ({ or =)
    let end_pos = after_paren
        .find('{')
        .or_else(|| after_paren.find('='))
        .unwrap_or(after_paren.len());
    let between = &after_paren[..end_pos];

    // Look for colon
    let colon_pos = between.find(':')?;
    let ret_type = between[colon_pos + 1..].trim();
    if ret_type.is_empty() || ret_type == "Unit" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "kotlin",
    grammar: Some(|| tree_sitter_kotlin::LANGUAGE.into()),
    extensions: &["kt", "kts"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.kt")),
    test_name_suggestion: Some(|name| super::pascal_test_name("test", name)),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["class_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_kotlin),
    test_markers: &["@Test", "@ParameterizedTest"],
    test_path_patterns: &["%/test/%", "%/tests/%", "%Test.kt"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "equals", "hashCode", "toString", "compareTo", "iterator",
    ],
    injections: &[],
    doc_format: "javadoc",
    doc_convention: "Use KDoc format: @param, @return, @throws tags.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "val var private protected public internal override lateinit",
    },
    skip_line_prefixes: &["class ", "data class", "sealed class", "enum class", "interface "],
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
    fn test_extract_return_kotlin() {
        assert_eq!(
            extract_return("fun add(a: Int, b: Int): Int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(extract_return("fun doSomething(): Unit {"), None);
        assert_eq!(extract_return("fun doSomething() {"), None);
        assert_eq!(
            extract_return("fun getName(): String ="),
            Some("Returns string".to_string())
        );
    }
    /// Parses a Kotlin data class definition and verifies the parser correctly identifies it as a class chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts the parse results and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to parse the file, the "Person" class chunk is not found in the results, or the chunk type is not `ChunkType::Class`.

    #[test]
    fn parse_kotlin_data_class() {
        let content = r#"
data class Person(val name: String, val age: Int)
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Person").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }
    /// Parses a Kotlin interface definition and verifies it is correctly identified as an interface chunk.
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
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "Printable" interface is not found in parsed chunks, or the chunk type assertion fails.

    #[test]
    fn parse_kotlin_interface() {
        let content = r#"
interface Printable {
    fun print()
    fun prettyPrint()
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
        assert_eq!(iface.chunk_type, ChunkType::Interface);
    }
    /// Parses a Kotlin enum class definition and verifies it is correctly identified as an Enum chunk type.
    /// 
    /// # Arguments
    /// 
    /// No parameters.
    /// 
    /// # Returns
    /// 
    /// No return value. This is a test function that asserts the parser correctly identifies a Kotlin enum class named "Color" with ChunkType::Enum.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "Color" enum is not found in parsed chunks, or the chunk type assertion fails.

    #[test]
    fn parse_kotlin_enum_class() {
        let content = r#"
enum class Color {
    RED, GREEN, BLUE
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks.iter().find(|c| c.name == "Color").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }
    /// Parses a Kotlin object declaration and verifies it is correctly identified as an Object chunk type.
    /// 
    /// This is a test function that creates a temporary Kotlin file containing a singleton object definition, parses it using the Parser, and asserts that the resulting chunk has the name "Singleton" and chunk type of Object.
    /// 
    /// # Arguments
    /// 
    /// None. This is a standalone test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function returns unit type `()` and is intended to be run as a test.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following assertions fail:
    /// - Creating the temporary file fails
    /// - Parsing the file fails
    /// - No chunk named "Singleton" is found in the parsed results
    /// - The chunk type is not `ChunkType::Object`

    #[test]
    fn parse_kotlin_object() {
        let content = r#"
object Singleton {
    fun greet(): String = "hello"
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let obj = chunks.iter().find(|c| c.name == "Singleton").unwrap();
        assert_eq!(obj.chunk_type, ChunkType::Object);
    }
    /// Tests that the parser correctly identifies and classifies a Kotlin function definition.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded Kotlin source code.
    /// 
    /// # Returns
    /// 
    /// None. Returns `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - Creating a temporary file fails
    /// - Initializing the parser fails
    /// - Parsing the file fails
    /// - No function chunk named "add" is found in the parsed results
    /// - The parsed chunk is not classified as a `ChunkType::Function`

    #[test]
    fn parse_kotlin_function() {
        let content = r#"
fun add(a: Int, b: Int): Int {
    return a + b
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Verifies that the parser correctly identifies and classifies Kotlin type alias declarations.
    /// 
    /// This test function creates a temporary Kotlin file containing a type alias definition, parses it using the Parser, and validates that the resulting chunk has the correct name ("StringMap") and type (TypeAlias).
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded test data.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that uses assertions to verify parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following conditions are not met:
    /// - Failed to create a temporary file
    /// - Failed to create a Parser instance
    /// - Failed to parse the file
    /// - No chunk with name "StringMap" is found in the parsed results
    /// - The found chunk's type is not ChunkType::TypeAlias

    #[test]
    fn parse_kotlin_typealias() {
        let content = "typealias StringMap = Map<String, String>\n";
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }
    /// Parses a Kotlin file and verifies that function calls are correctly extracted from code chunks.
    /// 
    /// This test function writes a temporary Kotlin file containing a `process` function, parses it to extract code chunks, and validates that the parser correctly identifies all function calls (specifically `parseInt` and `println`) made within the function.
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser initialization fails, file parsing fails, the `process` function chunk is not found, or if the expected function calls (`parseInt` or `println`) are not found in the extracted calls list.

    #[test]
    fn parse_kotlin_calls() {
        let content = r#"
fun process(input: String): Int {
    val trimmed = input.trim()
    val result = parseInt(trimmed)
    println(result)
    return result
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"parseInt"),
            "Expected parseInt call, got: {:?}",
            names
        );
        assert!(
            names.contains(&"println"),
            "Expected println call, got: {:?}",
            names
        );
    }
    /// Verifies that the parser correctly identifies and classifies Kotlin property declarations as Property chunks.
    /// 
    /// This is a test function that validates the parser's ability to recognize both immutable (`val`) and mutable (`var`) Kotlin properties, extract their names, and assign them the correct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions that will panic on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a temporary file, fails to parse the file, fails to find expected property chunks by name, or if the parsed chunks do not have the `ChunkType::Property` type.

    #[test]
    fn parse_kotlin_property() {
        let content = r#"
val greeting: String = "hello"
var counter: Int = 0
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let val_chunk = chunks.iter().find(|c| c.name == "greeting").unwrap();
        assert_eq!(val_chunk.chunk_type, ChunkType::Property);
        let var_chunk = chunks.iter().find(|c| c.name == "counter").unwrap();
        assert_eq!(var_chunk.chunk_type, ChunkType::Property);
    }
    /// Parses a Kotlin method defined within a class and verifies correct identification.
    /// 
    /// # Arguments
    /// 
    /// This function takes no parameters.
    /// 
    /// # Returns
    /// 
    /// Nothing. This is a test function that verifies parsing behavior through assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following occur:
    /// - Temporary file creation fails
    /// - Parser initialization fails
    /// - File parsing fails
    /// - The "add" method chunk is not found in parsed results
    /// - The parsed method's chunk type is not `Method`
    /// - The parsed method's parent type name is not "Calculator"

    #[test]
    fn parse_kotlin_method_in_class() {
        let content = r#"
class Calculator {
    fun add(a: Int, b: Int): Int {
        return a + b
    }
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }
    /// Verifies that the parser correctly identifies and classifies a Kotlin sealed class definition.
    /// 
    /// # Arguments
    /// 
    /// This function takes no arguments. It creates a temporary Kotlin file containing a sealed class with nested data class variants (Success and Error), then parses the file using the Parser.
    /// 
    /// # Returns
    /// 
    /// This function returns nothing (unit type). It performs assertions to verify parsing behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - A chunk named "Result" is not found in the parsed output
    /// - The parsed chunk's type is not classified as `ChunkType::Class`

    #[test]
    fn parse_kotlin_sealed_class() {
        let content = r#"
sealed class Result {
    data class Success(val data: String) : Result()
    data class Error(val message: String) : Result()
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let sealed = chunks.iter().find(|c| c.name == "Result").unwrap();
        assert_eq!(sealed.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_kotlin_secondary_constructor() {
        let content = r#"
class MyClass(val name: String) {
    constructor(x: Int) : this(x.toString())
    fun greet() { println("hi") }
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ctor = chunks
            .iter()
            .find(|c| c.name == "constructor" && c.chunk_type == ChunkType::Constructor);
        assert!(
            ctor.is_some(),
            "Expected secondary constructor as Constructor, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, c.chunk_type))
                .collect::<Vec<_>>()
        );
        // greet should still be a Method
        let method = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }

    #[test]
    fn parse_kotlin_init_block() {
        let content = r#"
class Config(val path: String) {
    init {
        println("loading config")
    }
    fun load() { }
}
"#;
        let file = write_temp_file(content, "kt");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let init = chunks
            .iter()
            .find(|c| c.name == "init" && c.chunk_type == ChunkType::Constructor);
        assert!(
            init.is_some(),
            "Expected init block as Constructor, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, c.chunk_type))
                .collect::<Vec<_>>()
        );
    }
}
