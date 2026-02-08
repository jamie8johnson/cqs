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
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
