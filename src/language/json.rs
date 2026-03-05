//! JSON language definition
//!
//! JSON is a data interchange format. Chunks are top-level key-value pairs.
//! No function calls or type references.

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting JSON top-level pairs.
const CHUNK_QUERY: &str = r#"
;; Key-value pairs: "key": value
(pair
  key: (string
    (string_content) @name)) @property
"#;

/// Doc comment node types — JSON has no comments (JSONC uses `//` and `/* */`)
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &["true", "false", "null"];

fn extract_return(_signature: &str) -> Option<String> {
    // JSON has no functions or return types
    None
}

/// Post-process JSON chunks: only keep top-level pairs.
/// A top-level pair's parent is an `object` whose parent is `document`.
fn post_process_json(
    _name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // pair > object > document
    if let Some(parent) = node.parent() {
        if parent.kind() == "object" {
            if let Some(grandparent) = parent.parent() {
                return grandparent.kind() == "document";
            }
        }
    }
    false
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "json",
    grammar: Some(|| tree_sitter_json::LANGUAGE.into()),
    extensions: &["json", "jsonc"],
    chunk_query: CHUNK_QUERY,
    call_query: None,
    signature_style: SignatureStyle::FirstLine,
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
    post_process_chunk: Some(post_process_json),
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
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
    fn parse_json_top_level_keys() {
        let content = r#"{
  "name": "my-project",
  "version": "1.0.0",
  "dependencies": {
    "lodash": "4.17.21"
  }
}
"#;
        let file = write_temp_file(content, "json");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"name"),
            "Expected 'name' key, got: {:?}",
            names
        );
        assert!(
            names.contains(&"version"),
            "Expected 'version' key, got: {:?}",
            names
        );
        assert!(
            names.contains(&"dependencies"),
            "Expected 'dependencies' key, got: {:?}",
            names
        );
        // Nested key "lodash" should be filtered out
        assert!(
            !names.contains(&"lodash"),
            "Nested key 'lodash' should be filtered, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_json_chunk_type() {
        let content = r#"{"key": "value"}"#;
        let file = write_temp_file(content, "json");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let key = chunks.iter().find(|c| c.name == "key");
        assert!(key.is_some(), "Expected 'key' chunk");
        assert_eq!(key.unwrap().chunk_type, ChunkType::Property);
    }

    #[test]
    fn parse_json_no_calls() {
        let content = r#"{"a": 1, "b": 2}"#;
        let file = write_temp_file(content, "json");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "JSON should have no calls");
        }
    }

    #[test]
    fn test_extract_return_json() {
        assert_eq!(extract_return(r#""key": "value""#), None);
        assert_eq!(extract_return(""), None);
    }
}
