//! C language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting C code chunks
const CHUNK_QUERY: &str = r#"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @enum

(type_definition
  declarator: (type_identifier) @name) @const

(declaration
  declarator: (init_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @function
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("struct", ChunkType::Struct),
    ("enum", ChunkType::Enum),
    ("const", ChunkType::Constant),
];

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "c",
    grammar: || tree_sitter_c::LANGUAGE.into(),
    extensions: &["c", "h"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
