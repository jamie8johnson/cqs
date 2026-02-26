//! Markdown language definition

use super::{LanguageDef, SignatureStyle};

/// Prose stopwords for keyword extraction — more extensive than code language stopwords
/// since markdown content is natural language.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "will", "can", "has",
    "have", "been", "being", "also", "such", "each", "when", "which", "would", "about", "into",
    "over", "after", "before", "more", "than", "then", "only", "very", "just", "may", "must",
    "should", "could", "does", "did", "had", "not", "but", "all", "any", "both", "its", "our",
    "their", "there", "here", "where", "what", "how", "who", "see", "use", "used", "using",
    "following", "example", "note", "important", "below", "above", "refer", "section", "page",
    "chapter", "figure", "table",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "markdown",
    grammar: None, // No tree-sitter — custom line-by-line heading parser
    extensions: &["md", "mdx"],
    chunk_query: "",
    call_query: None,
    signature_style: SignatureStyle::Breadcrumb,
    doc_nodes: &[],
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
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
