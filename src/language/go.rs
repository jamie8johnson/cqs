//! Go language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Go code chunks
const CHUNK_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_declaration
  name: (field_identifier) @name) @function

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type))) @struct

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type))) @interface

(const_declaration
  (const_spec
    name: (identifier) @name)) @const
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (selector_expression
    field: (field_identifier) @callee))
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("struct", ChunkType::Struct),
    ("interface", ChunkType::Interface),
    ("const", ChunkType::Constant),
];

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "go",
    grammar: || tree_sitter_go::LANGUAGE.into(),
    extensions: &["go"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
    method_node_kinds: &["method_declaration"],
    method_containers: &[],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
