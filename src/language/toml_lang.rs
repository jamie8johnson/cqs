//! TOML language definition
//!
//! TOML is a configuration language. Chunks are tables and top-level pairs.
//! No function calls or type references.

use super::{ChunkType, FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting TOML sections.
/// Tables → Section, table array elements → Section, top-level pairs → Property.
const CHUNK_QUERY: &str = r#"
;; Tables ([section])
(table
  (bare_key) @name) @property

;; Tables with dotted keys ([section.subsection])
(table
  (dotted_key) @name) @property

;; Tables with quoted keys (["section"])
(table
  (quoted_key) @name) @property

;; Table arrays ([[array]])
(table_array_element
  (bare_key) @name) @property

;; Table arrays with dotted keys ([[array.sub]])
(table_array_element
  (dotted_key) @name) @property

;; Top-level key-value pairs
(pair
  (bare_key) @name) @property

;; Top-level dotted key-value pairs
(pair
  (dotted_key) @name) @property

;; Top-level quoted key-value pairs
(pair
  (quoted_key) @name) @property
"#;

/// Doc comment node types — TOML uses `# comments`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &["true", "false"];

/// Strip quotes from TOML quoted keys.
fn post_process_toml(
    name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Only keep top-level pairs (skip pairs nested inside tables)
    if node.kind() == "pair" {
        if let Some(parent) = node.parent() {
            // A pair inside a table or table_array_element is nested
            if parent.kind() == "table" || parent.kind() == "table_array_element" {
                return false;
            }
        }
    }
    // Strip surrounding quotes from quoted keys
    if name.starts_with('"') && name.ends_with('"') && name.len() >= 2 {
        *name = name[1..name.len() - 1].to_string();
    }
    true
}

/// Extracts the return type from a function signature.
/// This function is a no-op for TOML content, as TOML has no function or return type syntax.
/// # Arguments
/// * `_signature` - A function signature string (unused for TOML)
/// # Returns
/// Always returns `None`, as TOML does not support function definitions or return type annotations.
fn extract_return(_signature: &str) -> Option<String> {
    // TOML has no functions or return types
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "toml",
    grammar: Some(|| tree_sitter_toml::LANGUAGE.into()),
    extensions: &["toml"],
    chunk_query: CHUNK_QUERY,
    call_query: None,
    signature_style: SignatureStyle::FirstLine,
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
    post_process_chunk: Some(post_process_toml),
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
    use super::*;
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
    fn parse_toml_table() {
        let content = r#"
[package]
name = "my-crate"
version = "1.0.0"

[dependencies]
serde = "1.0"
"#;
        let file = write_temp_file(content, "toml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"package"),
            "Expected 'package' table, got: {:?}",
            names
        );
        assert!(
            names.contains(&"dependencies"),
            "Expected 'dependencies' table, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_toml_chunk_type() {
        let content = r#"
[server]
host = "localhost"
port = 8080
"#;
        let file = write_temp_file(content, "toml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let server = chunks.iter().find(|c| c.name == "server");
        assert!(server.is_some(), "Expected 'server' chunk");
        assert_eq!(server.unwrap().chunk_type, ChunkType::Property);
    }

    #[test]
    fn parse_toml_no_calls() {
        let content = r#"
[database]
host = "localhost"
port = 5432
"#;
        let file = write_temp_file(content, "toml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "TOML should have no calls");
        }
    }

    #[test]
    fn test_extract_return_toml() {
        assert_eq!(extract_return("[section]"), None);
        assert_eq!(extract_return(""), None);
    }
}
