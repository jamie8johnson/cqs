//! Bash/Shell language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Bash function definitions
const CHUNK_QUERY: &str = r#"
;; Function definitions (both `function foo() {}` and `foo() {}` syntaxes)
(function_definition
  name: (word) @name) @function
"#;

/// Tree-sitter query for extracting command invocations
const CALL_QUERY: &str = r#"
;; Command invocations (function calls, builtins, externals)
(command
  name: (command_name) @callee)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "do", "done", "while", "until", "case", "esac",
    "in", "function", "return", "exit", "export", "local", "declare", "readonly", "unset", "shift",
    "set", "eval", "exec", "source", "true", "false", "echo", "printf", "read", "test",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "bash",
    grammar: Some(|| tree_sitter_bash::LANGUAGE.into()),
    extensions: &["sh", "bash"],
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
    fn parse_bash_function() {
        let content = r#"
function foo() {
    echo "hello"
}
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "foo");
        assert_eq!(chunks[0].chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_bash_function_short() {
        let content = r#"
foo() {
    echo "hello"
}
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "foo");
        assert_eq!(chunks[0].chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_bash_calls() {
        let content = r#"
function deploy() {
    echo "deploying..."
    grep -r "TODO" src/
    run_tests
}
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "deploy").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"echo"), "Expected echo, got: {:?}", names);
        assert!(names.contains(&"grep"), "Expected grep, got: {:?}", names);
        assert!(
            names.contains(&"run_tests"),
            "Expected run_tests, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_bash_multiline() {
        let content = r#"
function setup_env() {
    local env_name="$1"
    if [ -z "$env_name" ]; then
        echo "Usage: setup_env <name>"
        return 1
    fi
    export ENV_NAME="$env_name"
    echo "Environment set to $env_name"
}
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].name, "setup_env");
        assert!(chunks[0].content.contains("local env_name"));
    }

    #[test]
    fn parse_bash_nested_calls() {
        let content = r#"
function build() {
    compile_sources
    run_tests
    package_artifacts
}

function compile_sources() {
    gcc -o main main.c
}
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert_eq!(chunks.len(), 2);
        let build = chunks.iter().find(|c| c.name == "build").unwrap();
        let calls = parser.extract_calls_from_chunk(build);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"compile_sources"),
            "Expected compile_sources, got: {:?}",
            names
        );
        assert!(
            names.contains(&"run_tests"),
            "Expected run_tests, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_bash_no_chunks_outside_function() {
        let content = r#"
#!/bin/bash
echo "standalone command"
ls -la
# This is a comment
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        assert!(
            chunks.is_empty(),
            "Expected no chunks for bare commands, got: {:?}",
            chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_bash_doc_comment() {
        let content = r#"
# Deploy the application to production
function deploy() {
    echo "deploying"
}
"#;
        let file = write_temp_file(content, "sh");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "deploy").unwrap();
        assert!(
            func.doc.as_ref().map_or(false, |d| d.contains("Deploy")),
            "Expected doc comment, got: {:?}",
            func.doc
        );
    }
}
