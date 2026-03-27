//! SQL language definition

use super::{FieldStyle, LanguageDef, SignatureStyle};

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

/// Extracts the return type from a SQL function signature.
/// 
/// Searches for the "RETURNS" keyword in a SQL function signature and extracts the return type that follows it. The return type is the first word after "RETURNS", with any precision suffixes (e.g., "(10,2)") removed, and converted to lowercase.
/// 
/// # Arguments
/// 
/// * `signature` - A SQL function signature string to parse
/// 
/// # Returns
/// 
/// `Some(String)` containing a formatted return type description (e.g., "Returns int"), or `None` if no "RETURNS" keyword is found in the signature.
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
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
    injections: &[],
    doc_format: "default",
    doc_convention: "",
    field_style: FieldStyle::None,
    skip_line_prefixes: &[],
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
    /// Parses a SQL CREATE TABLE statement and verifies the resulting parsed structure.
    /// 
    /// This test function creates a temporary SQL file containing a CREATE TABLE statement for a "users" table with id and name columns, parses it using the Parser, and asserts that the parsed chunk is correctly identified as a Struct type with the name "users".
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the file cannot be parsed, the "users" chunk is not found in the parsed output, or the chunk type is not ChunkType::Struct.

    #[test]
    fn parse_sql_create_table() {
        let content = "CREATE TABLE users (\n  id INT PRIMARY KEY,\n  name VARCHAR(100)\n);\n";
        let file = write_temp_file(content, "sql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let table = chunks.iter().find(|c| c.name == "users").unwrap();
        assert_eq!(table.chunk_type, ChunkType::Struct);
    }
    /// Verifies that a SQL CREATE VIEW statement is correctly parsed as a Function chunk type.
    /// 
    /// This is a unit test that validates the parser's ability to recognize and categorize SQL VIEW definitions. It creates a temporary SQL file containing a CREATE VIEW statement, parses it, and asserts that the resulting chunk has the correct name and is classified as a Function chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a standalone test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts conditions and returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if any unwrap() call fails (file creation, parser initialization, or parsing) or if the assertions do not hold (chunk not found, incorrect chunk type).

    #[test]
    fn parse_sql_create_view_as_function() {
        let content = "CREATE VIEW active_users AS\nSELECT * FROM users WHERE active = 1;\n";
        let file = write_temp_file(content, "sql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let view = chunks.iter().find(|c| c.name == "active_users").unwrap();
        assert_eq!(view.chunk_type, ChunkType::Function);
    }
    /// Parses a SQL CREATE TYPE statement and verifies the resulting chunk metadata.
    /// 
    /// This function creates a temporary SQL file containing a CREATE TYPE ENUM statement, parses it using the Parser, and asserts that the parsed chunk has the correct name and type classification.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (unit type)
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created or written
    /// - The parser fails to initialize or parse the file
    /// - A chunk named "status" is not found in the parsed results
    /// - The parsed chunk's type is not `ChunkType::TypeAlias`

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
