//! Lua language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Lua code chunks.
///
/// Functions → Function (both `function foo()` and local function forms).
/// Method-style declarations via `method_index_expression` name field
/// are captured as functions and reclassified to Method via method_containers.
const CHUNK_QUERY: &str = r#"
;; Named function declarations (function foo() / function mod.foo() / function mod:bar())
(function_declaration
  name: (_) @name) @function
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
    post_process_chunk: None,
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
    fn test_extract_return_lua() {
        assert_eq!(extract_return("function foo(x)"), None);
        assert_eq!(extract_return(""), None);
    }
}
