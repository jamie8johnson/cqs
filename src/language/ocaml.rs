//! OCaml language definition

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting OCaml code chunks.
///
/// OCaml constructs:
///   - `value_definition` / `let_binding` → Function
///   - `type_definition` / `type_binding` → TypeAlias (or Enum/Struct via post-process)
///   - `module_definition` / `module_binding` → Module
const CHUNK_QUERY: &str = r#"
;; Let binding (function/value): let add x y = x + y
(value_definition
  (let_binding
    pattern: (value_name) @name)) @function

;; Type definition: type color = Red | Green | Blue
(type_definition
  (type_binding
    name: (type_constructor) @name)) @struct

;; Module definition: module Foo = struct ... end
(module_definition
  (module_binding
    (module_name) @name)) @struct
"#;

/// Tree-sitter query for extracting OCaml calls.
///
/// OCaml uses `application_expression` for function application:
///   - Direct: `add x y` → (application_expression function: (value_name))
///   - Qualified: `List.map f xs` → (application_expression function: (value_path (value_name)))
const CALL_QUERY: &str = r#"
;; Function application: foo x or Module.func x
;; All calls go through value_path (even unqualified ones)
(application_expression
  function: (value_path
    (value_name) @callee))
"#;

/// Doc comment node types — OCaml uses `(** ... *)` doc comments
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "let", "in", "val", "type", "module", "struct", "sig", "end", "fun", "function", "match",
    "with", "when", "if", "then", "else", "begin", "do", "done", "for", "to", "downto", "while",
    "open", "include", "rec", "and", "of", "mutable", "ref", "try", "raise", "exception",
    "external", "true", "false", "unit", "int", "float", "string", "bool", "char", "list",
    "option", "array", "Some", "None", "Ok", "Error", "failwith", "Printf", "Scanf",
    "List", "Array", "Map", "Set", "Hashtbl", "Buffer", "String",
];

/// Post-process OCaml chunks to set correct chunk types.
fn post_process_ocaml(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "value_definition" => *chunk_type = ChunkType::Function,
        "type_definition" => {
            // Classify based on type body content
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            // Variant types use | for constructors (not inside strings/comments)
            // Check for pattern: = Constructor | Constructor or = | Constructor
            if let Some(eq_pos) = text.find('=') {
                let after_eq = &text[eq_pos + 1..];
                if after_eq.contains('|') {
                    *chunk_type = ChunkType::Enum;
                } else if after_eq.contains('{') {
                    *chunk_type = ChunkType::Struct;
                } else {
                    *chunk_type = ChunkType::TypeAlias;
                }
            } else {
                *chunk_type = ChunkType::TypeAlias;
            }
        }
        "module_definition" => *chunk_type = ChunkType::Module,
        _ => {}
    }
    true
}

/// Extract return type from OCaml type signatures.
///
/// Handles val specifications: `val add : int -> int -> int`
/// Return type is the last type after the final `->`.
fn extract_return(signature: &str) -> Option<String> {
    let trimmed = signature.trim();

    // val specification: val name : t1 -> t2 -> return_type
    if trimmed.starts_with("val ") {
        let type_part = trimmed.split_once(':')?.1.trim();
        let ret = if type_part.contains("->") {
            type_part.rsplit("->").next()?.trim()
        } else {
            type_part
        };
        if ret.is_empty() {
            return None;
        }
        let words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", words.to_lowercase()));
    }

    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "ocaml",
    grammar: Some(|| tree_sitter_ocaml::LANGUAGE_OCAML.into()),
    extensions: &["ml", "mli"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("test/test_{stem}.ml")),
    type_query: None,
    common_types: &[
        "int", "float", "string", "bool", "char", "unit", "list", "option", "array", "ref",
    ],
    container_body_kinds: &["structure"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_ocaml as PostProcessChunkFn),
    test_markers: &["let%test", "let%expect_test", "let test_"],
    test_path_patterns: &["%/test/%", "%_test.ml"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "compare", "equal", "hash", "pp", "show", "to_string", "of_string",
    ],
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
    /// Parses an OCaml source file containing a function definition and verifies that the parser correctly identifies it as a function chunk.
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
    /// Panics if the temporary file cannot be written, the parser fails to initialize, the file cannot be parsed, the "add" function is not found in the parsed chunks, or the chunk type is not identified as a Function.

    #[test]
    fn parse_ocaml_function() {
        let content = r#"
let add x y = x + y
"#;
        let file = write_temp_file(content, "ml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Tests parsing of OCaml variant type definitions and verifies they are correctly identified as Enum chunks.
    /// 
    /// This test writes a temporary OCaml file containing a variant type definition with three constructors (Red, Green, Blue), parses it using the Parser, and asserts that the resulting chunk is found with the correct name and type classification.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the 'color' variant type is not found in the parsed chunks or if it is not classified as an Enum chunk type.

    #[test]
    fn parse_ocaml_type_variant() {
        let content = r#"
type color = Red | Green | Blue
"#;
        let file = write_temp_file(content, "ml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let dt = chunks
            .iter()
            .find(|c| c.name == "color" && c.chunk_type == ChunkType::Enum);
        assert!(dt.is_some(), "Should find 'color' variant type as Enum");
    }
    /// Parses an OCaml record type definition and verifies it is correctly recognized as a Struct chunk.
    /// 
    /// # Arguments
    /// 
    /// This function takes no arguments.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. This is a test function that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to parse the temporary file, if the file writing fails, or if the 'point' record type is not found in the parsed chunks as a Struct chunk type.

    #[test]
    fn parse_ocaml_type_record() {
        let content = r#"
type point = {
  x : float;
  y : float;
}
"#;
        let file = write_temp_file(content, "ml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let dt = chunks
            .iter()
            .find(|c| c.name == "point" && c.chunk_type == ChunkType::Struct);
        assert!(dt.is_some(), "Should find 'point' record type as Struct");
    }
    /// Parses an OCaml module definition and verifies that the parser correctly identifies and extracts the module chunk.
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
    /// Panics if the parser fails to initialize, fails to parse the temporary file, or if the "Calculator" module is not found in the parsed chunks.

    #[test]
    fn parse_ocaml_module() {
        let content = r#"
module Calculator = struct
  let add x y = x + y
end
"#;
        let file = write_temp_file(content, "ml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let module = chunks
            .iter()
            .find(|c| c.name == "Calculator" && c.chunk_type == ChunkType::Module);
        assert!(module.is_some(), "Should find 'Calculator' module");
    }
    /// Parses an OCaml source file and verifies that function calls within a specific function are correctly extracted.
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
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser fails to initialize
    /// - The file cannot be parsed
    /// - The "process" function is not found in the parsed chunks
    /// - The "validate" function call is not found in the extracted calls

    #[test]
    fn parse_ocaml_calls() {
        let content = r#"
let process text =
  let trimmed = String.trim text in
  Printf.printf "%s\n" trimmed;
  validate trimmed
"#;
        let file = write_temp_file(content, "ml");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"validate"),
            "Expected validate, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_return_ocaml() {
        assert_eq!(
            extract_return("val add : int -> int -> int"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            extract_return("val name : string"),
            Some("Returns string".to_string())
        );
        assert_eq!(extract_return("let add x y = x + y"), None);
        assert_eq!(extract_return(""), None);
    }
}
