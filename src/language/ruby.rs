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

    #[test]
    fn parse_ruby_constant() {
        let content = "MAX_RETRIES = 3\n";
        let file = write_temp_file(content, "rb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let c = chunks.iter().find(|c| c.name == "MAX_RETRIES").unwrap();
        assert_eq!(c.chunk_type, ChunkType::Constant);
    }

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
