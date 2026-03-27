//! Erlang language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Erlang code chunks.
///
/// Erlang top-level forms:
///   - `fun_decl` → Function (with `function_clause` children)
///   - `module_attribute` → Module
///   - `type_alias` / `opaque` → TypeAlias
///   - `record_decl` → Struct
///   - `behaviour_attribute` → Interface
///   - `callback` → Interface
///   - `spec` → skipped (attached to function, not standalone chunk)
const CHUNK_QUERY: &str = r#"
;; Function declaration (the outermost form wrapping function_clause(s))
(fun_decl
  clause: (function_clause
    name: (atom) @name)) @function

;; Module attribute: -module(name).
(module_attribute
  name: (atom) @name) @struct

;; Type alias: -type name(...) :: ...
(type_alias
  name: (type_name
    name: (atom) @name)) @struct

;; Opaque type: -opaque name(...) :: ...
(opaque
  name: (type_name
    name: (atom) @name)) @struct

;; Record declaration: -record(name, {fields}).
(record_decl
  name: (atom) @name) @struct

;; Behaviour attribute: -behaviour(name).
(behaviour_attribute
  name: (atom) @name) @interface

;; Callback: -callback name(Args) -> Ret.
(callback
  fun: (atom) @name) @interface

;; Preprocessor macro: -define(NAME, value).
(pp_define
  lhs: (macro_lhs
    name: (var) @name)) @macro

(pp_define
  lhs: (macro_lhs
    name: (atom) @name)) @macro
"#;

/// Tree-sitter query for extracting Erlang function calls.
const CALL_QUERY: &str = r#"
;; Local function call: foo(args)
(call
  expr: (atom) @callee)

;; Remote function call: module:function(args)
(call
  expr: (remote
    fun: (atom) @callee))
"#;

/// Doc comment node types — Erlang uses %% comments
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "module", "export", "import", "behaviour", "behavior", "callback", "spec", "type", "opaque",
    "record", "define", "ifdef", "ifndef", "endif", "include", "include_lib", "fun", "end",
    "case", "of", "if", "receive", "after", "when", "try", "catch", "throw", "begin", "and",
    "or", "not", "band", "bor", "bxor", "bnot", "bsl", "bsr", "div", "rem", "true", "false",
    "undefined", "ok", "error", "self", "lists", "maps", "io", "gen_server", "gen_statem",
    "supervisor", "application", "ets", "mnesia", "erlang", "string", "binary",
];

/// Post-process Erlang chunks to set correct chunk types.
fn post_process_erlang(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "fun_decl" => *chunk_type = ChunkType::Function,
        "module_attribute" => *chunk_type = ChunkType::Module,
        "type_alias" | "opaque" => *chunk_type = ChunkType::TypeAlias,
        "record_decl" => *chunk_type = ChunkType::Struct,
        "behaviour_attribute" => *chunk_type = ChunkType::Interface,
        "callback" => *chunk_type = ChunkType::Interface,
        "pp_define" => *chunk_type = ChunkType::Macro,
        _ => {}
    }
    true
}

/// Extracts the return type from an Erlang function signature.
/// 
/// # Arguments
/// 
/// * `signature` - A function signature string to parse
/// 
/// # Returns
/// 
/// Returns `None` because Erlang is dynamically typed and function signatures do not include explicit return type annotations.
fn extract_return(_signature: &str) -> Option<String> {
    // Erlang is dynamically typed — no return types in function heads
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "erlang",
    grammar: Some(|| tree_sitter_erlang::LANGUAGE.into()),
    extensions: &["erl", "hrl"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_SUITE.erl")),
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_erlang as PostProcessChunkFn),
    test_markers: &[],
    test_path_patterns: &["%/test/%", "%_SUITE.erl", "%_tests.erl"],
    structural_matchers: None,
    entry_point_names: &[
        "start",
        "start_link",
        "init",
        "handle_call",
        "handle_cast",
        "handle_info",
    ],
    trait_method_names: &[
        "init",
        "handle_call",
        "handle_cast",
        "handle_info",
        "terminate",
        "code_change",
    ],
    injections: &[],
    doc_format: "erlang_edoc",
    doc_convention: "Use EDoc format: @param, @returns, @throws tags.",
    field_style: FieldStyle::None,
    skip_line_prefixes: &["-record"],
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
    /// Parses an Erlang module file and verifies that function chunks are correctly identified.
    /// 
    /// This test function writes a temporary Erlang module file containing a `greet/1` function, parses it using the Parser, and asserts that the resulting chunks include a function chunk with the name "greet" and type `ChunkType::Function`.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (unit type)
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if parsing fails, if no chunk named "greet" is found in the parsed output, or if the found chunk's type is not `ChunkType::Function`.

    #[test]
    fn parse_erlang_function() {
        let content = r#"
-module(mymod).
-export([greet/1]).

greet(Name) ->
    io:format("Hello, ~s~n", [Name]).
"#;
        let file = write_temp_file(content, "erl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Parses an Erlang module file and verifies that the parser correctly identifies the module chunk.
    /// 
    /// This function creates a temporary Erlang file containing a simple calculator module with an `add/2` export, parses it using the `Parser`, and asserts that the resulting chunks contain a module chunk named "calculator".
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that panics on assertion failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the temporary file, or fails to find a chunk with name "calculator" and type `ChunkType::Module`.

    #[test]
    fn parse_erlang_module() {
        let content = r#"
-module(calculator).
-export([add/2]).

add(A, B) -> A + B.
"#;
        let file = write_temp_file(content, "erl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let module = chunks
            .iter()
            .find(|c| c.name == "calculator" && c.chunk_type == ChunkType::Module);
        assert!(module.is_some(), "Should find 'calculator' module");
    }
    /// Parses an Erlang source file containing a record definition and verifies that the parser correctly identifies the record as a struct chunk.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// Returns nothing; this is a test function that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, if file parsing fails, or if the 'state' record/struct chunk is not found in the parsed output.

    #[test]
    fn parse_erlang_record() {
        let content = r#"
-module(mymod).
-record(state, {count = 0, name}).

init() -> #state{count = 0, name = "test"}.
"#;
        let file = write_temp_file(content, "erl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let record = chunks
            .iter()
            .find(|c| c.name == "state" && c.chunk_type == ChunkType::Struct);
        assert!(record.is_some(), "Should find 'state' record/struct");
    }
    /// Parses an Erlang module and verifies that function calls within a chunk are correctly extracted.
    /// 
    /// This integration test writes a temporary Erlang file containing a module with a `process/1` function that calls `helper/1`, then parses the file and extracts function calls from the `process` chunk. It asserts that the `helper` function is correctly identified as a callee.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser cannot be initialized, the file cannot be parsed, the `process` chunk cannot be found, or if the extracted calls do not contain the expected `helper` function call.

    #[test]
    fn parse_erlang_calls() {
        let content = r#"
-module(mymod).
-export([process/1]).

process(Data) ->
    Trimmed = string:trim(Data),
    io:format("~s~n", [Trimmed]),
    helper(Trimmed).

helper(X) -> X.
"#;
        let file = write_temp_file(content, "erl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"helper"),
            "Expected helper, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_erlang_define_macro() {
        let content = r#"
-module(mymod).
-define(MAX_RETRIES, 3).
-define(TIMEOUT, 5000).
"#;
        let file = write_temp_file(content, "erl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let retries = chunks
            .iter()
            .find(|c| c.name == "MAX_RETRIES" && c.chunk_type == ChunkType::Macro);
        assert!(retries.is_some(), "Should find MAX_RETRIES macro, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>());
        let timeout = chunks
            .iter()
            .find(|c| c.name == "TIMEOUT" && c.chunk_type == ChunkType::Macro);
        assert!(timeout.is_some(), "Should find TIMEOUT macro");
    }

    #[test]
    fn test_extract_return_erlang() {
        assert_eq!(extract_return("greet(Name) ->"), None);
        assert_eq!(extract_return(""), None);
    }
}
