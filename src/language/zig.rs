//! Zig language definition

use super::{ChunkType, FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Zig code chunks.
///
/// Functions → Function, container types (struct/enum/union) are assigned via
/// `variable_declaration` and reclassified by `post_process_zig`.
const CHUNK_QUERY: &str = r#"
;; Function declarations
(function_declaration
  name: (identifier) @name) @function

;; Container type assignments (const Point = struct { ... })
;; Reclassified to Struct/Enum/TypeAlias by post_process_zig
(variable_declaration
  (identifier) @name) @struct

;; Test declarations
(test_declaration) @function
"#;

/// Tree-sitter query for extracting Zig function calls.
const CALL_QUERY: &str = r#"
;; Direct function calls
(call_expression
  function: (identifier) @callee)

;; Member function calls (obj.method())
(call_expression
  function: (field_expression
    member: (identifier) @callee))
"#;

/// Tree-sitter query for extracting Zig type references.
const TYPE_QUERY: &str = r#"
;; Type expressions in variable declarations and parameters
(type_expression (identifier) @type_ref)
"#;

/// Doc comment node types — Zig uses `///` doc comments parsed as `doc_comment`
const DOC_NODES: &[&str] = &["doc_comment", "line_comment"];

const STOPWORDS: &[&str] = &[
    "fn", "pub", "const", "var", "return", "if", "else", "for", "while", "break", "continue",
    "switch", "unreachable", "undefined", "null", "true", "false", "and", "or", "try", "catch",
    "comptime", "inline", "extern", "export", "struct", "enum", "union", "error", "test",
    "defer", "errdefer", "async", "await", "suspend", "resume", "nosuspend", "orelse",
    "anytype", "anyframe", "void", "noreturn", "type", "usize", "isize", "bool",
];

const COMMON_TYPES: &[&str] = &[
    "void", "noreturn", "bool", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
    "i64", "i128", "isize", "f16", "f32", "f64", "f128", "anytype", "anyframe", "type",
    "anyerror", "anyopaque",
];

/// Post-process Zig chunks: reclassify variable_declaration to correct type,
/// discard non-container variable declarations, and clean test names.
fn post_process_zig(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let kind = node.kind();

    if kind == "test_declaration" {
        // Extract test name from string child or identifier child
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                if child.kind() == "string" || child.kind() == "identifier" {
                    let text = &source[child.start_byte()..child.end_byte()];
                    // Strip quotes from string literals
                    let clean = text.trim_matches('"');
                    *name = clean.to_string();
                    return true;
                }
            }
        }
        *name = "anonymous_test".to_string();
        return true;
    }

    if kind == "variable_declaration" {
        let text = &source[node.start_byte()..node.end_byte()];
        if text.contains("struct") {
            *chunk_type = ChunkType::Struct;
        } else if text.contains("enum") {
            *chunk_type = ChunkType::Enum;
        } else if text.contains("union") {
            *chunk_type = ChunkType::TypeAlias;
        } else if text.contains("error{") || text.contains("error {") {
            *chunk_type = ChunkType::Enum;
        } else {
            // Regular variable — not a significant definition
            return false;
        }
    }

    true
}

/// Extracts and formats the return type from a Zig function signature.
/// 
/// Parses a Zig function signature to locate the return type between the closing parenthesis and opening brace. Strips error union syntax (leading `!`) and filters out void and noreturn types. Returns a formatted string describing the return type.
/// 
/// # Arguments
/// 
/// * `signature` - A Zig function signature string (e.g., `fn name(params) ReturnType { ... }`)
/// 
/// # Returns
/// 
/// `Some(String)` containing a formatted description like "Returns TypeName" if a valid return type is found, or `None` if the signature has no return type, returns void/noreturn, or contains only whitespace.
fn extract_return(signature: &str) -> Option<String> {
    // Zig: fn name(params) ReturnType { ... }
    // Look for ) followed by a type before {
    let paren_pos = signature.rfind(')')?;
    let after_paren = &signature[paren_pos + 1..];
    let brace_pos = after_paren.find('{').unwrap_or(after_paren.len());
    let ret_part = after_paren[..brace_pos].trim();
    if ret_part.is_empty() || ret_part == "void" || ret_part == "noreturn" || ret_part == "anytype"
    {
        return None;
    }
    // Strip error union: !Type → Type
    let ret_type = ret_part.strip_prefix('!').unwrap_or(ret_part).trim();
    if ret_type.is_empty() || ret_type == "void" {
        return None;
    }
    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    if ret_words.is_empty() {
        return None;
    }
    Some(format!("Returns {}", ret_words))
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "zig",
    grammar: Some(|| tree_sitter_zig::LANGUAGE.into()),
    extensions: &["zig"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["struct_declaration", "enum_declaration", "union_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["struct_declaration", "enum_declaration", "union_declaration"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_zig),
    test_markers: &["test "],
    test_path_patterns: &["%/tests/%", "%_test.zig"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[],
    injections: &[],
    doc_format: "default",
    doc_convention: "Use /// doc comments describing parameters and return values.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "pub",
    },
    skip_line_prefixes: &["const ", "pub const"],
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
    /// Parses a Zig source file containing a simple addition function and verifies that the parser correctly identifies it as a function chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to parse the file, if the "add" function chunk is not found in the parsed results, or if the parsed chunk is not of type `ChunkType::Function`.

    #[test]
    fn parse_zig_function() {
        let content = r#"
const std = @import("std");

pub fn add(a: i32, b: i32) i32 {
    return a + b;
}
"#;
        let file = write_temp_file(content, "zig");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Verifies that the parser correctly identifies and classifies a Zig struct definition.
    /// 
    /// This test parses a Zig source file containing a struct named `Point` with fields and a method, then validates that the parser recognizes it as a struct chunk with the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that asserts parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the file cannot be parsed, the `Point` struct is not found in the parsed chunks, or the chunk type is not `ChunkType::Struct`.

    #[test]
    fn parse_zig_struct() {
        let content = r#"
const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point, other: Point) f32 {
        const dx = self.x - other.x;
        const dy = self.y - other.y;
        return @sqrt(dx * dx + dy * dy);
    }
};
"#;
        let file = write_temp_file(content, "zig");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "Point").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
    }
    /// Tests parsing of Zig enum declarations.
    /// 
    /// This test function verifies that the parser correctly identifies and classifies enum definitions in Zig source code. It creates a temporary file containing a simple enum definition with three variants, parses it, and validates that the resulting chunk is properly recognized as an enum type.
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None (unit type). This is a test function.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, if file parsing fails, if the "Color" enum is not found in the parsed chunks, or if the chunk type is not correctly identified as `ChunkType::Enum`.

    #[test]
    fn parse_zig_enum() {
        let content = r#"
const Color = enum {
    red,
    green,
    blue,
};
"#;
        let file = write_temp_file(content, "zig");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks.iter().find(|c| c.name == "Color").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }
    /// Parses a Zig source file and verifies that function calls within code chunks are correctly extracted.
    /// 
    /// This test function creates a temporary Zig file containing a `process` function with standard library calls, parses it using the Parser, locates the `process` function chunk, extracts all calls from that chunk, and asserts that expected function calls (like `init` or `print`) are present in the extracted results.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function returns `()` and is intended to be run as a test.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to initialize or parse the file, if the `process` function chunk is not found, or if the expected function calls are not present in the extracted results.

    #[test]
    fn parse_zig_calls() {
        let content = r#"
const std = @import("std");

pub fn process(allocator: std.mem.Allocator) void {
    const list = std.ArrayList(u8).init(allocator);
    std.debug.print("processing\n", .{});
}
"#;
        let file = write_temp_file(content, "zig");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"init") || names.contains(&"print"),
            "Expected member calls, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_return_zig() {
        assert_eq!(
            extract_return("pub fn add(a: i32, b: i32) i32 {"),
            Some("Returns i32".to_string())
        );
        assert_eq!(extract_return("pub fn main() void {"), None);
        assert_eq!(extract_return("pub fn run() !void {"), None);
        assert_eq!(extract_return(""), None);
    }
}
