//! Perl language definition

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Perl code chunks.
///
/// Perl constructs:
///   - `function_definition` → Function (sub name { ... })
///   - `package_statement` → Module (package Foo; or package Foo { ... })
const CHUNK_QUERY: &str = r#"
;; Subroutine definition: sub add { ... }
(function_definition
  name: (identifier) @name) @function

;; Package declaration: package MyModule;
(package_statement) @struct
"#;

/// Tree-sitter query for extracting Perl calls.
///
/// Perl uses several call forms:
///   - `call_expression_with_bareword` for direct calls: foo(args)
///   - `method_invocation` for method calls: $obj->method(args)
const CALL_QUERY: &str = r#"
;; Direct function call: foo(args)
(call_expression_with_bareword
  function_name: (identifier) @callee)

;; Method call: $obj->method(args) or Package->method(args)
(method_invocation
  function_name: (identifier) @callee)
"#;

/// Doc comment node types — Perl uses # for single-line comments
/// and POD (=head1 etc.) for documentation
const DOC_NODES: &[&str] = &["comments", "pod"];

const STOPWORDS: &[&str] = &[
    "sub", "my", "our", "local", "use", "require", "package", "return", "if", "elsif", "else",
    "unless", "while", "until", "for", "foreach", "do", "eval", "die", "warn", "print", "say",
    "chomp", "chop", "push", "pop", "shift", "unshift", "splice", "join", "split", "map", "grep",
    "sort", "keys", "values", "each", "exists", "delete", "defined", "ref", "bless", "new",
    "BEGIN", "END", "AUTOLOAD", "DESTROY", "open", "close", "read", "write", "seek", "tell",
    "Carp", "Exporter", "Scalar", "List", "File", "IO", "POSIX", "Data", "Dumper", "strict",
    "warnings", "utf8", "Encode", "Getopt", "Test", "More",
];

/// Post-process Perl chunks to set correct chunk types.
fn post_process_perl(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "function_definition" => *chunk_type = ChunkType::Function,
        "package_statement" => {
            *chunk_type = ChunkType::Module;
            // Extract package name from text: "package Foo::Bar;"
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            let text = text.trim();
            if let Some(rest) = text.strip_prefix("package") {
                let rest = rest.trim();
                // Take until ; or { or whitespace
                let pkg_name: String = rest
                    .chars()
                    .take_while(|c| *c != ';' && *c != '{' && !c.is_whitespace())
                    .collect();
                if !pkg_name.is_empty() {
                    *name = pkg_name;
                }
            }
        }
        _ => {}
    }
    true
}

/// Extract return type from Perl signatures.
///
/// Perl doesn't have static return types, so this always returns None.
fn extract_return(_signature: &str) -> Option<String> {
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "perl",
    grammar: Some(|| tree_sitter_perl::LANGUAGE.into()),
    extensions: &["pl", "pm"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("t/{stem}.t")),
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_perl as PostProcessChunkFn),
    test_markers: &[],
    test_path_patterns: &["%/t/%", "%.t"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "new", "AUTOLOAD", "DESTROY", "import", "BEGIN", "END",
    ],
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
    fn parse_perl_subroutine() {
        let content = r#"
sub add {
    my ($a, $b) = @_;
    return $a + $b;
}
"#;
        let file = write_temp_file(content, "pl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_perl_package() {
        let content = r#"
package Calculator;

sub add {
    my ($self, $a, $b) = @_;
    return $a + $b;
}

1;
"#;
        let file = write_temp_file(content, "pm");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let pkg = chunks
            .iter()
            .find(|c| c.name == "Calculator" && c.chunk_type == ChunkType::Module);
        assert!(pkg.is_some(), "Should find 'Calculator' package as Module");
    }

    #[test]
    fn parse_perl_calls() {
        let content = r#"
sub process {
    my ($data) = @_;
    my $result = transform($data);
    validate($result);
    return $result;
}
"#;
        let file = write_temp_file(content, "pl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"transform"),
            "Expected transform, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_return_perl() {
        assert_eq!(extract_return("sub add {"), None);
        assert_eq!(extract_return(""), None);
    }
}
