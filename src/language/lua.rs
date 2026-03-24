//! Lua language definition

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

/// Returns true if the node is nested inside a function body.
fn is_inside_function(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        match parent.kind() {
            "function_declaration" | "function_definition" => return true,
            _ => {}
        }
        cursor = parent.parent();
    }
    false
}

/// Post-process Lua chunks: only keep `@const` captures whose name is UPPER_CASE
/// and that are at module level (not inside function bodies). Also skip assignments
/// whose RHS is a function_definition (already captured as Function), and deduplicate
/// assignment_statement nodes that are already captured via their parent variable_declaration.
#[allow(clippy::ptr_arg)] // signature must match PostProcessChunkFn type alias
fn post_process_lua(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        // Deduplicate: if this assignment_statement is inside a variable_declaration,
        // skip it — the variable_declaration match already captures the same constant.
        if node.kind() == "assignment_statement" {
            if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declaration" {
                    return false;
                }
            }
        }
        // Skip constants inside function bodies — only capture module-level
        if is_inside_function(node) {
            return false;
        }
        // Skip if RHS is a function_definition (already captured as Function)
        if has_function_value(node) {
            return false;
        }
        return is_upper_snake_case(name);
    }
    true
}

/// Check if any value in the assignment is a function_definition.
fn has_function_value(node: tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "expression_list" || child.kind() == "assignment_statement" {
            // Recurse into expression_list or nested assignment_statement
            if has_function_value(child) {
                return true;
            }
        }
        if child.kind() == "function_definition" {
            return true;
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    false
}

/// Tree-sitter query for extracting Lua code chunks.
///
/// Functions → Function (both `function foo()` and local function forms).
/// Method-style declarations via `method_index_expression` name field
/// are captured as functions and reclassified to Method via method_containers.
/// Constants → Constant (UPPER_CASE convention, filtered via post_process).
const CHUNK_QUERY: &str = r#"
;; Named function declarations (function foo() / function mod.foo() / function mod:bar())
(function_declaration
  name: (_) @name) @function

;; Local variable assignments (local MAX_SIZE = 100)
;; Filtered to UPPER_CASE by post_process_lua
(variable_declaration
  (assignment_statement
    (variable_list
      name: (identifier) @name))) @const

;; Global assignments (MAX_RETRIES = 3)
;; Filtered to UPPER_CASE by post_process_lua
(assignment_statement
  (variable_list
    name: (identifier) @name)) @const
"#;

/// Tree-sitter query for extracting Lua function calls.
const CALL_QUERY: &str = r#"
;; Direct function calls (foo())
(function_call
  name: (identifier) @callee)

;; Method calls (obj:method())
(function_call
  name: (method_index_expression
    method: (identifier) @callee))
"#;

/// Doc comment node types — Lua uses `-- comments`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "end", "local", "return", "if", "then", "else", "elseif", "for", "do", "while",
    "repeat", "until", "break", "in", "and", "or", "not", "nil", "true", "false", "self",
    "require", "module", "print", "pairs", "ipairs", "table", "string", "math", "io", "os",
    "type", "tostring", "tonumber", "error", "pcall", "xpcall", "setmetatable", "getmetatable",
];

/// Extracts the return type from a function signature.
/// 
/// # Arguments
/// 
/// * `_signature` - A function signature string to parse
/// 
/// # Returns
/// 
/// Returns `None` as Lua does not support type annotations in function signatures, so return types cannot be extracted from the signature itself.
fn extract_return(_signature: &str) -> Option<String> {
    // Lua has no type annotations in signatures
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "lua",
    grammar: Some(|| tree_sitter_lua::LANGUAGE.into()),
    extensions: &["lua"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_lua as PostProcessChunkFn),
    test_markers: &[],
    test_path_patterns: &["%/tests/%", "%/test/%", "%_test.lua", "%_spec.lua"],
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
    /// Parses a Lua function definition from a temporary file and verifies the parser correctly identifies it.
    /// 
    /// This is a test function that creates a temporary Lua file containing a simple function definition, parses it using the Parser, and asserts that the resulting chunk is correctly identified as a Function type with the name "greet".
    /// 
    /// # Arguments
    /// 
    /// None. This function uses internal test data.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser initialization fails
    /// - The file parsing fails
    /// - A chunk named "greet" is not found in the parsed chunks
    /// - The parsed chunk's type is not ChunkType::Function

    #[test]
    fn parse_lua_function() {
        let content = r#"
function greet(name)
    print("Hello, " .. name)
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Parses a Lua file containing a local function definition and verifies the parser correctly identifies it as a function chunk.
    /// 
    /// This is a unit test that creates a temporary Lua file with a local function named "helper", parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None - this is a test function that operates on internally created test data.
    /// 
    /// # Returns
    /// 
    /// Nothing - this function is a test assertion that will panic if the parsed function chunk does not match expectations.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to read the file, if the "helper" function chunk is not found in the parsed results, or if the chunk type is not `ChunkType::Function`.

    #[test]
    fn parse_lua_local_function() {
        let content = r#"
local function helper(x)
    return x * 2
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "helper").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Parses a Lua file containing a function definition and verifies that function calls within it are correctly extracted.
    /// 
    /// This is a test function that creates a temporary Lua file with a `process` function, parses it using a Lua parser, extracts all function calls from the parsed function chunk, and asserts that the expected function calls (`print` and `tonumber`) are present in the results.
    /// 
    /// # Arguments
    /// 
    /// None. This is a self-contained test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize
    /// - The parser fails to parse the file
    /// - The `process` function is not found in the parsed chunks
    /// - The extracted calls do not contain the expected `print` or `tonumber` function names

    #[test]
    fn parse_lua_calls() {
        let content = r#"
function process(data)
    local trimmed = string.trim(data)
    print(trimmed)
    return tonumber(trimmed)
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"print"), "Expected print, got: {:?}", names);
        assert!(
            names.contains(&"tonumber"),
            "Expected tonumber, got: {:?}",
            names
        );
    }
    /// Tests parsing and extraction of Lua method calls from a function.
    /// 
    /// This test verifies that the parser correctly identifies method calls using the colon syntax (e.g., `obj:init()`) within a Lua function. It creates a temporary Lua file containing a function with method calls, parses it, extracts the function chunk, and validates that all method names are correctly identified.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that asserts expected behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the file cannot be parsed, the "setup" function cannot be found, or if the expected method calls ("init" or "configure") are not found in the extracted calls.

    #[test]
    fn parse_lua_method_call() {
        let content = r#"
function setup(obj)
    obj:init()
    obj:configure("default")
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "setup").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"init"), "Expected init, got: {:?}", names);
        assert!(
            names.contains(&"configure"),
            "Expected configure, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_lua_local_constant() {
        let content = r#"
local MAX_SIZE = 100
local API_URL = "https://example.com"
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let max = chunks.iter().find(|c| c.name == "MAX_SIZE").unwrap();
        assert_eq!(max.chunk_type, ChunkType::Constant);
        let url = chunks.iter().find(|c| c.name == "API_URL").unwrap();
        assert_eq!(url.chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_lua_global_constant() {
        let content = r#"
MAX_RETRIES = 3
DEFAULT_TIMEOUT = 30
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let retries = chunks.iter().find(|c| c.name == "MAX_RETRIES").unwrap();
        assert_eq!(retries.chunk_type, ChunkType::Constant);
        let timeout = chunks.iter().find(|c| c.name == "DEFAULT_TIMEOUT").unwrap();
        assert_eq!(timeout.chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_lua_skip_lowercase_vars() {
        let content = r#"
local counter = 0
local myTable = {}
helper_value = 42
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // lowercase names should be filtered out by post_process
        assert!(chunks.iter().find(|c| c.name == "counter").is_none());
        assert!(chunks.iter().find(|c| c.name == "myTable").is_none());
        assert!(chunks.iter().find(|c| c.name == "helper_value").is_none());
    }

    #[test]
    fn parse_lua_skip_constants_inside_functions() {
        let content = r#"
function init()
    local MAX_LOCAL = 50
    GLOBAL_IN_FUNC = 99
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Constants inside function bodies should be skipped
        assert!(chunks.iter().find(|c| c.name == "MAX_LOCAL").is_none());
        assert!(chunks.iter().find(|c| c.name == "GLOBAL_IN_FUNC").is_none());
        // But the function itself should be captured
        let func = chunks.iter().find(|c| c.name == "init").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_lua_skip_function_assigned_to_var() {
        let content = r#"
local MY_HANDLER = function(x)
    return x * 2
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Function-valued assignments should not become constants
        assert!(chunks.iter().find(|c| c.name == "MY_HANDLER" && c.chunk_type == ChunkType::Constant).is_none());
    }

    #[test]
    fn parse_lua_mixed_functions_and_constants() {
        let content = r#"
local VERSION = "1.0.0"
MAX_BUFFER = 4096

function process(data)
    return data
end

local function helper()
    return true
end
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| (c.name.as_str(), c.chunk_type)).collect();
        assert!(names.contains(&("VERSION", ChunkType::Constant)), "Expected VERSION constant, got: {:?}", names);
        assert!(names.contains(&("MAX_BUFFER", ChunkType::Constant)), "Expected MAX_BUFFER constant, got: {:?}", names);
        assert!(names.contains(&("process", ChunkType::Function)), "Expected process function, got: {:?}", names);
        assert!(names.contains(&("helper", ChunkType::Function)), "Expected helper function, got: {:?}", names);
    }

    #[test]
    fn parse_lua_no_duplicate_constants() {
        let content = r#"
local MAX_SIZE = 100
MAX_RETRIES = 3
"#;
        let file = write_temp_file(content, "lua");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Each constant should appear exactly once
        let max_count = chunks.iter().filter(|c| c.name == "MAX_SIZE").count();
        let retries_count = chunks.iter().filter(|c| c.name == "MAX_RETRIES").count();
        assert_eq!(max_count, 1, "MAX_SIZE should appear once, got {}", max_count);
        assert_eq!(retries_count, 1, "MAX_RETRIES should appear once, got {}", retries_count);
    }

    #[test]
    fn test_is_upper_snake_case_lua() {
        assert!(is_upper_snake_case("MAX_RETRIES"));
        assert!(is_upper_snake_case("API_URL_V2"));
        assert!(is_upper_snake_case("X"));
        assert!(!is_upper_snake_case("lowercase"));
        assert!(!is_upper_snake_case("MixedCase"));
        assert!(!is_upper_snake_case(""));
        assert!(!is_upper_snake_case("123")); // no letters
    }

    #[test]
    fn test_extract_return_lua() {
        assert_eq!(extract_return("function foo(x)"), None);
        assert_eq!(extract_return(""), None);
    }
}
