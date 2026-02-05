//! Java language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

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
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(method_invocation
  name: (identifier) @callee)

(object_creation_expression
  type: (type_identifier) @callee)
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("class", ChunkType::Class),
    ("interface", ChunkType::Interface),
    ("enum", ChunkType::Enum),
    ("struct", ChunkType::Struct),
];

/// Doc comment node types (Javadoc /** ... */ and regular comments)
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "java",
    grammar: || tree_sitter_java::LANGUAGE.into(),
    extensions: &["java"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
