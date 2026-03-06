//! R language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting R code chunks.
///
/// R has no named function definitions — functions are assigned to variables:
///   `my_func <- function(x) { ... }`
///   `my_func = function(x) { ... }`
///
/// We match `binary_operator` nodes where the rhs is a `function_definition`.
const CHUNK_QUERY: &str = r#"
;; Function assignment with <- operator
(binary_operator
  lhs: (identifier) @name
  rhs: (function_definition)) @function
"#;

/// Tree-sitter query for extracting R function calls.
const CALL_QUERY: &str = r#"
;; Direct function calls (foo(args))
(call
  function: (identifier) @callee)

;; Namespaced calls (pkg::func())
(call
  function: (namespace_operator
    rhs: (identifier) @callee))
"#;

/// Doc comment node types — R uses `#` comments, roxygen uses `#'`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "if", "else", "for", "in", "while", "repeat", "break", "next", "return",
    "library", "require", "source", "TRUE", "FALSE", "NULL", "NA", "Inf", "NaN", "print",
    "cat", "paste", "paste0", "sprintf", "message", "warning", "stop", "tryCatch",
    "c", "list", "data", "frame", "matrix", "vector", "length", "nrow", "ncol",
];

fn extract_return(_signature: &str) -> Option<String> {
    // R has no type annotations in signatures
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "r",
    grammar: Some(|| tree_sitter_r::LANGUAGE.into()),
    extensions: &["r", "R"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/testthat/test-{stem}.R")),
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["test_that", "expect_"],
    test_path_patterns: &["%/tests/%", "%/testthat/%", "test-%.R", "test_%.R"],
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

    #[test]
    fn parse_r_function_arrow() {
        let content = r#"
greet <- function(name) {
    paste("Hello,", name)
}
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_r_multiple_functions() {
        let content = r#"
add <- function(a, b) {
    a + b
}

multiply <- function(a, b) {
    a * b
}
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert!(chunks.iter().any(|c| c.name == "add"));
        assert!(chunks.iter().any(|c| c.name == "multiply"));
    }

    #[test]
    fn parse_r_calls() {
        let content = r#"
process_data <- function(df) {
    cleaned <- na.omit(df)
    result <- mean(cleaned$value)
    print(result)
    return(result)
}
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process_data").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"print"), "Expected print, got: {:?}", names);
    }

    #[test]
    fn test_extract_return_r() {
        assert_eq!(extract_return("greet <- function(name) {"), None);
        assert_eq!(extract_return(""), None);
    }
}
