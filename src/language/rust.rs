//! Rust language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Rust code chunks
const CHUNK_QUERY: &str = r#"
(function_item
  name: (identifier) @name) @function

(struct_item
  name: (type_identifier) @name) @struct

(enum_item
  name: (type_identifier) @name) @enum

(trait_item
  name: (type_identifier) @name) @trait

(const_item
  name: (identifier) @name) @const

(static_item
  name: (identifier) @name) @const
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))

(call_expression
  function: (scoped_identifier
    name: (identifier) @callee))

(macro_invocation
  macro: (identifier) @callee)
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("struct", ChunkType::Struct),
    ("enum", ChunkType::Enum),
    ("trait", ChunkType::Trait),
    ("const", ChunkType::Constant),
];

/// Doc comment node types
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "rust",
    grammar: || tree_sitter_rust::LANGUAGE.into(),
    extensions: &["rs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
