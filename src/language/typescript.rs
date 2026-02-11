//! TypeScript language definition

use super::{LanguageDef, SignatureStyle};

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

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "const", "let", "var", "return", "if", "else", "for", "while", "do",
    "switch", "case", "break", "continue", "new", "this", "class", "extends", "import",
    "export", "from", "default", "try", "catch", "finally", "throw", "async", "await",
    "true", "false", "null", "undefined", "typeof", "instanceof", "void",
];

fn extract_return(signature: &str) -> Option<String> {
    // TypeScript: return type after `):` e.g. `function foo(): string`
    if let Some(colon) = signature.rfind("):") {
        let ret = signature[colon + 2..].trim();
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "typescript",
    grammar: Some(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
    extensions: &["ts", "tsx"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
