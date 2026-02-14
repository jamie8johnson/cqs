//! Go language definition

use super::{LanguageDef, SignatureStyle};

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

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "func", "var", "const", "type", "struct", "interface", "return", "if", "else", "for",
    "range", "switch", "case", "break", "continue", "go", "defer", "select", "chan", "map",
    "package", "import", "true", "false", "nil",
];

fn extract_return(signature: &str) -> Option<String> {
    // Go: `func name(params) returnType {` or `func (recv) name(params) returnType {`
    // Strip trailing { first
    let sig = signature.trim_end_matches('{').trim();

    if sig.ends_with(')') {
        // Check if it's a multi-return like (string, error)
        // Find the matching ( for the final )
        let mut depth = 0;
        let mut start_idx = None;
        for (i, c) in sig.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        start_idx = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(start) = start_idx {
            // Check if there's a ) before this ( - that would be the params close
            let before = &sig[..start].trim();
            if before.ends_with(')') {
                // Multi-return: extract the (...)
                let ret = &sig[start..];
                if !ret.is_empty() {
                    return Some(format!("Returns {}", ret));
                }
            }
        }
        return None;
    } else {
        // Plain return type after last )
        if let Some(paren) = sig.rfind(')') {
            let ret = sig[paren + 1..].trim();
            if ret.is_empty() {
                return None;
            }
            let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
            return Some(format!("Returns {}", ret_words));
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "go",
    grammar: Some(|| tree_sitter_go::LANGUAGE.into()),
    extensions: &["go"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &["method_declaration"],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}_test.go")),
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
