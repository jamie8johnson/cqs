//! JavaScript language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting JavaScript code chunks
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
  name: (identifier) @name) @class

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

/// Post-process JavaScript chunks: skip `@const` captures whose value is an arrow_function
/// or function_expression (already captured as Function), and skip const inside function bodies.
fn post_process_javascript(
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

/// Extracts the return type from a JavaScript function signature.
/// 
/// # Arguments
/// 
/// * `_signature` - A string slice containing a JavaScript function signature
/// 
/// # Returns
/// 
/// Always returns `None`, as JavaScript function signatures do not contain type annotations. Return type information should be extracted from JSDoc comments instead, which are handled separately during natural language generation.
fn extract_return(_signature: &str) -> Option<String> {
    // JavaScript doesn't have type annotations in signatures.
    // JSDoc parsing is handled separately in NL generation.
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "javascript",
    grammar: Some(|| tree_sitter_javascript::LANGUAGE.into()),
    extensions: &["js", "jsx", "mjs", "cjs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.test.js")),
    test_name_suggestion: Some(|name| format!("test('{}', ...)", name)),
    type_query: None,
    common_types: &[
        "Array", "Map", "Set", "Promise", "Date", "Error", "RegExp", "Function", "Object",
        "Symbol", "WeakMap", "WeakSet",
    ],
    container_body_kinds: &["class_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_javascript as PostProcessChunkFn),
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
    skip_line_prefixes: &["class ", "export "],
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

    #[test]
    fn parse_javascript_const_value() {
        let content = r#"
const MAX_RETRIES = 3;
const API_URL = "https://example.com";
const handler = () => { return 1; };

function foo() {}
"#;
        let file = write_temp_file(content, "js");
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
