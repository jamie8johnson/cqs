//! Nix language definition
//!
//! Nix is a functional package-management language. Chunks are attribute bindings
//! (functions, attribute sets). Call graph via `apply_expression`.

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Nix definitions as chunks.
///
/// Captures:
/// - Function bindings: `name = args: body;`
/// - Attribute set bindings: `name = { ... };` and `name = rec { ... };`
/// - Let-in function bindings (top-level)
const CHUNK_QUERY: &str = r#"
;; Attribute binding whose value is a function
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (function_expression)) @function

;; Attribute binding whose value is an attribute set
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (attrset_expression)) @struct

;; Attribute binding whose value is a recursive attribute set
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (rec_attrset_expression)) @struct

;; Attribute binding whose value is a function application (e.g., mkDerivation { ... })
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (apply_expression)) @function
"#;

/// Tree-sitter query for extracting function calls (applications).
///
/// Nix uses juxtaposition for function application: `f x` is `apply_expression`.
const CALL_QUERY: &str = r#"
;; Direct function application: `foo arg`
(apply_expression
  function: (variable_expression
    name: (identifier) @callee))

;; Qualified function application: `lib.mkDerivation arg`
(apply_expression
  function: (select_expression
    attrpath: (attrpath) @callee))
"#;

/// Doc comment node types — Nix uses `# comments` and `/* block comments */`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "true", "false", "null", "if", "then", "else", "let", "in", "with", "rec", "inherit",
    "import", "assert", "builtins", "throw", "abort",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "nix",
    grammar: Some(|| tree_sitter_nix::LANGUAGE.into()),
    extensions: &["nix"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
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
    fn parse_nix_function_binding() {
        let content = r#"
{
  mkHello = name:
    "Hello, ${name}!";
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"mkHello"),
            "Expected 'mkHello', got: {:?}",
            names
        );
        let func = chunks.iter().find(|c| c.name == "mkHello").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_nix_attrset_binding() {
        let content = r#"
{
  config = {
    enableFeature = true;
    port = 8080;
  };
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"config"),
            "Expected 'config', got: {:?}",
            names
        );
        let cfg = chunks.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(cfg.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_nix_calls() {
        let content = r#"
{
  myPackage = mkDerivation {
    name = "hello";
    buildInputs = [ pkgs.gcc ];
  };

  greet = name:
    builtins.trace "greeting" (lib.concatStrings ["Hello, " name]);
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // mkDerivation is called in myPackage binding
        let pkg = chunks.iter().find(|c| c.name == "myPackage");
        assert!(pkg.is_some(), "Expected 'myPackage' chunk");

        // Check calls in greet
        let greet = chunks.iter().find(|c| c.name == "greet");
        if let Some(g) = greet {
            let calls = parser.extract_calls_from_chunk(g);
            let callee_names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
            // Should find builtins.trace or lib.concatStrings as qualified calls
            assert!(
                !callee_names.is_empty(),
                "Expected some calls in greet function"
            );
        }
    }

    #[test]
    fn parse_nix_rec_attrset() {
        let content = r#"
{
  helpers = rec {
    double = x: x * 2;
    quadruple = x: double (double x);
  };
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let helpers = chunks.iter().find(|c| c.name == "helpers");
        assert!(helpers.is_some(), "Expected 'helpers' chunk");
        assert_eq!(helpers.unwrap().chunk_type, ChunkType::Struct);
    }
}
