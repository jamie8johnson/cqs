//! Ruby language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Ruby code chunks
const CHUNK_QUERY: &str = r#"
;; Methods
(method
  name: (identifier) @name) @function

;; Singleton methods (def self.foo)
(singleton_method
  name: (identifier) @name) @function

;; Classes
(class
  name: (constant) @name) @class

;; Modules
(module
  name: (constant) @name) @module

;; Constants (UPPER_CASE = value)
(assignment
  left: (constant) @name) @const
"#;

/// Tree-sitter query for extracting Ruby function calls
const CALL_QUERY: &str = r#"
(call
  method: (identifier) @callee)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "def", "class", "module", "end", "if", "elsif", "else", "unless", "case", "when", "for",
    "while", "until", "do", "begin", "rescue", "ensure", "raise", "return", "yield", "self",
    "super", "true", "false", "nil", "and", "or", "not", "in", "include", "extend", "prepend",
    "require", "private", "protected", "public", "attr_accessor", "attr_reader", "attr_writer",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "ruby",
    grammar: Some(|| tree_sitter_ruby::LANGUAGE.into()),
    extensions: &["rb", "rake", "gemspec"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &["singleton_method"],
    method_containers: &["class", "module"],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/spec/{stem}_spec.rb")),
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["describe ", "it ", "context "],
    test_path_patterns: &["%/spec/%", "%/test/%", "%\\_spec.rb", "%\\_test.rb"],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[
        "to_s", "to_i", "to_f", "to_a", "to_h", "inspect",
        "hash", "eql?", "==", "<=>", "each", "initialize",
    ],
    injections: &[],
    doc_format: "hash_comment",
    doc_convention: "Use YARD format: @param, @return, @raise tags.",
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
    /// Tests that the parser correctly identifies and classifies a Ruby class definition.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded Ruby source code.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts parsing behavior and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a temporary file, parse the file, find a "Calculator" chunk, or if the chunk type is not `ChunkType::Class`.

    #[test]
    fn parse_ruby_class() {
        let content = r#"
class Calculator
  def add(a, b)
    a + b
  end
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }
    /// Parses a Ruby module definition and verifies the parser correctly identifies it as a Module chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to parse the file, if the "Helpers" module chunk is not found in the parsed results, or if the chunk type is not Module.

    #[test]
    fn parse_ruby_module() {
        let content = r#"
module Helpers
  def helper
    42
  end
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let module = chunks.iter().find(|c| c.name == "Helpers").unwrap();
        assert_eq!(module.chunk_type, ChunkType::Module);
    }
    /// Parses a Ruby file containing a standalone method and verifies it is correctly identified as a function chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "standalone_method" chunk is not found, or the chunk type is not `ChunkType::Function`.

    #[test]
    fn parse_ruby_method() {
        let content = r#"
def standalone_method(x)
  x * 2
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "standalone_method").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Verifies that the parser correctly identifies and classifies singleton methods defined on a Ruby class using the `def self.method_name` syntax.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded Ruby source code.
    /// 
    /// # Returns
    /// 
    /// Nothing. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - Temporary file creation fails
    /// - Parser initialization fails
    /// - File parsing fails
    /// - A chunk named "bar" is not found in the parsed results
    /// - The found chunk is not classified as a `ChunkType::Method`

    #[test]
    fn parse_ruby_singleton_method() {
        let content = r#"
class Foo
  def self.bar
    "hello"
  end
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "bar").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }
    /// Parses a Ruby class containing a method and verifies the method chunk is correctly identified with its parent class.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. Asserts that a method named "add" is parsed as a Method chunk type with parent class "Calculator".
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "add" method chunk is not found, or any assertion fails.

    #[test]
    fn parse_ruby_method_in_class() {
        let content = r#"
class Calculator
  def add(a, b)
    a + b
  end
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }
    /// Parses a Ruby method defined within a module and verifies it is correctly identified with its parent module name.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. Uses assertions to verify parsing behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - Temporary file creation fails
    /// - Parser initialization fails
    /// - File parsing fails
    /// - The expected method "capitalize_all" is not found in parsed chunks
    /// - Assertions about chunk type or parent module name fail

    #[test]
    fn parse_ruby_method_in_module() {
        let content = r#"
module StringUtils
  def capitalize_all(str)
    str.upcase
  end
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "capitalize_all").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("StringUtils"));
    }
    /// Parses a Ruby constant definition and verifies it is correctly identified as a Constant chunk type.
    /// 
    /// This is a test function that creates a temporary Ruby file containing a constant assignment, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts on the parse results and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the file cannot be parsed, the "MAX_RETRIES" chunk is not found in the parse results, or the chunk type is not `ChunkType::Constant`.

    #[test]
    fn parse_ruby_constant() {
        let content = "MAX_RETRIES = 3\n";
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let c = chunks.iter().find(|c| c.name == "MAX_RETRIES").unwrap();
        assert_eq!(c.chunk_type, ChunkType::Constant);
    }
    /// Parses a Ruby function and extracts method calls from its body to verify that specific function calls are properly identified.
    /// 
    /// This test function creates a temporary Ruby file containing a `process` function that calls `transform` and `puts` methods, parses the file, locates the `process` function chunk, extracts all method calls from it, and verifies that both expected calls are present in the extracted results.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded Ruby source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and will panic if the expected method calls are not found.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser initialization fails, the file parsing fails, the `process` function chunk is not found, or if either the `transform` or `puts` method calls are not extracted from the function.

    #[test]
    fn parse_ruby_calls() {
        let content = r#"
def process(input)
  result = transform(input)
  result.to_s
  puts(result)
end
"#;
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"transform"), "Expected transform call, got: {:?}", names);
        assert!(names.contains(&"puts"), "Expected puts call, got: {:?}", names);
    }
}
