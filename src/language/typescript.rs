//! TypeScript language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting TypeScript code chunks
const CHUNK_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_definition
  name: (property_identifier) @name) @function

;; Arrow function assigned to variable: const foo = () => {}
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

;; Arrow function assigned with var/let
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

(class_declaration
  name: (type_identifier) @name) @class

(interface_declaration
  name: (type_identifier) @name) @interface

(enum_declaration
  name: (identifier) @name) @enum

(type_alias_declaration
  name: (type_identifier) @name) @typealias

;; Namespace/module declarations
(internal_module
  name: (identifier) @name) @module

;; Module-level const declarations (non-function values)
(lexical_declaration
  kind: "const"
  (variable_declarator
    name: (identifier) @name
    value: (_) @_val) @const)
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (member_expression
    property: (property_identifier) @callee))
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(required_parameter type: (type_annotation (type_identifier) @param_type))
(required_parameter type: (type_annotation (generic_type name: (type_identifier) @param_type)))
(optional_parameter type: (type_annotation (type_identifier) @param_type))
(optional_parameter type: (type_annotation (generic_type name: (type_identifier) @param_type)))

;; Return
(function_declaration return_type: (type_annotation (type_identifier) @return_type))
(function_declaration return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))
(method_definition return_type: (type_annotation (type_identifier) @return_type))
(method_definition return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))
(arrow_function return_type: (type_annotation (type_identifier) @return_type))
(arrow_function return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))

;; Field
(public_field_definition type: (type_annotation (type_identifier) @field_type))
(public_field_definition type: (type_annotation (generic_type name: (type_identifier) @field_type)))
(property_signature type: (type_annotation (type_identifier) @field_type))
(property_signature type: (type_annotation (generic_type name: (type_identifier) @field_type)))

;; Impl (extends/implements)
(class_heritage (extends_clause value: (identifier) @impl_type))
(class_heritage (implements_clause (type_identifier) @impl_type))
(extends_type_clause (type_identifier) @impl_type)

;; Bound (type parameter constraints)
(constraint (type_identifier) @bound_type)

;; Alias
(type_alias_declaration value: (type_identifier) @alias_type)
(type_alias_declaration value: (generic_type name: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "const", "let", "var", "return", "if", "else", "for", "while", "do",
    "switch", "case", "break", "continue", "new", "this", "class", "extends", "import",
    "export", "from", "default", "try", "catch", "finally", "throw", "async", "await",
    "true", "false", "null", "undefined", "typeof", "instanceof", "void",
];

/// Returns true if the node is nested inside a function/method/arrow body.
fn is_inside_function(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        match parent.kind() {
            "function_declaration" | "function_expression" | "arrow_function"
            | "method_definition" | "generator_function_declaration"
            | "generator_function" => return true,
            _ => {}
        }
        cursor = parent.parent();
    }
    false
}

/// Post-process TypeScript chunks: skip `@const` captures whose value is an arrow_function
/// or function_expression (already captured as Function), and skip const inside function bodies.
fn post_process_typescript(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        // Skip const declarations inside function bodies — only capture module-level
        if is_inside_function(node) {
            return false;
        }
        // node is the variable_declarator; check if the value child is a function
        if let Some(value) = node.child_by_field_name("value") {
            let kind = value.kind();
            if kind == "arrow_function" || kind == "function_expression" || kind == "function" {
                return false;
            }
        }
    }
    true
}

/// Extracts the return type from a TypeScript function signature and formats it as a description.
/// 
/// # Arguments
/// * `signature` - A TypeScript function signature string to parse
/// 
/// # Returns
/// `Some(String)` containing a formatted return type description (e.g., "Returns string") if a return type annotation is found after `):`, or `None` if no return type is present or the signature is malformed.
fn extract_return(signature: &str) -> Option<String> {
    // TypeScript: return type after `):` e.g. `function foo(): string`
    if let Some(colon) = signature.rfind("):") {
        let ret = signature[colon + 2..].trim();
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "typescript",
    grammar: Some(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
    extensions: &["ts", "tsx"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.test.ts")),
    test_name_suggestion: Some(|name| format!("test('{}', ...)", name)),
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "string", "number", "boolean", "void", "null", "undefined", "any", "never", "unknown",
        "Array", "Map", "Set", "Promise", "Record", "Partial", "Required", "Readonly", "Pick",
        "Omit", "Exclude", "Extract", "NonNullable", "ReturnType", "Date", "Error", "RegExp",
        "Function", "Object", "Symbol",
    ],
    container_body_kinds: &["class_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_typescript as PostProcessChunkFn),
    test_markers: &["describe(", "it(", "test("],
    test_path_patterns: &["%.test.%", "%.spec.%", "%/tests/%"],
    structural_matchers: None,
    entry_point_names: &["handler", "middleware", "beforeEach", "afterEach", "beforeAll", "afterAll"],
    trait_method_names: &["toString", "valueOf", "toJSON"],
    injections: &[],
    doc_format: "javadoc",
    doc_convention: "Use JSDoc format: @param {type} name, @returns {type}, @throws {type}.",
    field_style: FieldStyle::NameFirst {
        separators: ":=;",
        strip_prefixes: "public private protected readonly static",
    },
    skip_line_prefixes: &["class ", "interface ", "type ", "export "],
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
    /// Parses a TypeScript namespace declaration and verifies it is recognized as a module chunk.
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
    /// Panics if the temporary file cannot be written, the parser fails to initialize, the file cannot be parsed, the "Validators" namespace is not found in the parsed chunks, or the chunk type assertion fails.

    #[test]
    fn parse_typescript_namespace() {
        let content = "namespace Validators {\n  export function check() {}\n}\n";
        let file = write_temp_file(content, "ts");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ns = chunks.iter().find(|c| c.name == "Validators").unwrap();
        assert_eq!(ns.chunk_type, ChunkType::Module);
    }
    /// Parses a TypeScript type alias and verifies it is correctly identified.
    /// 
    /// This test function writes a TypeScript type alias definition to a temporary file, parses it using the Parser, and asserts that the resulting chunk is correctly identified as a TypeAlias with the name "Result".
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the file cannot be parsed, or if a chunk named "Result" is not found in the parsed output.

    #[test]
    fn parse_typescript_type_alias() {
        let content = "type Result = Success | Failure;\n";
        let file = write_temp_file(content, "ts");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "Result").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_typescript_const_value() {
        let content = r#"
const MAX_RETRIES: number = 3;
const API_URL = "https://example.com";
const handler = () => { return 1; };

function foo() {}
"#;
        let file = write_temp_file(content, "ts");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
        assert!(max.is_some(), "Should capture MAX_RETRIES");
        assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
        let url = chunks.iter().find(|c| c.name == "API_URL");
        assert!(url.is_some(), "Should capture API_URL");
        assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);
        // handler is an arrow function — should be Function, not Constant
        let handler = chunks.iter().find(|c| c.name == "handler");
        assert!(handler.is_some(), "Should capture handler");
        assert_eq!(handler.unwrap().chunk_type, ChunkType::Function);
    }
}
