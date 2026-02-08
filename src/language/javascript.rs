//! JavaScript language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

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
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (member_expression
    property: (property_identifier) @callee))
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("class", ChunkType::Class),
];

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "const", "let", "var", "return", "if", "else", "for", "while", "do",
    "switch", "case", "break", "continue", "new", "this", "class", "extends", "import",
    "export", "from", "default", "try", "catch", "finally", "throw", "async", "await",
    "true", "false", "null", "undefined", "typeof", "instanceof", "void",
];

fn extract_return(_signature: &str) -> Option<String> {
    // JavaScript doesn't have type annotations in signatures.
    // JSDoc parsing is handled separately in NL generation.
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "javascript",
    grammar: || tree_sitter_javascript::LANGUAGE.into(),
    extensions: &["js", "jsx", "mjs", "cjs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
