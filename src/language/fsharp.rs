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

    #[test]
    fn parse_fsharp_function() {
        let content = "let add x y = x + y\n";
        let file = write_temp_file(content, "fs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(func.chunk_type, crate::parser::ChunkType::Function);
    }

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
}
