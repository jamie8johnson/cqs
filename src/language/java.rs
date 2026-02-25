//! Java language definition

use super::{LanguageDef, SignatureStyle};

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

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(formal_parameter type: (type_identifier) @param_type)
(formal_parameter type: (generic_type (type_identifier) @param_type))
(formal_parameter type: (scoped_type_identifier (type_identifier) @param_type))
(formal_parameter type: (array_type element: (type_identifier) @param_type))
(spread_parameter (type_identifier) @param_type)
(spread_parameter (generic_type (type_identifier) @param_type))

;; Return
(method_declaration type: (type_identifier) @return_type)
(method_declaration type: (generic_type (type_identifier) @return_type))
(method_declaration type: (scoped_type_identifier (type_identifier) @return_type))
(method_declaration type: (array_type element: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (generic_type (type_identifier) @field_type))
(field_declaration type: (scoped_type_identifier (type_identifier) @field_type))
(field_declaration type: (array_type element: (type_identifier) @field_type))

;; Impl (extends/implements)
(superclass (type_identifier) @impl_type)
(super_interfaces (type_list (type_identifier) @impl_type))

;; Bound (type parameter bounds)
(type_bound (type_identifier) @bound_type)

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types (Javadoc /** ... */ and regular comments)
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "public", "private", "protected", "static", "final", "abstract", "class", "interface",
    "extends", "implements", "return", "if", "else", "for", "while", "do", "switch", "case",
    "break", "continue", "new", "this", "super", "try", "catch", "finally", "throw", "throws",
    "import", "package", "void", "int", "boolean", "string", "true", "false", "null",
];

fn extract_return(signature: &str) -> Option<String> {
    // Java: return type is before the method name, similar to C
    // e.g., "public int add(int a, int b)" or "private static String getName()"
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            // Last word is method name, second-to-last is return type
            let ret_type = words[words.len() - 2];
            if !matches!(
                ret_type,
                "void"
                    | "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "final"
                    | "abstract"
                    | "synchronized"
                    | "native"
            ) {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "java",
    grammar: Some(|| tree_sitter_java::LANGUAGE.into()),
    extensions: &["java"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.java")),
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "String", "Object", "Integer", "Long", "Double", "Float", "Boolean", "Byte", "Character",
        "List", "ArrayList", "Map", "HashMap", "Set", "HashSet", "Collection", "Iterator",
        "Iterable", "Optional", "Stream", "Exception", "RuntimeException", "IOException", "Class",
        "Void", "Comparable", "Serializable", "Cloneable",
    ],
    container_body_kinds: &["class_body"],
    extract_container_name: None,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
