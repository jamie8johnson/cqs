//! IEC 61131-3 Structured Text language definition

use super::{FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting ST code chunks
const CHUNK_QUERY: &str = r#"
(function_block_definition
  name: (identifier) @name) @class

(function_definition
  name: (identifier) @name) @function

(program_definition
  name: (identifier) @name) @module

(method_definition
  name: (identifier) @name) @function

(action_definition
  name: (identifier) @name) @function

(type_definition
  name: (identifier) @name) @struct
"#;

/// Tree-sitter query for extracting function/FB calls
const CALL_QUERY: &str = r#"
(call_expression
  functionName: (identifier) @callee)
"#;

/// Tree-sitter query for extracting type references from VAR declarations
const TYPE_QUERY: &str = r#"
;; Variable declarations with basic or derived types
(var_decl_item
  (basic_data_type) @param_type)

(var_decl_item
  (derived_data_type) @param_type)

;; Array element types
(array_type
  (basic_data_type) @field_type)
(array_type
  (derived_data_type) @field_type)

;; Struct fields
(struct_field
  (basic_data_type) @field_type)
(struct_field
  (derived_data_type) @field_type)

;; FUNCTION_BLOCK EXTENDS
(function_block_definition
  base: (identifier) @impl_type)

;; Function/method return types
(function_definition
  (basic_data_type) @return_type)
(function_definition
  (derived_data_type) @return_type)
(method_definition
  (basic_data_type) @return_type)
(method_definition
  (derived_data_type) @return_type)
"#;

/// Doc comment node types (ST uses (* ... *) block comments and // inline)
const DOC_NODES: &[&str] = &["block_comment", "inline_comment"];

const STOPWORDS: &[&str] = &[
    // Control flow
    "IF", "THEN", "ELSIF", "ELSE", "END_IF",
    "CASE", "OF", "END_CASE",
    "FOR", "TO", "BY", "DO", "END_FOR",
    "WHILE", "END_WHILE",
    "REPEAT", "UNTIL", "END_REPEAT",
    "RETURN", "EXIT",
    // Declarations
    "PROGRAM", "END_PROGRAM",
    "FUNCTION", "END_FUNCTION",
    "FUNCTION_BLOCK", "END_FUNCTION_BLOCK",
    "METHOD", "END_METHOD",
    "ACTION", "END_ACTION",
    "TYPE", "END_TYPE",
    "STRUCT", "END_STRUCT",
    "VAR", "VAR_INPUT", "VAR_OUTPUT", "VAR_IN_OUT", "VAR_TEMP", "VAR_GLOBAL", "END_VAR",
    "CONSTANT", "RETAIN", "PERSISTENT",
    // Data types
    "BOOL", "BYTE", "WORD", "DWORD", "LWORD",
    "SINT", "INT", "DINT", "LINT",
    "USINT", "UINT", "UDINT", "ULINT",
    "REAL", "LREAL",
    "STRING", "WSTRING",
    "TIME", "DATE", "DATE_AND_TIME", "TIME_OF_DAY",
    "ARRAY",
    // Operators
    "AND", "OR", "XOR", "NOT", "MOD",
    // Literals
    "TRUE", "FALSE",
    // Access
    "PUBLIC", "PRIVATE", "PROTECTED", "INTERNAL", "FINAL", "ABSTRACT",
    "EXTENDS",
];

static DEFINITION: LanguageDef = LanguageDef {
    name: "structured_text",
    grammar: Some(|| tree_sitter_structured_text::LANGUAGE.into()),
    extensions: &["st", "stl"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &["method_definition"],
    method_containers: &["function_block_definition"],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "BOOL", "BYTE", "WORD", "DWORD", "LWORD",
        "SINT", "INT", "DINT", "LINT",
        "USINT", "UINT", "UDINT", "ULINT",
        "REAL", "LREAL",
        "STRING", "WSTRING",
        "TIME", "DATE", "TON", "TOF", "TP",
        "CTU", "CTD", "CTUD",
        "R_TRIG", "F_TRIG",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &["Main", "MAIN"],
    trait_method_names: &[],
    injections: &[],
    doc_format: "block_comment",
    doc_convention: "Use (* ... *) block comments before declarations.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["VAR", "END_VAR", "FUNCTION", "END_FUNCTION", "PROGRAM", "END_PROGRAM"],
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
    fn parse_function_block() {
        let content = r#"
FUNCTION_BLOCK PID_Controller
VAR_INPUT
    SetPoint : REAL;
    ProcessValue : REAL;
END_VAR
VAR_OUTPUT
    Output : REAL;
END_VAR
    Output := SetPoint - ProcessValue;
END_FUNCTION_BLOCK
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let fb = chunks.iter().find(|c| c.name == "PID_Controller").unwrap();
        assert_eq!(fb.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_function() {
        let content = r#"
FUNCTION CalculateChecksum : INT
VAR_INPUT
    Length : INT;
END_VAR
VAR
    Sum : INT;
END_VAR
    Sum := 0;
    CalculateChecksum := Sum;
END_FUNCTION
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let f = chunks.iter().find(|c| c.name == "CalculateChecksum").unwrap();
        assert_eq!(f.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_program() {
        let content = r#"
PROGRAM Main
VAR
    Temperature : REAL;
END_VAR
    Temperature := 72.5;
END_PROGRAM
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let p = chunks.iter().find(|c| c.name == "Main").unwrap();
        assert_eq!(p.chunk_type, ChunkType::Module);
    }

    #[test]
    fn parse_type_definition() {
        let content = r#"
TYPE MotorState :
STRUCT
    Speed : REAL;
    Running : BOOL;
    Direction : INT;
END_STRUCT;
END_TYPE
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let t = chunks.iter().find(|c| c.name == "MotorState").unwrap();
        assert_eq!(t.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_call_graph() {
        let content = r#"
PROGRAM Main
VAR
    PID1 : PID_Controller;
    Temp : REAL;
END_VAR
    PID1(SetPoint := 72.5, ProcessValue := Temp);
END_PROGRAM
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let main = chunks.iter().find(|c| c.name == "Main").unwrap();
        // PID1 call should be in the chunk content
        assert!(main.content.contains("PID1"));
    }

    #[test]
    fn parse_method_definition() {
        let content = r#"
FUNCTION_BLOCK Motor
METHOD PUBLIC Start : BOOL
VAR_INPUT
    Speed : REAL;
END_VAR
    Start := Speed > 0.0;
END_METHOD
END_FUNCTION_BLOCK
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "Start").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }

    #[test]
    fn parse_action_definition() {
        let content = r#"
FUNCTION_BLOCK Controller
ACTION ResetCounters
    Counter1 := 0;
    Counter2 := 0;
END_ACTION
END_FUNCTION_BLOCK
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let action = chunks.iter().find(|c| c.name == "ResetCounters").unwrap();
        // ACTION inside FUNCTION_BLOCK is treated as a method (parent container inference)
        assert_eq!(action.chunk_type, ChunkType::Method);
    }

    #[test]
    fn parse_type_references_in_var_input() {
        let content = r#"
FUNCTION_BLOCK Conveyor
VAR_INPUT
    Speed : REAL;
    Sensor : ProximitySensor;
END_VAR
    (* control logic *)
END_FUNCTION_BLOCK
"#;
        let file = write_temp_file(content, "st");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let fb = chunks.iter().find(|c| c.name == "Conveyor").unwrap();
        assert_eq!(fb.chunk_type, ChunkType::Class);
        // Typed VAR_INPUT declarations should be in chunk content
        assert!(fb.content.contains("ProximitySensor"));
    }
}
