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

/// Tree-sitter query for extracting type references
///
/// Classified captures: @param_type, @return_type, @field_type, @impl_type, @bound_type, @alias_type
/// Catch-all: @type_ref (for types inside generics not reached by classified patterns)
const TYPE_QUERY: &str = r#"
;; Param
(parameter type: (type_identifier) @param_type)
(parameter type: (generic_type type: (type_identifier) @param_type))
(parameter type: (reference_type type: (type_identifier) @param_type))
(parameter type: (reference_type type: (generic_type type: (type_identifier) @param_type)))
(parameter type: (scoped_type_identifier name: (type_identifier) @param_type))

;; Return
(function_item return_type: (type_identifier) @return_type)
(function_item return_type: (generic_type type: (type_identifier) @return_type))
(function_item return_type: (reference_type type: (type_identifier) @return_type))
(function_item return_type: (reference_type type: (generic_type type: (type_identifier) @return_type)))
(function_item return_type: (scoped_type_identifier name: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (generic_type type: (type_identifier) @field_type))
(field_declaration type: (reference_type type: (type_identifier) @field_type))
(field_declaration type: (reference_type type: (generic_type type: (type_identifier) @field_type)))
(field_declaration type: (scoped_type_identifier name: (type_identifier) @field_type))

;; Impl
(impl_item type: (type_identifier) @impl_type)
(impl_item type: (generic_type type: (type_identifier) @impl_type))
(impl_item trait: (type_identifier) @impl_type)
(impl_item trait: (scoped_type_identifier name: (type_identifier) @impl_type))

;; Bound
(trait_bounds (type_identifier) @bound_type)
(trait_bounds (scoped_type_identifier name: (type_identifier) @bound_type))

;; Alias
(type_item type: (type_identifier) @alias_type)
(type_item type: (generic_type type: (type_identifier) @alias_type))

;; Catch-all (captures types inside generics, type_arguments, etc.)
(type_identifier) @type_ref
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
    type_query: Some(TYPE_QUERY),
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
