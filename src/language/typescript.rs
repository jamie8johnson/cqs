//! TypeScript language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

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
    ("interface", ChunkType::Interface),
    ("enum", ChunkType::Enum),
];

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "typescript",
    grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    extensions: &["ts", "tsx"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
