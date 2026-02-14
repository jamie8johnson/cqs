//! Rust language definition

use super::{LanguageDef, SignatureStyle};

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

/// Doc comment node types
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "impl", "mod", "struct", "enum", "trait", "type",
    "where", "const", "static", "unsafe", "async", "await", "move", "ref", "self", "super",
    "crate", "return", "if", "else", "for", "while", "loop", "match", "break", "continue",
    "as", "in", "true", "false", "some", "none", "ok", "err",
];

fn extract_return(signature: &str) -> Option<String> {
    if let Some(arrow) = signature.find("->") {
        let ret = signature[arrow + 2..].trim();
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "rust",
    grammar: Some(|| tree_sitter_rust::LANGUAGE.into()),
    extensions: &["rs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["impl_item", "trait_item"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/{stem}_test.rs")),
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
