//! Java language definition

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Java code chunks
const CHUNK_QUERY: &str = r#"
(method_declaration
  name: (identifier) @name) @function

(constructor_declaration
  name: (identifier) @name) @function

(class_declaration
  name: (identifier) @name) @class

(interface_declaration
  name: (identifier) @name) @interface

(enum_declaration
  name: (identifier) @name) @enum

(record_declaration
  name: (identifier) @name) @struct

;; Annotation types (@interface)
(annotation_type_declaration
  name: (identifier) @name) @interface

;; Fields (class-level only — local vars are local_variable_declaration)
(field_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @property
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(method_invocation
  name: (identifier) @callee)

(object_creation_expression
  type: (type_identifier) @callee)
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(formal_parameter type: (type_identifier) @param_type)
(formal_parameter type: (generic_type (type_identifier) @param_type))
(formal_parameter type: (scoped_type_identifier (type_identifier) @param_type))
(formal_parameter type: (array_type element: (type_identifier) @param_type))
(spread_parameter (type_identifier) @param_type)
(spread_parameter (generic_type (type_identifier) @param_type))

;; Return
(method_declaration type: (type_identifier) @return_type)
(method_declaration type: (generic_type (type_identifier) @return_type))
(method_declaration type: (scoped_type_identifier (type_identifier) @return_type))
(method_declaration type: (array_type element: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (generic_type (type_identifier) @field_type))
(field_declaration type: (scoped_type_identifier (type_identifier) @field_type))
(field_declaration type: (array_type element: (type_identifier) @field_type))

;; Impl (extends/implements)
(superclass (type_identifier) @impl_type)
(super_interfaces (type_list (type_identifier) @impl_type))

;; Bound (type parameter bounds)
(type_bound (type_identifier) @bound_type)

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types (Javadoc /** ... */ and regular comments)
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "public", "private", "protected", "static", "final", "abstract", "class", "interface",
    "extends", "implements", "return", "if", "else", "for", "while", "do", "switch", "case",
    "break", "continue", "new", "this", "super", "try", "catch", "finally", "throw", "throws",
    "import", "package", "void", "int", "boolean", "string", "true", "false", "null",
];

/// Post-process Java chunks: promote `static final` fields from Property to Constant,
/// and reclassify `constructor_declaration` nodes as Constructor.
fn post_process_java(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    if *chunk_type == ChunkType::Property && node.kind() == "field_declaration" {
        // Check if modifiers contain both "static" and "final"
        let field_text = &source[node.start_byte()..node.end_byte()];
        // Look at text before the type/name: modifiers come first
        let has_static = field_text.contains("static");
        let has_final = field_text.contains("final");
        if has_static && has_final {
            *chunk_type = ChunkType::Constant;
        }
    }
    // constructor_declaration nodes are constructors
    if node.kind() == "constructor_declaration"
        && matches!(*chunk_type, ChunkType::Function | ChunkType::Method)
    {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

/// Extracts the return type from a Java method signature and formats it as a documentation string.
/// 
/// Parses a Java method signature to identify the return type by finding the opening parenthesis and analyzing the words preceding it. The return type is assumed to be the second-to-last word before the parenthesis (the last word being the method name). Filters out Java modifiers and keywords that are not actual return types.
/// 
/// # Arguments
/// 
/// * `signature` - A Java method signature string (e.g., "public int add(int a, int b)")
/// 
/// # Returns
/// 
/// `Some(String)` containing a formatted return type description if a valid return type is found, or `None` if the signature cannot be parsed or the return type is a modifier/keyword rather than an actual type.
fn extract_return(signature: &str) -> Option<String> {
    // Java: return type is before the method name, similar to C
    // e.g., "public int add(int a, int b)" or "private static String getName()"
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            // Last word is method name, second-to-last is return type
            let ret_type = words[words.len() - 2];
            if !matches!(
                ret_type,
                "void"
                    | "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "final"
                    | "abstract"
                    | "synchronized"
                    | "native"
            ) {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "java",
    grammar: Some(|| tree_sitter_java::LANGUAGE.into()),
    extensions: &["java"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.java")),
    test_name_suggestion: Some(|name| super::pascal_test_name("test", name)),
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "String", "Object", "Integer", "Long", "Double", "Float", "Boolean", "Byte", "Character",
        "List", "ArrayList", "Map", "HashMap", "Set", "HashSet", "Collection", "Iterator",
        "Iterable", "Optional", "Stream", "Exception", "RuntimeException", "IOException", "Class",
        "Void", "Comparable", "Serializable", "Cloneable",
    ],
    container_body_kinds: &["class_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_java as PostProcessChunkFn),
    test_markers: &["@Test", "@ParameterizedTest", "@RepeatedTest"],
    test_path_patterns: &["%/test/%", "%/tests/%", "%Test.java"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "equals", "hashCode", "toString", "compareTo", "clone",
        "iterator", "run", "call", "close", "accept", "apply", "get",
    ],
    injections: &[],
    doc_format: "javadoc",
    doc_convention: "Use Javadoc format: @param, @return, @throws tags.",
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
    /// Parses a Java annotation type definition and verifies it is correctly identified as an interface chunk.
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
    /// Panics if the temporary file cannot be written, the parser fails to parse the file, the "Inject" annotation is not found in the parsed chunks, or the chunk type is not `ChunkType::Interface`.

    #[test]
    fn parse_java_annotation_type() {
        let content = r#"
public @interface Inject {
    String value() default "";
}
"#;
        let file = write_temp_file(content, "java");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ann = chunks.iter().find(|c| c.name == "Inject").unwrap();
        assert_eq!(ann.chunk_type, ChunkType::Interface);
    }
    /// Verifies that Java class fields are correctly parsed as Property chunks.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize, file parsing fails, the "name" field is not found in parsed chunks, the "MAX_SIZE" constant is not found in parsed chunks, or if the chunk types are not ChunkType::Property.

    #[test]
    fn parse_java_field_as_property() {
        let content = r#"
public class Config {
    private String name;
    public static final int MAX_SIZE = 100;
}
"#;
        let file = write_temp_file(content, "java");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let field = chunks.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(field.chunk_type, ChunkType::Property);
        // static final fields should be Constant, not Property
        let constant = chunks.iter().find(|c| c.name == "MAX_SIZE").unwrap();
        assert_eq!(constant.chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_java_constructor() {
        let content = r#"
public class Person {
    private String name;

    public Person(String name) {
        this.name = name;
    }

    public String getName() {
        return name;
    }
}
"#;
        let file = write_temp_file(content, "java");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ctor = chunks.iter().find(|c| c.name == "Person" && c.chunk_type != ChunkType::Class).unwrap();
        assert_eq!(ctor.chunk_type, ChunkType::Constructor);
        // getName should still be a Method
        let method = chunks.iter().find(|c| c.name == "getName").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }
}
