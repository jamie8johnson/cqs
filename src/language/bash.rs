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
    test_markers: &[],
    test_path_patterns: &["%/tests/%", "%\\_test.sh", "%.bats"],
    structural_matchers: None,
    entry_point_names: &["main"],
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
    /// Parses a bash function definition and verifies the parser correctly identifies it as a function chunk.
    /// 
    /// This test function creates a temporary bash file containing a function definition, parses it using the Parser, and asserts that the resulting chunk has the correct name ("foo") and type (Function).
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following assertions fail:
    /// - The parsed chunks list contains exactly one element
    /// - The chunk name is "foo"
    /// - The chunk type is ChunkType::Function

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
    /// Parses a short bash function definition and verifies the parser correctly identifies it.
    /// 
    /// This is a unit test that validates the parser's ability to extract and classify a simple bash function. It creates a temporary shell file containing a basic function definition, parses it, and asserts that exactly one chunk is returned with the correct name ("foo") and type (Function).
    /// 
    /// # Arguments
    /// 
    /// None - this is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None - this function returns `()` and is used for assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if any assertion fails, indicating the parser did not correctly identify the bash function name, type, or chunk count.

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
    /// Parses a Bash script containing a function with multiple command calls and verifies that the parser correctly extracts all invoked commands (echo, grep, run_tests) from the function chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded Bash script content.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to verify parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following conditions fail:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize
    /// - The parser fails to parse the file
    /// - The "deploy" function chunk is not found
    /// - The extracted calls do not include "echo", "grep", or "run_tests" commands

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
    /// Verifies that the parser correctly identifies and extracts a multi-line Bash function definition from a shell script file.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if any assertion fails, including:
    /// - If the parser does not extract exactly one chunk from the file
    /// - If the extracted chunk name is not "setup_env"
    /// - If the chunk content does not contain "local env_name"
    /// - If file creation or parsing operations fail

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
    /// Verifies that the parser correctly identifies nested function calls within a bash function definition. Creates a temporary bash file containing a `build` function that calls `compile_sources`, `run_tests`, and `package_artifacts`, parses it, and asserts that the extracted calls match the expected function names.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to initialize or parse the file, if the expected number of chunks is not found, if the `build` function chunk is not found, or if the expected function calls are not present in the extracted calls.

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
    /// Verifies that the parser does not extract any code chunks from a Bash script containing only standalone commands and comments outside of any function definitions.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters. It creates a temporary Bash script file containing standalone echo and ls commands along with comments.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. This is a test function that verifies parser behavior through assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser extracts any chunks from the script, indicating incorrect behavior in treating bare commands as non-extractable code chunks.

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
    /// Verifies that the parser correctly extracts doc comments from Bash functions.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// Returns nothing (`()`). This is a test assertion function.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser initialization fails
    /// - The file cannot be parsed
    /// - A function named "deploy" is not found in the parsed chunks
    /// - The doc comment does not contain the text "Deploy"

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
