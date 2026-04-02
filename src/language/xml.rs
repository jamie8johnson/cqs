//! XML language definition
//!
//! XML is a markup language for structured data. Chunks are top-level elements.
//! Uses `LANGUAGE_XML` (non-standard export, like OCaml's `LANGUAGE_OCAML`).

use super::{ChunkType, FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting XML elements.
/// XML grammar uses capitalized node kinds per the XML spec:
///   - `STag` = start tag, `ETag` = end tag, `EmptyElemTag` = self-closing
///   - `Name` = element/attribute name
const CHUNK_QUERY: &str = r#"
;; Elements with start tag
(element
  (STag
    (Name) @name)) @struct

;; Self-closing elements
(element
  (EmptyElemTag
    (Name) @name)) @struct

;; Processing instructions (<?xml-stylesheet ... ?>)
(PI
  (PITarget) @name) @function
"#;

/// Doc comment node types — XML uses `<!-- ... -->` comments
const DOC_NODES: &[&str] = &["Comment"];

const STOPWORDS: &[&str] = &[
    "xml", "xmlns", "version", "encoding", "standalone", "xsi", "xsd", "type", "name", "value",
];

/// Extracts the return type from a function signature.
/// # Arguments
/// * `_signature` - A string slice containing a function signature (unused for XML)
/// # Returns
/// Returns `None` as XML has no concept of functions or return types.
fn extract_return(_signature: &str) -> Option<String> {
    // XML has no functions or return types
    None
}

/// Post-process XML chunks: only keep top-level elements (direct children of root).
fn post_process_xml(
    _name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Processing instructions are always kept
    if node.kind() == "PI" {
        return true;
    }
    // element > document (depth 1) or element > content > element > document (depth 2)
    if let Some(parent) = node.parent() {
        let pk = parent.kind();
        if pk == "document" {
            return true;
        }
        // Depth 2: element inside root element's content
        if pk == "content" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "element" {
                    if let Some(ggp) = grandparent.parent() {
                        return ggp.kind() == "document";
                    }
                }
            }
        }
    }
    false
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "xml",
    grammar: Some(|| tree_sitter_xml::LANGUAGE_XML.into()),
    extensions: &["xml", "xsl", "xslt", "xsd", "svg", "wsdl", "rss", "plist", "l5x"],
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
    post_process_chunk: Some(post_process_xml),
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
    fn parse_xml_root_elements() {
        let content = r#"<?xml version="1.0"?>
<catalog>
  <book>
    <title>Rust Programming</title>
    <author>Steve</author>
  </book>
  <book>
    <title>The C Language</title>
    <author>K&amp;R</author>
  </book>
</catalog>
"#;
        let file = write_temp_file(content, "xml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        // catalog is root, book is depth 2 — both should appear
        assert!(
            names.contains(&"catalog"),
            "Expected 'catalog', got: {:?}",
            names
        );
        assert!(
            names.contains(&"book"),
            "Expected 'book' at depth 2, got: {:?}",
            names
        );
        // title/author are depth 3 — should be filtered
        assert!(
            !names.contains(&"title"),
            "Deep 'title' should be filtered, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_xml_element_type() {
        let content = r#"<root><item/></root>"#;
        let file = write_temp_file(content, "xml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let root = chunks.iter().find(|c| c.name == "root");
        assert!(root.is_some(), "Expected 'root' element");
        assert_eq!(root.unwrap().chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_xml_no_calls() {
        let content = r#"<root><child/></root>"#;
        let file = write_temp_file(content, "xml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "XML should have no calls");
        }
    }

    #[test]
    fn test_extract_return_xml() {
        assert_eq!(extract_return("<element/>"), None);
        assert_eq!(extract_return(""), None);
    }
}
