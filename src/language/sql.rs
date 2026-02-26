//! SQL language definition

use super::{LanguageDef, SignatureStyle};

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
  (object_reference) @name) @function

(create_trigger
  name: (identifier) @name) @function

;; Tables
(create_table
  (object_reference) @name) @struct

;; User-defined types
(create_type
  (object_reference) @name) @typealias
"#;

/// Tree-sitter query for extracting calls (function invocations + EXEC)
const CALL_QUERY: &str = r#"
(invocation
  (object_reference) @callee)

(execute_statement
  (object_reference) @callee)
"#;

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
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
    use crate::parser::{ChunkType, Parser};
    use std::io::Write;

    fn write_temp_file(content: &str, ext: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn parse_sql_create_table() {
        let content = "CREATE TABLE users (\n  id INT PRIMARY KEY,\n  name VARCHAR(100)\n);\n";
        let file = write_temp_file(content, "sql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let table = chunks.iter().find(|c| c.name == "users").unwrap();
        assert_eq!(table.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_sql_create_view_as_function() {
        let content = "CREATE VIEW active_users AS\nSELECT * FROM users WHERE active = 1;\n";
        let file = write_temp_file(content, "sql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let view = chunks.iter().find(|c| c.name == "active_users").unwrap();
        assert_eq!(view.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_sql_create_type() {
        let content = "CREATE TYPE status AS ENUM ('active', 'inactive');\n";
        let file = write_temp_file(content, "sql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ty = chunks.iter().find(|c| c.name == "status").unwrap();
        assert_eq!(ty.chunk_type, ChunkType::TypeAlias);
    }
}
