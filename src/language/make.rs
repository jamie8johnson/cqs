//! Makefile language definition
//!
//! Make is a build automation tool. Chunks are rules (targets) and variable
//! assignments. No call graph — prerequisite references are structural, not
//! function calls.

use super::{InjectionRule, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Make definitions as chunks.
///
/// Captures:
/// - Rules: targets with recipes
/// - Variable assignments: `VAR = value` / `VAR := value`
const CHUNK_QUERY: &str = r#"
;; Make rules (targets)
(rule
  (targets (word) @name)) @function

;; Variable assignments
(variable_assignment
  name: (word) @name) @property
"#;

/// Doc comment node types — Makefiles use `# comments`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "all", "clean", "install", "uninstall", "dist", "distclean", "check", "test",
    "phony", "default", "ifdef", "ifndef", "ifeq", "ifneq", "else", "endif",
    "include", "override", "export", "unexport", "define", "endef",
    "wildcard", "patsubst", "subst", "filter", "sort", "word", "words",
    "foreach", "call", "eval", "origin", "shell", "info", "warning", "error",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "make",
    grammar: Some(|| tree_sitter_make::LANGUAGE.into()),
    extensions: &["mk", "mak"],
    chunk_query: CHUNK_QUERY,
    call_query: None,
    signature_style: SignatureStyle::FirstLine,
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
    entry_point_names: &["all", "default"],
    trait_method_names: &[],
    injections: &[
        InjectionRule {
            container_kind: "recipe",
            content_kind: "shell_text",
            target_language: "bash",
            detect_language: None,
            content_scoped_lines: false,
        },
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
    fn parse_make_rule() {
        let content = r#"
all: build test
	echo "Done"

build: src/main.c
	gcc -o main src/main.c

test: build
	./run_tests
"#;
        let file = write_temp_file(content, "mk");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"all"),
            "Expected 'all' rule, got: {:?}",
            names
        );
        assert!(
            names.contains(&"build"),
            "Expected 'build' rule, got: {:?}",
            names
        );
        assert!(
            names.contains(&"test"),
            "Expected 'test' rule, got: {:?}",
            names
        );
        let build = chunks.iter().find(|c| c.name == "build").unwrap();
        assert_eq!(build.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_make_variable() {
        let content = r#"
CC = gcc
CFLAGS = -Wall -Werror
SRC = $(wildcard src/*.c)

all: $(SRC)
	$(CC) $(CFLAGS) -o main $(SRC)
"#;
        let file = write_temp_file(content, "mk");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"CC"),
            "Expected 'CC' variable, got: {:?}",
            names
        );
        assert!(
            names.contains(&"CFLAGS"),
            "Expected 'CFLAGS' variable, got: {:?}",
            names
        );
        let cc = chunks.iter().find(|c| c.name == "CC").unwrap();
        assert_eq!(cc.chunk_type, ChunkType::Property);
    }

    #[test]
    fn parse_make_no_calls() {
        let content = r#"
clean:
	rm -rf build/
"#;
        let file = write_temp_file(content, "mk");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "Make should have no call graph");
        }
    }

    #[test]
    fn parse_make_bash_injection() {
        let content = "setup:\n\tmy_helper() { \\\n\t\techo \"setting up\"; \\\n\t}; \\\n\tmy_helper\n";
        let file = write_temp_file(content, "mk");
        let parser = Parser::new().unwrap();
        let (chunks, _calls, _types) = parser.parse_file_all(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"setup"), "Expected Make 'setup' rule, got: {:?}", names);
        // Bash injection may extract function if grammar can parse line-continued shell
    }

    #[test]
    fn parse_make_pattern_rule() {
        let content = r#"
%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

install: all
	cp main /usr/local/bin/
"#;
        let file = write_temp_file(content, "mk");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"install"),
            "Expected 'install' rule, got: {:?}",
            names
        );
    }
}
