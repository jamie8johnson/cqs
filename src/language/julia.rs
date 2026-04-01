//! Julia language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Julia code chunks.
/// Julia constructs:
///   - `function_definition` → Function (name in `signature` → `identifier`)
///   - `struct_definition` → Struct (name in `type_head` → `identifier`)
///   - `abstract_definition` → TypeAlias (name in `type_head` → `identifier`)
///   - `module_definition` → Module (has named `name` field)
///   - `macro_definition` → Macro (name in `signature` → `identifier`)
const CHUNK_QUERY: &str = r#"
;; Function definition: function add(x, y) ... end
(function_definition
  (signature
    (call_expression . (identifier) @name))) @function

;; Struct definition: struct Point x::Float64 end
(struct_definition
  (type_head
    (identifier) @name)) @struct

;; Abstract type: abstract type Shape end
(abstract_definition
  (type_head
    (identifier) @name)) @struct

;; Module definition: module Foo ... end
(module_definition
  name: (identifier) @name) @struct

;; Macro definition: macro name(args) ... end
(macro_definition
  (signature
    (call_expression . (identifier) @name))) @function
"#;

/// Tree-sitter query for extracting Julia calls.
/// Julia uses `call_expression` for function calls:
///   - Direct: `add(x, y)` → (call_expression (identifier))
const CALL_QUERY: &str = r#"
;; Direct function call: foo(args)
(call_expression
  (identifier) @callee)
"#;

/// Doc comment node types — Julia uses triple-quoted string literals as docstrings
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "function", "end", "module", "struct", "mutable", "abstract", "type", "macro", "begin",
    "let", "const", "if", "elseif", "else", "for", "while", "do", "try", "catch", "finally",
    "return", "break", "continue", "import", "using", "export", "true", "false", "nothing",
    "where", "in", "isa", "typeof", "Int", "Int64", "Float64", "String", "Bool", "Char",
    "Vector", "Array", "Dict", "Set", "Tuple", "Nothing", "Any", "Union", "AbstractFloat",
    "AbstractString", "println", "print", "push!", "pop!", "length", "size", "map", "filter",
];

/// Post-process Julia chunks to set correct chunk types.
fn post_process_julia(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "function_definition" => *chunk_type = ChunkType::Function,
        "struct_definition" => *chunk_type = ChunkType::Struct,
        "abstract_definition" => *chunk_type = ChunkType::TypeAlias,
        "module_definition" => *chunk_type = ChunkType::Module,
        "macro_definition" => *chunk_type = ChunkType::Macro,
        _ => {}
    }
    true
}

/// Extract return type from Julia function signatures.
/// Julia signatures: `function add(x::Int, y::Int)::Int`
/// Return type is after `)::`
fn extract_return(signature: &str) -> Option<String> {
    let trimmed = signature.trim();

    // function foo(x, y)::ReturnType
    let paren_pos = trimmed.rfind(')')?;
    let after = trimmed[paren_pos + 1..].trim();
    let ret = after.strip_prefix("::")?.trim();

    // Remove trailing 'where' clause
    let ret = ret.split_whitespace().next()?;

    if ret.is_empty() {
        return None;
    }

    // Skip Nothing (void equivalent)
    if ret == "Nothing" {
        return None;
    }

    let words = crate::nl::tokenize_identifier(ret).join(" ");
    Some(format!("Returns {}", words.to_lowercase()))
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "julia",
    grammar: Some(|| tree_sitter_julia::LANGUAGE.into()),
    extensions: &["jl"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_test.jl")),
    test_name_suggestion: None,
    type_query: None,
    common_types: &[
        "Int", "Int64", "Float64", "String", "Bool", "Char", "Vector", "Array", "Dict", "Set",
        "Tuple", "Nothing", "Any",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_julia as PostProcessChunkFn),
    test_markers: &["@test", "@testset"],
    test_path_patterns: &["%/test/%", "%_test.jl"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "show", "convert", "promote_rule", "iterate", "length", "getindex", "setindex!",
    ],
    injections: &[],
    doc_format: "default",
    doc_convention: "Use triple-quoted docstrings with # Arguments, # Returns sections.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["struct ", "mutable struct"],
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
    fn parse_julia_function() {
        let content = r#"
function add(x, y)
    return x + y
end
"#;
        let file = write_temp_file(content, "jl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_julia_struct() {
        let content = r#"
struct Point
    x::Float64
    y::Float64
end
"#;
        let file = write_temp_file(content, "jl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks
            .iter()
            .find(|c| c.name == "Point" && c.chunk_type == ChunkType::Struct);
        assert!(s.is_some(), "Should find 'Point' struct");
    }

    #[test]
    fn parse_julia_module() {
        let content = r#"
module Calculator
    function add(x, y)
        return x + y
    end
end
"#;
        let file = write_temp_file(content, "jl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let module = chunks
            .iter()
            .find(|c| c.name == "Calculator" && c.chunk_type == ChunkType::Module);
        assert!(module.is_some(), "Should find 'Calculator' module");
    }

    #[test]
    fn parse_julia_abstract_type() {
        let content = r#"
abstract type Shape end
"#;
        let file = write_temp_file(content, "jl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let at = chunks
            .iter()
            .find(|c| c.name == "Shape" && c.chunk_type == ChunkType::TypeAlias);
        assert!(at.is_some(), "Should find 'Shape' abstract type");
    }

    #[test]
    fn parse_julia_calls() {
        let content = r#"
function process(data)
    result = transform(data)
    println(result)
    validate(result)
end
"#;
        let file = write_temp_file(content, "jl");
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
    fn test_extract_return_julia() {
        assert_eq!(
            extract_return("function add(x::Int, y::Int)::Int"),
            Some("Returns int".to_string())
        );
        assert_eq!(extract_return("function greet(name)"), None);
        assert_eq!(
            extract_return("function main()::Nothing"),
            None
        );
        assert_eq!(extract_return(""), None);
    }
}
