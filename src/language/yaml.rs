//! YAML language definition
//!
//! YAML is a configuration/data language. Chunks are top-level mapping keys.
//! No function calls or type references.

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting YAML top-level mapping keys as chunks.
///
/// Each top-level `block_mapping_pair` becomes a Property chunk.
const CHUNK_QUERY: &str = r#"
;; Top-level mapping pairs (key: value)
(block_mapping_pair
  key: (flow_node) @name) @property
"#;

/// Doc comment node types — YAML uses `# comments`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "true", "false", "null", "yes", "no", "on", "off",
];

fn extract_return(_signature: &str) -> Option<String> {
    // YAML has no functions or return types
    None
}

/// Post-process YAML chunks: only keep top-level keys (depth 1).
/// Nested keys within mappings are too granular.
fn post_process_yaml(
    _name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Only keep top-level mapping pairs (parent is block_mapping, grandparent is stream/document)
    if let Some(parent) = node.parent() {
        if let Some(grandparent) = parent.parent() {
            let gp_kind = grandparent.kind();
            // Top-level: stream > document > block_node > block_mapping > block_mapping_pair
            // or: stream > block_mapping > block_mapping_pair
            return gp_kind == "stream"
                || gp_kind == "document"
                || grandparent.parent().is_some_and(|ggp| {
                    ggp.kind() == "stream" || ggp.kind() == "document"
                });
        }
    }
    true
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "yaml",
    grammar: Some(|| tree_sitter_yaml::LANGUAGE.into()),
    extensions: &["yaml", "yml"],
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
    post_process_chunk: Some(post_process_yaml),
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
    fn parse_yaml_top_level_keys() {
        let content = r#"name: my-service
version: 1.0.0
dependencies:
  - redis
  - postgres
"#;
        let file = write_temp_file(content, "yaml");
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
    }

    #[test]
    fn parse_yaml_chunk_type() {
        let content = r#"server:
  host: localhost
  port: 8080
"#;
        let file = write_temp_file(content, "yaml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let server = chunks.iter().find(|c| c.name == "server");
        assert!(server.is_some(), "Expected 'server' chunk");
        assert_eq!(server.unwrap().chunk_type, ChunkType::Property);
    }

    #[test]
    fn parse_yaml_no_calls() {
        let content = r#"database:
  host: localhost
  port: 5432
"#;
        let file = write_temp_file(content, "yaml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "YAML should have no calls");
        }
    }

    #[test]
    fn test_extract_return_yaml() {
        assert_eq!(extract_return("key: value"), None);
        assert_eq!(extract_return(""), None);
    }
}
