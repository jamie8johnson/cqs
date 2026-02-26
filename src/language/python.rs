//! Python language definition

use super::{LanguageDef, SignatureStyle};

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

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(typed_parameter type: (type (identifier) @param_type))
(typed_parameter type: (type (generic_type (identifier) @param_type)))
(typed_default_parameter type: (type (identifier) @param_type))
(typed_default_parameter type: (type (generic_type (identifier) @param_type)))

;; Return
(function_definition return_type: (type (identifier) @return_type))
(function_definition return_type: (type (generic_type (identifier) @return_type)))

;; Field
(assignment type: (type (identifier) @field_type))
(assignment type: (type (generic_type (identifier) @field_type)))

;; Impl (class inheritance)
(class_definition superclasses: (argument_list (identifier) @impl_type))

;; Alias (PEP 695)
(type_alias_statement (type (identifier) @alias_type))

;; Catch-all (scoped to type positions)
(type (identifier) @type_ref)
"#;

/// Doc comment node types (sibling comments and standalone strings before a definition)
const DOC_NODES: &[&str] = &["string", "comment"];

const STOPWORDS: &[&str] = &[
    "def", "class", "self", "return", "if", "elif", "else", "for", "while", "import",
    "from", "as", "with", "try", "except", "finally", "raise", "pass", "break", "continue",
    "and", "or", "not", "in", "is", "true", "false", "none", "lambda", "yield", "global",
    "nonlocal",
];

fn extract_return(signature: &str) -> Option<String> {
    if let Some(arrow) = signature.rfind("->") {
        let ret = signature[arrow + 2..].trim().trim_end_matches(':');
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "python",
    grammar: Some(|| tree_sitter_python::LANGUAGE.into()),
    extensions: &["py", "pyi"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilColon,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_definition"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/test_{stem}.py")),
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "str", "int", "float", "bool", "list", "dict", "set", "tuple", "None", "Any", "Optional",
        "Union", "List", "Dict", "Set", "Tuple", "Type", "Callable", "Iterator", "Generator",
        "Coroutine", "Exception", "ValueError", "TypeError", "KeyError", "IndexError", "Path",
        "Self",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
