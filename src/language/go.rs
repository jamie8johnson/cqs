//! Go language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Go code chunks
const CHUNK_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_declaration
  name: (field_identifier) @name) @function

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type))) @struct

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type))) @interface

;; Type aliases — named types (type MyInt int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (type_identifier))) @typealias

;; Type aliases — function types (type Handler func(...))
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (function_type))) @typealias

;; Type aliases — pointer types (type Ptr *int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (pointer_type))) @typealias

;; Type aliases — slice types (type Names []string)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (slice_type))) @typealias

;; Type aliases — map types (type Cache map[string]int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (map_type))) @typealias

;; Type aliases — array types (type Data [10]byte)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (array_type))) @typealias

;; Type aliases — channel types (type Ch chan int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (channel_type))) @typealias

;; Type aliases — qualified types (type Foo pkg.Type)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (qualified_type))) @typealias

;; Go 1.9+ type alias (type Foo = int)
(type_declaration
  (type_alias
    name: (type_identifier) @name)) @typealias

(const_declaration
  (const_spec
    name: (identifier) @name)) @const
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (selector_expression
    field: (field_identifier) @callee))
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (pointer_type (type_identifier) @param_type))
(parameter_declaration type: (qualified_type name: (type_identifier) @param_type))
(parameter_declaration type: (generic_type type: (type_identifier) @param_type))
(parameter_declaration type: (slice_type element: (type_identifier) @param_type))

;; Return
(function_declaration result: (type_identifier) @return_type)
(function_declaration result: (pointer_type (type_identifier) @return_type))
(function_declaration result: (qualified_type name: (type_identifier) @return_type))
(function_declaration result: (generic_type type: (type_identifier) @return_type))
(method_declaration result: (type_identifier) @return_type)
(method_declaration result: (pointer_type (type_identifier) @return_type))
(method_declaration result: (qualified_type name: (type_identifier) @return_type))
(method_declaration result: (generic_type type: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (pointer_type (type_identifier) @field_type))
(field_declaration type: (qualified_type name: (type_identifier) @field_type))
(field_declaration type: (generic_type type: (type_identifier) @field_type))
(field_declaration type: (slice_type element: (type_identifier) @field_type))

;; Impl (interface embedding — embedded types wrapped in type_elem)
(interface_type (type_elem (type_identifier) @impl_type))
(interface_type (type_elem (qualified_type name: (type_identifier) @impl_type)))

;; Alias (type definitions and type aliases)
(type_spec type: (type_identifier) @alias_type)
(type_spec type: (generic_type type: (type_identifier) @alias_type))
(type_alias type: (type_identifier) @alias_type)

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "func", "var", "const", "type", "struct", "interface", "return", "if", "else", "for",
    "range", "switch", "case", "break", "continue", "go", "defer", "select", "chan", "map",
    "package", "import", "true", "false", "nil",
];

fn extract_return(signature: &str) -> Option<String> {
    // Go: `func name(params) returnType {` or `func (recv) name(params) returnType {`
    // Strip trailing { first
    let sig = signature.trim_end_matches('{').trim();

    if sig.ends_with(')') {
        // Check if it's a multi-return like (string, error)
        // Find the matching ( for the final )
        let mut depth = 0;
        let mut start_idx = None;
        for (i, c) in sig.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        start_idx = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(start) = start_idx {
            // Check if there's a ) before this ( - that would be the params close
            let before = &sig[..start].trim();
            if before.ends_with(')') {
                // Multi-return: extract the (...)
                let ret = &sig[start..];
                if !ret.is_empty() {
                    return Some(format!("Returns {}", ret));
                }
            }
        }
        return None;
    } else {
        // Plain return type after last )
        if let Some(paren) = sig.rfind(')') {
            let ret = sig[paren + 1..].trim();
            if ret.is_empty() {
                return None;
            }
            let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
            return Some(format!("Returns {}", ret_words));
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "go",
    grammar: Some(|| tree_sitter_go::LANGUAGE.into()),
    extensions: &["go"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &["method_declaration"],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}_test.go")),
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "string", "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32",
        "uint64", "float32", "float64", "bool", "byte", "rune", "error", "any", "comparable",
        "Context",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &["%\\_test.go"],
    structural_matchers: None,
    entry_point_names: &["main", "init"],
    trait_method_names: &[
        "String", "Error", "Close", "Read", "Write", "ServeHTTP",
        "Len", "Less", "Swap", "MarshalJSON", "UnmarshalJSON",
    ],
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

    #[test]
    fn parse_go_named_type() {
        let content = "package main\n\ntype MyInt int\n";
        let file = write_temp_file(content, "go");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "MyInt").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_go_function_type() {
        let content = "package main\n\ntype Handler func(w Writer, r *Request)\n";
        let file = write_temp_file(content, "go");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "Handler").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_go_type_alias_equals() {
        let content = "package main\n\ntype MyInt = int\n";
        let file = write_temp_file(content, "go");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "MyInt").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_go_struct_still_struct() {
        // Ensure struct type declarations are NOT captured as TypeAlias
        let content = "package main\n\ntype Foo struct {\n\tX int\n}\n";
        let file = write_temp_file(content, "go");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "Foo").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
    }
}
