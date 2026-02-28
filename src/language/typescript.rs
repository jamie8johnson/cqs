//! TypeScript language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting TypeScript code chunks
const CHUNK_QUERY: &str = r#"
(function_declaration
  name: (identifier) @name) @function

(method_definition
  name: (property_identifier) @name) @function

;; Arrow function assigned to variable: const foo = () => {}
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

;; Arrow function assigned with var/let
(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function) @function))

(class_declaration
  name: (type_identifier) @name) @class

(interface_declaration
  name: (type_identifier) @name) @interface

(enum_declaration
  name: (identifier) @name) @enum

(type_alias_declaration
  name: (type_identifier) @name) @typealias

;; Namespace/module declarations
(internal_module
  name: (identifier) @name) @module
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (member_expression
    property: (property_identifier) @callee))
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(required_parameter type: (type_annotation (type_identifier) @param_type))
(required_parameter type: (type_annotation (generic_type name: (type_identifier) @param_type)))
(optional_parameter type: (type_annotation (type_identifier) @param_type))
(optional_parameter type: (type_annotation (generic_type name: (type_identifier) @param_type)))

;; Return
(function_declaration return_type: (type_annotation (type_identifier) @return_type))
(function_declaration return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))
(method_definition return_type: (type_annotation (type_identifier) @return_type))
(method_definition return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))
(arrow_function return_type: (type_annotation (type_identifier) @return_type))
(arrow_function return_type: (type_annotation (generic_type name: (type_identifier) @return_type)))

;; Field
(public_field_definition type: (type_annotation (type_identifier) @field_type))
(public_field_definition type: (type_annotation (generic_type name: (type_identifier) @field_type)))
(property_signature type: (type_annotation (type_identifier) @field_type))
(property_signature type: (type_annotation (generic_type name: (type_identifier) @field_type)))

;; Impl (extends/implements)
(class_heritage (extends_clause value: (identifier) @impl_type))
(class_heritage (implements_clause (type_identifier) @impl_type))
(extends_type_clause (type_identifier) @impl_type)

;; Bound (type parameter constraints)
(constraint (type_identifier) @bound_type)

;; Alias
(type_alias_declaration value: (type_identifier) @alias_type)
(type_alias_declaration value: (generic_type name: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "const", "let", "var", "return", "if", "else", "for", "while", "do",
    "switch", "case", "break", "continue", "new", "this", "class", "extends", "import",
    "export", "from", "default", "try", "catch", "finally", "throw", "async", "await",
    "true", "false", "null", "undefined", "typeof", "instanceof", "void",
];

fn extract_return(signature: &str) -> Option<String> {
    // TypeScript: return type after `):` e.g. `function foo(): string`
    if let Some(colon) = signature.rfind("):") {
        let ret = signature[colon + 2..].trim();
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "typescript",
    grammar: Some(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
    extensions: &["ts", "tsx"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_body", "class_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.test.ts")),
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "string", "number", "boolean", "void", "null", "undefined", "any", "never", "unknown",
        "Array", "Map", "Set", "Promise", "Record", "Partial", "Required", "Readonly", "Pick",
        "Omit", "Exclude", "Extract", "NonNullable", "ReturnType", "Date", "Error", "RegExp",
        "Function", "Object", "Symbol",
    ],
    container_body_kinds: &["class_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["describe(", "it(", "test("],
    test_path_patterns: &["%.test.%", "%.spec.%", "%/tests/%"],
    structural_matchers: None,
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
    fn parse_typescript_namespace() {
        let content = "namespace Validators {\n  export function check() {}\n}\n";
        let file = write_temp_file(content, "ts");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ns = chunks.iter().find(|c| c.name == "Validators").unwrap();
        assert_eq!(ns.chunk_type, ChunkType::Module);
    }

    #[test]
    fn parse_typescript_type_alias() {
        let content = "type Result = Success | Failure;\n";
        let file = write_temp_file(content, "ts");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "Result").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }
}
