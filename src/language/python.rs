//! Python language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Python code chunks
const CHUNK_QUERY: &str = r#"
(function_definition
  name: (identifier) @name) @function

(class_definition
  name: (identifier) @name) @class
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call
  function: (identifier) @callee)

(call
  function: (attribute
    attribute: (identifier) @callee))
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("class", ChunkType::Class),
];

/// Doc comment node types (docstrings are expression_statements containing strings)
const DOC_NODES: &[&str] = &["expression_statement"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "python",
    grammar: || tree_sitter_python::LANGUAGE.into(),
    extensions: &["py", "pyi"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilColon,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
