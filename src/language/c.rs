//! C language definition

use super::{FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting C code chunks
const CHUNK_QUERY: &str = r#"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @enum

(type_definition
  declarator: (type_identifier) @name) @typealias

(declaration
  declarator: (init_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @function

;; Union definitions
(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Preprocessor constants (#define FOO 42)
(preproc_def
  name: (identifier) @name) @const

;; Preprocessor function macros (#define FOO(x) ...)
(preproc_function_def
  name: (identifier) @name) @macro
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (struct_specifier name: (type_identifier) @param_type))
(parameter_declaration type: (enum_specifier name: (type_identifier) @param_type))

;; Return
(function_definition type: (type_identifier) @return_type)
(function_definition type: (struct_specifier name: (type_identifier) @return_type))
(function_definition type: (enum_specifier name: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (struct_specifier name: (type_identifier) @field_type))
(field_declaration type: (enum_specifier name: (type_identifier) @field_type))

;; Alias (typedef)
(type_definition type: (type_identifier) @alias_type)
(type_definition type: (struct_specifier name: (type_identifier) @alias_type))
(type_definition type: (enum_specifier name: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "case", "break", "continue", "return",
    "typedef", "struct", "enum", "union", "void", "int", "char", "float", "double", "long",
    "short", "unsigned", "signed", "static", "extern", "const", "volatile", "sizeof",
    "null", "true", "false",
];

/// Extracts the return type from a C function signature and formats it as documentation text.
/// 
/// # Arguments
/// 
/// `signature` - A C function signature string, expected to contain a return type, function name, and parameter list in parentheses (e.g., "int add(int a, int b)").
/// 
/// # Returns
/// 
/// `Some(String)` containing the formatted return type documentation (e.g., "Returns int") if a non-void return type is found after filtering out storage class specifiers (static, inline, extern, const, volatile). Returns `None` if the signature is malformed, has no return type, or the return type is void.
fn extract_return(signature: &str) -> Option<String> {
    // C: return type is before the function name, e.g., "int add(int a, int b)"
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        // Last word is function name, everything before is return type + modifiers
        if words.len() >= 2 {
            // Filter out storage class specifiers
            let type_words: Vec<&str> = words[..words.len() - 1]
                .iter()
                .filter(|w| {
                    !matches!(**w, "static" | "inline" | "extern" | "const" | "volatile")
                })
                .copied()
                .collect();
            if !type_words.is_empty() && type_words != ["void"] {
                let ret = type_words.join(" ");
                let ret_words = crate::nl::tokenize_identifier(&ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "c",
    grammar: Some(|| tree_sitter_c::LANGUAGE.into()),
    extensions: &["c", "h"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "int", "char", "float", "double", "void", "long", "short", "unsigned", "size_t",
        "ssize_t", "ptrdiff_t", "FILE", "bool",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &["%/tests/%", "%\\_test.c"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[],
    injections: &[],
    doc_format: "javadoc",
    doc_convention: "Use Doxygen format: @param, @return, @throws tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes: "static const volatile extern unsigned signed",
    },
    skip_line_prefixes: &["struct ", "union ", "enum ", "typedef "],
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
    /// Parses a C union declaration and verifies it is correctly identified as a struct chunk type.
    /// 
    /// This function tests the parser's ability to handle C union syntax by writing a union definition to a temporary file, parsing it, and asserting that the resulting chunk has the expected name and type classification.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data internally.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser cannot be initialized, the file cannot be parsed, the "Data" chunk is not found in the parse results, or the chunk type assertion fails.

    #[test]
    fn parse_c_union() {
        let content = "union Data {\n  int i;\n  float f;\n};\n";
        let file = write_temp_file(content, "c");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let u = chunks.iter().find(|c| c.name == "Data").unwrap();
        assert_eq!(u.chunk_type, ChunkType::Struct);
    }
    /// Parses a C preprocessor define directive and verifies it is recognized as a constant chunk.
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
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the MAX_SIZE chunk is not found, or the chunk type is not identified as a Constant.

    #[test]
    fn parse_c_define_constant() {
        let content = "#define MAX_SIZE 1024\n";
        let file = write_temp_file(content, "c");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let c = chunks.iter().find(|c| c.name == "MAX_SIZE").unwrap();
        assert_eq!(c.chunk_type, ChunkType::Constant);
    }
    /// Tests that the parser correctly identifies and classifies C preprocessor define macros.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts on parsing results and panics if assertions fail.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize, file parsing fails, the "SWAP" macro is not found in parsed chunks, or the chunk type is not correctly identified as a Macro.

    #[test]
    fn parse_c_define_macro() {
        let content = "#define SWAP(a, b) do { int t = a; a = b; b = t; } while(0)\n";
        let file = write_temp_file(content, "c");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let m = chunks.iter().find(|c| c.name == "SWAP").unwrap();
        assert_eq!(m.chunk_type, ChunkType::Macro);
    }
    /// Verifies that a C typedef declaration is correctly parsed as a TypeAlias chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses internal helper functions and assertions.
    /// 
    /// # Returns
    /// 
    /// None. Returns `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize
    /// - Parsing the file fails
    /// - A chunk named "MyInt" is not found in the parsed results
    /// - The parsed chunk is not of type `ChunkType::TypeAlias`

    #[test]
    fn parse_c_typedef_as_typealias() {
        let content = "typedef int MyInt;\n";
        let file = write_temp_file(content, "c");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "MyInt").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }
}
