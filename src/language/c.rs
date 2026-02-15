//! C language definition

use super::{LanguageDef, SignatureStyle};

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

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (struct_specifier name: (type_identifier) @param_type))
(parameter_declaration type: (enum_specifier name: (type_identifier) @param_type))

;; Return
(function_definition type: (type_identifier) @return_type)
(function_definition type: (struct_specifier name: (type_identifier) @return_type))
(function_definition type: (enum_specifier name: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (struct_specifier name: (type_identifier) @field_type))
(field_declaration type: (enum_specifier name: (type_identifier) @field_type))

;; Alias (typedef)
(type_definition type: (type_identifier) @alias_type)
(type_definition type: (struct_specifier name: (type_identifier) @alias_type))
(type_definition type: (enum_specifier name: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "case", "break", "continue", "return",
    "typedef", "struct", "enum", "union", "void", "int", "char", "float", "double", "long",
    "short", "unsigned", "signed", "static", "extern", "const", "volatile", "sizeof",
    "null", "true", "false",
];

fn extract_return(signature: &str) -> Option<String> {
    // C: return type is before the function name, e.g., "int add(int a, int b)"
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        // Last word is function name, everything before is return type + modifiers
        if words.len() >= 2 {
            // Filter out storage class specifiers
            let type_words: Vec<&str> = words[..words.len() - 1]
                .iter()
                .filter(|w| {
                    !matches!(**w, "static" | "inline" | "extern" | "const" | "volatile")
                })
                .copied()
                .collect();
            if !type_words.is_empty() && type_words != ["void"] {
                let ret = type_words.join(" ");
                let ret_words = crate::nl::tokenize_identifier(&ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "c",
    grammar: Some(|| tree_sitter_c::LANGUAGE.into()),
    extensions: &["c", "h"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    type_query: Some(TYPE_QUERY),
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
