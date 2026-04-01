//! Nix language definition
//!
//! Nix is a functional package-management language. Chunks are attribute bindings
//! (functions, attribute sets). Call graph via `apply_expression`.

use super::{FieldStyle, InjectionRule, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Nix definitions as chunks.
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

/// Nix binding names that contain shell scripts.
/// In Nix derivations, these attribute bindings hold shell code:
/// build phases, hooks, and script fields. We only inject bash for
/// indented strings in these contexts to avoid false positives.
const SHELL_CONTEXTS: &[&str] = &[
    "buildPhase",
    "installPhase",
    "configurePhase",
    "checkPhase",
    "unpackPhase",
    "patchPhase",
    "fixupPhase",
    "distPhase",
    "shellHook",
    "preBuild",
    "postBuild",
    "preInstall",
    "postInstall",
    "preCheck",
    "postCheck",
    "preConfigure",
    "postConfigure",
    "preUnpack",
    "postUnpack",
    "prePatch",
    "postPatch",
    "preFixup",
    "postFixup",
    "script",
    "buildCommand",
    "installCommand",
];

/// Detect whether an `indented_string_expression` contains shell code.
/// Walks up from the container node to find the parent `binding` and
/// checks the attribute name against known shell contexts (build phases,
/// hooks, etc.). Returns `None` (use default bash) for shell contexts,
/// `Some("_skip")` for everything else.
fn detect_nix_shell_context(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    // Walk up to find the binding parent
    let parent = match node.parent() {
        Some(p) if p.kind() == "binding" => p,
        _ => {
            tracing::debug!("Nix indented string not in binding context, skipping injection");
            return Some("_skip");
        }
    };

    // Find attrpath child of binding → get last identifier
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if child.kind() == "attrpath" {
            let mut inner_cursor = child.walk();
            let mut last_ident = None;
            for attr_child in child.children(&mut inner_cursor) {
                if attr_child.kind() == "identifier" {
                    last_ident = Some(&source[attr_child.byte_range()]);
                }
            }
            if let Some(ident) = last_ident {
                if SHELL_CONTEXTS.contains(&ident) {
                    tracing::debug!(binding = ident, "Nix shell context detected, injecting bash");
                    return None; // Use default target (bash)
                }
                tracing::debug!(binding = ident, "Nix binding not a shell context, skipping");
                return Some("_skip");
            }
        }
    }

    Some("_skip")
}

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
    injections: &[
        // Indented strings (''...'') in shell-context bindings contain bash.
        // detect_nix_shell_context checks the parent binding's attrpath name
        // against known shell contexts (buildPhase, installPhase, etc.).
        InjectionRule {
            container_kind: "indented_string_expression",
            content_kind: "string_fragment",
            target_language: "bash",
            detect_language: Some(detect_nix_shell_context),
            content_scoped_lines: false,
        },
    ],
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

    // --- Injection tests ---

    #[test]
    fn parse_nix_shell_injection() {
        // buildPhase with bash content should trigger bash injection.
        // The outer binding `hello = mkDerivation { ... }` produces a Nix chunk.
        // The inner buildPhase indented string is injected as bash.
        let content = r#"
{
  hello = mkDerivation {
    name = "hello";
    buildPhase = ''
      mkdir -p build
      gcc -o build/hello src/main.c
    '';
    installPhase = ''
      mkdir -p $out/bin
      cp build/hello $out/bin/
    '';
  };
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Nix binding chunk should still exist
        assert!(
            chunks.iter().any(|c| c.language == crate::parser::Language::Nix),
            "Expected Nix chunks to survive injection, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_nix_non_shell_skipped() {
        // Indented strings NOT in shell contexts should be skipped
        let content = r#"
{
  description = ''
    This is a multi-line description.
    It should not be parsed as bash.
  '';
  longDescription = ''
    Another indented string that is just text,
    not shell code.
  '';
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // No bash chunks should be extracted
        let bash_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Bash)
            .collect();
        assert!(
            bash_chunks.is_empty(),
            "Non-shell indented strings should NOT produce bash chunks, got: {:?}",
            bash_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_nix_without_strings_unchanged() {
        // Nix file with no indented strings — injection should not fire
        let content = r#"
{
  add = a: b: a + b;
  config = {
    port = 8080;
  };
}
"#;
        let file = write_temp_file(content, "nix");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // All chunks should be Nix
        for chunk in &chunks {
            assert_eq!(
                chunk.language,
                crate::parser::Language::Nix,
                "File without indented strings should only have Nix chunks"
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
