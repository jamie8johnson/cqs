//! INI language definition
//!
//! INI is a simple configuration format. Chunks are sections and settings.
//! No function calls or type references.

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting INI sections and settings.
const CHUNK_QUERY: &str = r#"
;; Sections: [section_name]
(section
  (section_name
    (text) @name)) @module

;; Settings: key = value
(setting
  (setting_name) @name) @property
"#;

/// Doc comment node types — INI uses `;` or `#` comments
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &["true", "false", "yes", "no", "on", "off"];

/// Extracts the return type from a function signature.
/// 
/// # Arguments
/// 
/// * `_signature` - A function signature string (unused, as INI format does not support functions)
/// 
/// # Returns
/// 
/// Returns `None`, as INI files do not contain function definitions or return types.
fn extract_return(_signature: &str) -> Option<String> {
    // INI has no functions or return types
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "ini",
    grammar: Some(|| tree_sitter_ini::LANGUAGE.into()),
    extensions: &["ini", "cfg"],
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
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
    injections: &[],
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
    /// Tests that the parser correctly identifies INI file sections as Module chunks.
    /// 
    /// This test verifies that when parsing an INI configuration file with multiple named sections (e.g., `[database]`, `[server]`), each section is properly recognized and represented as a Module-type chunk with the correct section name.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts on parsing results and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create, fails to parse the temporary file, or if either the "database" or "server" sections are not found with ChunkType::Module.

    #[test]
    fn parse_ini_sections() {
        let content = r#"[database]
host = localhost
port = 5432

[server]
host = 0.0.0.0
port = 8080
"#;
        let file = write_temp_file(content, "ini");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            chunks.iter().any(|c| c.name == "database" && c.chunk_type == ChunkType::Module),
            "Expected 'database' section as Module, got: {:?}",
            names
        );
        assert!(
            chunks.iter().any(|c| c.name == "server" && c.chunk_type == ChunkType::Module),
            "Expected 'server' section as Module, got: {:?}",
            names
        );
    }
    /// Parses an INI configuration file and verifies that a debug property setting is correctly identified.
    /// 
    /// This test function creates a temporary INI file with app configuration settings, parses it using the Parser, and asserts that the debug property is found and correctly typed as a Property chunk.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, fails to parse the temporary file, or if the debug property is not found in the parsed chunks with the expected Property chunk type.

    #[test]
    fn parse_ini_settings() {
        let content = r#"[app]
debug = true
log_level = info
"#;
        let file = write_temp_file(content, "ini");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let debug = chunks
            .iter()
            .find(|c| c.name == "debug" && c.chunk_type == ChunkType::Property);
        assert!(
            debug.is_some(),
            "Expected 'debug' setting as Property, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
    }
    /// Verifies that the parser correctly handles INI files without extracting any function calls.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser extracts any calls from an INI file chunk, indicating incorrect parsing behavior.

    #[test]
    fn parse_ini_no_calls() {
        let content = "[section]\nkey = value\n";
        let file = write_temp_file(content, "ini");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "INI should have no calls");
        }
    }

    #[test]
    fn test_extract_return_ini() {
        assert_eq!(extract_return("[section]"), None);
        assert_eq!(extract_return("key = value"), None);
        assert_eq!(extract_return(""), None);
    }
}
