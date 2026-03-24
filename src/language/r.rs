//! R language definition

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Returns true if the name follows UPPER_CASE convention (all ASCII uppercase/digits/underscores,
/// at least one letter, e.g. MAX_RETRIES, API_URL_V2).
fn is_upper_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.bytes().any(|b| b.is_ascii_uppercase())
}

/// Tree-sitter query for extracting R code chunks.
///
/// R has no named function definitions — functions are assigned to variables:
///   `my_func <- function(x) { ... }`
///   `my_func = function(x) { ... }`
///
/// We also capture:
///   - Non-function assignments for constants (UPPER_CASE) and R6 classes
///   - Top-level `call` nodes for S4 class definitions (setClass)
const CHUNK_QUERY: &str = r#"
;; Function assignment: name <- function(...) or name = function(...)
(binary_operator
  lhs: (identifier) @name
  rhs: (function_definition)) @function

;; Non-function assignment: name <- expr (for constants and R6 classes)
;; post_process distinguishes R6Class calls (→ Class) from UPPER_CASE constants (→ Constant)
(binary_operator
  lhs: (identifier) @name
  rhs: (call)) @const

;; Scalar/literal assignment for constants: name <- 10, name <- "str", name <- TRUE
(binary_operator
  lhs: (identifier) @name
  rhs: (float)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (string)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (true)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (false)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (null)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (inf)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (nan)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (na)) @const

;; Negative literal: name <- -1
(binary_operator
  lhs: (identifier) @name
  rhs: (unary_operator)) @const

;; S4 class definition: setClass("ClassName", ...)
;; The actual class name is extracted from the first string argument in post_process.
(call
  function: (identifier) @name
  arguments: (arguments
    (argument
      (string)))) @class
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

/// Extracts the return type from an R function signature.
///
/// Returns `None` — R functions do not have type annotations in their signatures.
fn extract_return(_signature: &str) -> Option<String> {
    None
}

/// Returns true if the node is nested inside a function body.
fn is_inside_function(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        if parent.kind() == "function_definition" {
            return true;
        }
        cursor = parent.parent();
    }
    false
}

/// Extract the first string argument from a `call` node's arguments.
/// For `setClass("Person", ...)` returns `Some("Person")`.
fn first_string_arg<'a>(node: tree_sitter::Node, source: &'a str) -> Option<&'a str> {
    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "argument" {
            // Look for the string inside the argument
            let mut inner = child.walk();
            if inner.goto_first_child() {
                loop {
                    let ic = inner.node();
                    if ic.kind() == "string" {
                        // Extract string_content child
                        let mut sc = ic.walk();
                        if sc.goto_first_child() {
                            loop {
                                if sc.node().kind() == "string_content" {
                                    return Some(&source[sc.node().byte_range()]);
                                }
                                if !sc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                        return None;
                    }
                    if !inner.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    None
}

/// Check if a `call` node's function identifier matches the given name.
fn call_function_name<'a>(node: tree_sitter::Node, source: &'a str) -> Option<&'a str> {
    let func = node.child_by_field_name("function")?;
    if func.kind() == "identifier" {
        return Some(&source[func.byte_range()]);
    }
    // Also handle namespaced: R6::R6Class
    if func.kind() == "namespace_operator" {
        let mut cursor = func.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.is_named() && child.kind() == "identifier" {
                    // Take the last identifier (rhs of ::)
                    let text = &source[child.byte_range()];
                    // Keep going to find the rhs
                    if !cursor.goto_next_sibling() {
                        return Some(text);
                    }
                    // Skip :: operator
                    loop {
                        let next = cursor.node();
                        if next.is_named() && next.kind() == "identifier" {
                            return Some(&source[next.byte_range()]);
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                    return Some(text);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    None
}

/// S4 class-defining functions whose first string argument is a class name.
const S4_CLASS_FUNCTIONS: &[&str] = &["setClass", "setRefClass"];

/// Post-process R chunks:
/// - `@class` (call nodes): keep only S4 class-defining calls, extract class name
/// - `@const` (binary_operator): detect R6Class → Class, else keep only UPPER_CASE constants
/// - `@function`: pass through unchanged
#[allow(clippy::ptr_arg)]
fn post_process_r(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match *chunk_type {
        ChunkType::Class => {
            // This is a top-level `call` node captured by @class.
            // Only keep S4 class-defining calls; extract class name from first string arg.
            if !S4_CLASS_FUNCTIONS.contains(&name.as_str()) {
                return false;
            }
            if let Some(class_name) = first_string_arg(node, source) {
                *name = class_name.to_string();
                true
            } else {
                // Can't extract class name — discard
                false
            }
        }
        ChunkType::Constant => {
            // This is a binary_operator with non-function rhs.
            // Could be R6Class assignment or a constant.
            if is_inside_function(node) {
                return false;
            }

            // Check if rhs is a call to R6Class
            let rhs = node.child_by_field_name("rhs");
            if let Some(rhs_node) = rhs {
                if rhs_node.kind() == "call" {
                    if let Some(fn_name) = call_function_name(rhs_node, source) {
                        if fn_name == "R6Class" {
                            *chunk_type = ChunkType::Class;
                            return true;
                        }
                    }
                }
            }

            // Not R6 — only keep UPPER_CASE constants
            is_upper_snake_case(name)
        }
        _ => true,
    }
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
    post_process_chunk: Some(post_process_r as PostProcessChunkFn),
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
    fn parse_r_function_equals() {
        let content = r#"
greet = function(name) {
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

    // --- S4 class tests ---

    #[test]
    fn parse_r_s4_set_class() {
        let content = r#"
setClass("Person",
  representation(
    name = "character",
    age = "numeric"
  )
)
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Person");
        assert!(class.is_some(), "Should capture S4 class 'Person'");
        assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_r_s4_set_ref_class() {
        let content = r#"
setRefClass("Counter",
  fields = list(
    count = "numeric"
  ),
  methods = list(
    increment = function() {
      count <<- count + 1
    }
  )
)
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Counter");
        assert!(class.is_some(), "Should capture S4 reference class 'Counter'");
        assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_r_non_class_call_filtered() {
        // A non-class-defining call like library() should not be captured
        let content = r#"
library(ggplot2)
print("hello")
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert!(
            chunks.is_empty(),
            "Non-class calls should be filtered out, got: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    // --- R6 class tests ---

    #[test]
    fn parse_r_r6_class() {
        let content = r#"
Person <- R6Class("Person",
  public = list(
    name = NULL,
    initialize = function(name) {
      self$name <- name
    },
    greet = function() {
      cat(paste0("Hello, my name is ", self$name, ".\n"))
    }
  )
)
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Person");
        assert!(class.is_some(), "Should capture R6 class 'Person'");
        assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_r_r6_class_equals() {
        let content = r#"
Animal = R6Class("Animal",
  public = list(
    species = NULL,
    initialize = function(species) {
      self$species <- species
    }
  )
)
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Animal");
        assert!(class.is_some(), "Should capture R6 class with = assignment");
        assert_eq!(class.unwrap().chunk_type, ChunkType::Class);
    }

    // --- Constant tests ---

    #[test]
    fn parse_r_upper_case_constants() {
        let content = r#"
MAX_RETRIES <- 3
API_URL <- "https://example.com"
DEFAULT_TIMEOUT <- 30
lowercase_var <- 42
MixedCase <- "nope"

my_func <- function(x) { x }
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
        assert!(max.is_some(), "Should capture MAX_RETRIES");
        assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);

        let url = chunks.iter().find(|c| c.name == "API_URL");
        assert!(url.is_some(), "Should capture API_URL");
        assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);

        let timeout = chunks.iter().find(|c| c.name == "DEFAULT_TIMEOUT");
        assert!(timeout.is_some(), "Should capture DEFAULT_TIMEOUT");
        assert_eq!(timeout.unwrap().chunk_type, ChunkType::Constant);

        // lowercase and MixedCase should be filtered out
        assert!(
            chunks.iter().find(|c| c.name == "lowercase_var").is_none(),
            "Should not capture lowercase_var"
        );
        assert!(
            chunks.iter().find(|c| c.name == "MixedCase").is_none(),
            "Should not capture MixedCase"
        );

        // Function should still be captured
        let func = chunks.iter().find(|c| c.name == "my_func");
        assert!(func.is_some(), "Should still capture functions");
        assert_eq!(func.unwrap().chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_r_constants_with_equals() {
        let content = r#"
MAX_VAL = 100
API_KEY = "secret"
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let max = chunks.iter().find(|c| c.name == "MAX_VAL");
        assert!(max.is_some(), "Should capture MAX_VAL with = assignment");
        assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_r_constants_inside_function_ignored() {
        let content = r#"
my_func <- function() {
    MAX_LOCAL <- 99
    result <- MAX_LOCAL + 1
    return(result)
}
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Only the function should be captured, not the local constant
        assert_eq!(chunks.len(), 1, "Should only capture the function");
        assert_eq!(chunks[0].name, "my_func");
        assert_eq!(chunks[0].chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_r_boolean_and_special_constants() {
        let content = r#"
USE_CACHE <- TRUE
EMPTY_VAL <- NULL
MISSING <- NA
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let cache = chunks.iter().find(|c| c.name == "USE_CACHE");
        assert!(cache.is_some(), "Should capture TRUE constant");
        assert_eq!(cache.unwrap().chunk_type, ChunkType::Constant);

        let empty = chunks.iter().find(|c| c.name == "EMPTY_VAL");
        assert!(empty.is_some(), "Should capture NULL constant");
        assert_eq!(empty.unwrap().chunk_type, ChunkType::Constant);

        let missing = chunks.iter().find(|c| c.name == "MISSING");
        assert!(missing.is_some(), "Should capture NA constant");
        assert_eq!(missing.unwrap().chunk_type, ChunkType::Constant);
    }

    // --- Mixed file test ---

    #[test]
    fn parse_r_mixed_file() {
        let content = r#"
#' @title Person class
setClass("Person",
  representation(name = "character", age = "numeric")
)

Logger <- R6Class("Logger",
  public = list(
    log = function(msg) cat(msg, "\n")
  )
)

MAX_CONNECTIONS <- 100

process <- function(x) {
    x * 2
}
"#;
        let file = write_temp_file(content, "r");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<(&str, ChunkType)> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();

        assert!(
            names.contains(&("Person", ChunkType::Class)),
            "Should have S4 class Person, got: {:?}",
            names
        );
        assert!(
            names.contains(&("Logger", ChunkType::Class)),
            "Should have R6 class Logger, got: {:?}",
            names
        );
        assert!(
            names.contains(&("MAX_CONNECTIONS", ChunkType::Constant)),
            "Should have constant MAX_CONNECTIONS, got: {:?}",
            names
        );
        assert!(
            names.contains(&("process", ChunkType::Function)),
            "Should have function process, got: {:?}",
            names
        );
    }

    #[test]
    fn test_is_upper_snake_case() {
        assert!(is_upper_snake_case("MAX_RETRIES"));
        assert!(is_upper_snake_case("API_URL_V2"));
        assert!(is_upper_snake_case("X"));
        assert!(!is_upper_snake_case("lowercase"));
        assert!(!is_upper_snake_case("MixedCase"));
        assert!(!is_upper_snake_case(""));
        assert!(!is_upper_snake_case("123")); // no letters
    }
}
