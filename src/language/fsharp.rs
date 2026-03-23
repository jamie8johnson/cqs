//! F# language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting F# code chunks
const CHUNK_QUERY: &str = r#"
;; Functions (let bindings with arguments)
(function_or_value_defn
  (function_declaration_left
    (identifier) @name)) @function

;; Module definitions
(module_defn
  (identifier) @name) @module

;; Type definitions — records → struct
(type_definition
  (record_type_defn
    (type_name
      type_name: (identifier) @name))) @struct

;; Type definitions — discriminated unions → enum
(type_definition
  (union_type_defn
    (type_name
      type_name: (identifier) @name))) @enum

;; Type definitions — enums → enum
(type_definition
  (enum_type_defn
    (type_name
      type_name: (identifier) @name))) @enum

;; Type definitions — interfaces
(type_definition
  (interface_type_defn
    (type_name
      type_name: (identifier) @name))) @interface

;; Type definitions — delegates
(type_definition
  (delegate_type_defn
    (type_name
      type_name: (identifier) @name))) @delegate

;; Type definitions — type abbreviations (type Foo = string)
(type_definition
  (type_abbrev_defn
    (type_name
      type_name: (identifier) @name))) @typealias

;; Type definitions — classes (anon_type_defn = class with optional primary constructor)
(type_definition
  (anon_type_defn
    (type_name
      type_name: (identifier) @name))) @class

;; Type extensions (type MyType with member ...)
(type_extension
  (type_name
    type_name: (identifier) @name)) @extension

;; Member definitions — concrete (member this.Method(...) = ...)
(member_defn
  (method_or_prop_defn
    name: (property_or_ident
      method: (identifier) @name))) @function

;; Member definitions — abstract (abstract member Name: ...)
(member_defn
  (member_signature
    (identifier) @name)) @function
"#;

/// Tree-sitter query for extracting F# function calls
const CALL_QUERY: &str = r#"
;; Function application — first child is the function
(application_expression
  . (long_identifier_or_op
       (long_identifier
         (identifier) @callee)))

;; Dot access calls — obj.Method
(dot_expression
  field: (long_identifier_or_op
           (long_identifier
             (identifier) @callee)))
"#;

/// Tree-sitter query for extracting F# type references
const TYPE_QUERY: &str = r#"
;; Record field types
(record_field
  (type
    (long_identifier
      (identifier) @field_type)))

;; Parameter types in typed patterns: (x: int)
(typed_pattern
  (type
    (long_identifier
      (identifier) @param_type)))

;; Inheritance
(class_inherits_decl
  (type
    (long_identifier
      (identifier) @impl_type)))

;; Interface implementation
(interface_implementation
  (type
    (long_identifier
      (identifier) @impl_type)))

;; Constraint types
(constraint
  (type
    (long_identifier
      (identifier) @bound_type)))
"#;

/// Doc comment node types — F# uses /// XML doc comments (parsed as line_comment)
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "let", "in", "if", "then", "else", "match", "with", "fun", "function", "type", "module",
    "open", "do", "for", "while", "yield", "return", "mutable", "rec", "and", "or", "not",
    "true", "false", "null", "abstract", "member", "override", "static", "private", "public",
    "internal", "val", "new", "inherit", "interface", "end", "begin", "of", "as", "when",
    "upcast", "downcast", "use", "try", "finally", "raise", "async", "task",
];

const COMMON_TYPES: &[&str] = &[
    "string", "int", "bool", "float", "decimal", "byte", "char", "unit", "obj", "int64", "uint",
    "int16", "double", "nativeint", "bigint", "seq", "list", "array", "option", "voption",
    "result", "Map", "Set", "Dictionary", "HashSet", "ResizeArray", "Task", "Async",
    "IDisposable", "IEnumerable", "IComparable", "Exception", "StringBuilder",
    "CancellationToken",
];

/// Extracts the return type annotation from an F# function signature and formats it as a documentation string.
/// 
/// # Arguments
/// 
/// * `signature` - An F# function signature string (e.g., "let processData (input: string) : int =")
/// 
/// # Returns
/// 
/// Returns `Some(String)` containing a formatted return type description (e.g., "Returns int") if a non-unit return type annotation exists after the last colon outside of parentheses. Returns `None` if no '=' is found, no return type annotation exists, the return type is empty, or the return type is "unit".
fn extract_return(signature: &str) -> Option<String> {
    // F#: optional return type annotation after last ':' before '='
    // e.g., "let processData (input: string) : int =" → "int"
    // Must handle nested parens (parameter types also use ':')
    let eq_pos = signature.find('=')?;
    let before_eq = &signature[..eq_pos];

    // Find the last ':' that's outside parentheses
    let mut paren_depth = 0i32;
    let mut last_colon_outside = None;
    for (i, ch) in before_eq.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            ':' if paren_depth == 0 => last_colon_outside = Some(i),
            _ => {}
        }
    }

    let colon_pos = last_colon_outside?;
    let ret_type = before_eq[colon_pos + 1..].trim();
    if ret_type.is_empty() || ret_type == "unit" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

/// Extract container type name for F# type definitions.
/// F# containers (anon_type_defn, record_type_defn, etc.) store the name
/// in a child `type_name` node's `type_name` field — not a direct `name` field.
fn extract_container_name_fsharp(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_name" {
            if let Some(name) = child.child_by_field_name("type_name") {
                return Some(source[name.byte_range()].to_string());
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "fsharp",
    grammar: Some(|| tree_sitter_fsharp::LANGUAGE_FSHARP.into()),
    extensions: &["fs", "fsi"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[
        "anon_type_defn",
        "interface_type_defn",
        "record_type_defn",
        "union_type_defn",
    ],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.fs")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &[],
    extract_container_name: Some(extract_container_name_fsharp),
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["[<Test>]", "[<Fact>]", "[<Theory>]"],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%Tests.fs"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "Equals", "GetHashCode", "ToString", "CompareTo", "Dispose",
    ],
    injections: &[],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
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
    fn test_extract_return_fsharp() {
        assert_eq!(
            extract_return("let processData (input: string) : int ="),
            Some("Returns int".to_string())
        );
        assert_eq!(extract_return("let add x y ="), None);
        assert_eq!(
            extract_return("member this.GetName() : string ="),
            Some("Returns string".to_string())
        );
        assert_eq!(extract_return("let doSomething () : unit ="), None);
    }
    /// Parses an F# function definition and verifies the parser correctly identifies it as a function chunk.
    /// 
    /// This is a test function that creates a temporary F# file containing a simple function definition, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
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
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - No chunk named "add" is found in the parsed results
    /// - The chunk type is not Function

    #[test]
    fn parse_fsharp_function() {
        let content = "let add x y = x + y\n";
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, crate::parser::ChunkType::Function);
    }
    /// Parses an F# record type definition and verifies it is correctly identified as a struct chunk.
    /// 
    /// This test function writes an F# record definition to a temporary file, parses it using the Parser, and asserts that the resulting chunk for the "Person" record is recognized with the correct ChunkType::Struct classification.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded F# source code.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, if parsing fails, if the "Person" chunk is not found in the parsed results, or if the chunk_type is not ChunkType::Struct.

    #[test]
    fn parse_fsharp_record() {
        let content = r#"
type Person = {
    Name: string
    Age: int
}
"#;
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let record = chunks.iter().find(|c| c.name == "Person").unwrap();
        assert_eq!(record.chunk_type, crate::parser::ChunkType::Struct);
    }
    /// Verifies that the parser correctly recognizes F# discriminated unions as enum chunks.
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
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "Shape" chunk is not found in the parsed output, or the chunk type is not `ChunkType::Enum`.

    #[test]
    fn parse_fsharp_discriminated_union() {
        let content = r#"
type Shape =
    | Circle of float
    | Rectangle of float * float
"#;
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let du = chunks.iter().find(|c| c.name == "Shape").unwrap();
        assert_eq!(du.chunk_type, crate::parser::ChunkType::Enum);
    }
    /// Verifies that the parser correctly identifies and extracts F# class and method definitions from source code.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded F# source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to parse the file
    /// - The "Calculator" class chunk is not found
    /// - The "Add" method chunk is not found
    /// - Any of the assertions about chunk types or parent relationships fail

    #[test]
    fn parse_fsharp_class_and_method() {
        let content = r#"
type Calculator() =
    member this.Add(a: int, b: int) : int =
        a + b
"#;
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
        assert_eq!(class.chunk_type, crate::parser::ChunkType::Class);
        let method = chunks.iter().find(|c| c.name == "Add").unwrap();
        assert_eq!(method.chunk_type, crate::parser::ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }
    /// Verifies that F# abstract type definitions without the [<Interface>] attribute are correctly parsed as Class chunks.
    /// 
    /// This test validates the parser's handling of F# interface-like types that lack explicit [<Interface>] attributes. F# allows abstract classes and interfaces to be defined identically in syntax, and tree-sitter-fsharp parses unattributed abstract types as anonymous type definitions (Class). The test writes a temporary F# file containing an abstract member definition and confirms the resulting parsed chunk is classified as a Class type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data and parser instance.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts on the parser output and returns unit.
    /// 
    /// # Panics
    /// 
    /// Panics if file creation fails, parsing fails, the "ILogger" chunk is not found, or the assertion on chunk type fails.

    #[test]
    fn parse_fsharp_interface() {
        // F# interfaces with [<Interface>] attribute use interface_type_defn.
        // Without the attribute, tree-sitter-fsharp parses them as anon_type_defn (Class).
        // This is correct F# behavior — abstract classes and interfaces are both valid
        // without [<Interface>]. We accept Class for unattributed abstract types.
        let content = r#"
type ILogger =
    abstract member Log: string -> unit
"#;
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let iface = chunks.iter().find(|c| c.name == "ILogger").unwrap();
        // Without [<Interface>] attribute, tree-sitter classifies as anon_type_defn → Class
        assert_eq!(iface.chunk_type, crate::parser::ChunkType::Class);
    }
    /// Parses an F# module definition and verifies the parser correctly identifies it as a Module chunk type.
    /// 
    /// # Arguments
    /// 
    /// None - this is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None - this function performs assertions and returns unit `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser cannot be instantiated
    /// - The file cannot be parsed
    /// - A chunk named "Helpers" is not found in the parsed output
    /// - The "Helpers" chunk is not identified as a Module type

    #[test]
    fn parse_fsharp_module() {
        let content = r#"
module Helpers =
    let helper x = x + 1
"#;
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let module = chunks.iter().find(|c| c.name == "Helpers").unwrap();
        assert_eq!(module.chunk_type, crate::parser::ChunkType::Module);
    }
    /// Verifies that F# type abbreviations are correctly parsed as type aliases.
    /// 
    /// This test function writes a temporary F# file containing a type abbreviation (e.g., `type Callback = int -> string`), parses it using the Parser, and asserts that the resulting chunk is correctly identified as a TypeAlias with the expected name.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded test data.
    /// 
    /// # Returns
    /// 
    /// None. Returns unit `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser fails to parse the file
    /// - A chunk named "Callback" is not found in the parsed output
    /// - The parsed chunk's type is not `ChunkType::TypeAlias`

    #[test]
    fn parse_fsharp_type_abbreviation() {
        // F# type abbreviation: `type X = ExistingType`
        // Note: `type Name = string` is parsed as union_type_defn by tree-sitter-fsharp
        // because bare lowercase identifiers are ambiguous. Use function types to test.
        let content = "type Callback = int -> string\n";
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "Callback").unwrap();
        assert_eq!(ta.chunk_type, crate::parser::ChunkType::TypeAlias);
    }
    /// Parses F# method calls and verifies that function call extraction correctly identifies dot notation calls.
    /// 
    /// This test function creates a parser and processes F# code containing string and integer operations, then validates that the `Trim` and `Parse` method calls are properly extracted from the parsed content.
    /// 
    /// # Arguments
    /// 
    /// None. This is a standalone test function that uses hardcoded F# source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the extracted function calls do not contain the expected "Trim" or "Parse" method names, indicating the parser failed to correctly identify dot notation calls in F# code.

    #[test]
    fn parse_fsharp_calls() {
        let content = r#"
let processData (input: string) : int =
    let trimmed = input.Trim()
    let parsed = Int32.Parse(trimmed)
    add parsed 1
"#;
        let parser = Parser::new().unwrap();
        let lang = crate::parser::Language::FSharp;
        let calls = parser.extract_calls(content, lang, 0, content.len(), 0);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        // Dot calls
        assert!(names.contains(&"Trim"), "Expected Trim call, got: {:?}", names);
        assert!(
            names.contains(&"Parse"),
            "Expected Parse call, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_fsharp_type_extension() {
        let content = "type MyRecord with\n    member x.Greet() = \"hello\"\n";
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ext = chunks.iter().find(|c| c.chunk_type == crate::parser::ChunkType::Extension);
        assert!(
            ext.is_some(),
            "Expected a type extension chunk, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>()
        );
        let ext = ext.unwrap();
        assert_eq!(ext.name, "MyRecord");
        assert_eq!(ext.chunk_type, crate::parser::ChunkType::Extension);
    }
}
