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
