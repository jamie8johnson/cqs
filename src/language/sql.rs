//! SQL language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting SQL code chunks
const CHUNK_QUERY: &str = r#"
(create_function
  (object_reference) @name) @function

(create_procedure
  (object_reference) @name) @function

(alter_function
  (object_reference) @name) @function

(alter_procedure
  (object_reference) @name) @function

(create_view
  (object_reference) @name) @const

(create_trigger
  name: (identifier) @name) @function
"#;

/// Tree-sitter query for extracting calls (function invocations + EXEC)
const CALL_QUERY: &str = r#"
(invocation
  (object_reference) @callee)

(execute_statement
  (object_reference) @callee)
"#;

/// Mapping from capture names to chunk types
const TYPE_MAP: &[(&str, ChunkType)] = &[
    ("function", ChunkType::Function),
    ("const", ChunkType::Constant),
];

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment", "marginalia"];

const STOPWORDS: &[&str] = &[
    "create", "alter", "procedure", "function", "view", "trigger", "begin", "end", "declare", "set",
    "select", "from", "where", "insert", "into", "update", "delete", "exec", "execute", "as",
    "returns", "return", "if", "else", "while", "and", "or", "not", "null", "int", "varchar",
    "nvarchar", "decimal", "table", "on", "after", "before", "instead", "of", "for", "each",
    "row", "order", "by", "group", "having", "join", "inner", "left", "right", "outer", "go",
    "with", "nocount", "language", "replace",
];

fn extract_return(signature: &str) -> Option<String> {
    // SQL functions: look for RETURNS type between name and AS
    let upper = signature.to_uppercase();
    if let Some(ret_pos) = upper.find("RETURNS") {
        let after = &signature[ret_pos + 7..].trim();
        // Take the first word as the return type, lowercase it
        // SQL types are all-caps (DECIMAL, INT, VARCHAR) — just lowercase, don't tokenize
        let type_str = after.split_whitespace().next()?;
        // Strip precision suffix like (10,2)
        let base_type = type_str.split('(').next().unwrap_or(type_str);
        return Some(format!("Returns {}", base_type.to_lowercase()));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "sql",
    grammar: Some(|| tree_sitter_sql::LANGUAGE.into()),
    extensions: &["sql"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilAs,
    type_map: TYPE_MAP,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
