//! Solidity language definition

use super::{FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Solidity code chunks
const CHUNK_QUERY: &str = r#"
;; Contracts
(contract_declaration
  name: (identifier) @name
  body: (contract_body)) @class

;; Interfaces
(interface_declaration
  name: (identifier) @name
  body: (contract_body)) @interface

;; Libraries
(library_declaration
  name: (identifier) @name
  body: (contract_body)) @module

;; Structs
(struct_declaration
  name: (identifier) @name
  body: (struct_body)) @struct

;; Enums
(enum_declaration
  name: (identifier) @name
  body: (enum_body)) @enum

;; Functions
(function_definition
  name: (identifier) @name) @function

;; Modifiers
(modifier_definition
  name: (identifier) @name) @function

;; Events
(event_definition
  name: (identifier) @name) @event

;; State variables
(state_variable_declaration
  name: (identifier) @name) @property

;; Errors (custom error types)
(error_declaration
  name: (identifier) @name) @struct
"#;

/// Tree-sitter query for extracting function calls
/// Note: Solidity grammar uses supertype `expression` for the `function` field
/// in `call_expression`, so `function: (identifier)` and `function: (member_expression)`
/// fail with Structure errors. We use two patterns:
/// 1. member_expression → capture just the property (method name)
/// 2. call_expression function: (_) → capture whole callee (works for direct calls;
///    member calls captured above get the whole `obj.method` text, but dedup
///    means the first pattern's clean capture wins)
const CALL_QUERY: &str = r#"
;; Member function call — token.transfer() → captures "transfer"
(member_expression
  property: (identifier) @callee)

;; All function calls — captures the full callee expression
;; For direct calls like require(), this captures "require"
;; For member calls, this captures "token.transfer" (deduped with above)
(call_expression
  function: (_) @callee)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "return", "break", "continue",
    "contract", "interface", "library", "struct", "enum", "function", "modifier",
    "event", "error", "mapping", "address", "bool", "string", "bytes", "uint",
    "int", "uint256", "int256", "uint8", "bytes32", "public", "private",
    "internal", "external", "view", "pure", "payable", "memory", "storage",
    "calldata", "indexed", "virtual", "override", "abstract", "immutable",
    "constant", "emit", "require", "assert", "revert", "this", "super",
    "true", "false", "msg", "block", "tx",
];

/// Extracts the return type information from a Solidity function signature.
/// Parses a Solidity function signature to find the `returns` clause and extracts the return type specification. Tokenizes the return type declaration and formats it as a human-readable string.
/// # Arguments
/// * `signature` - A Solidity function signature string (e.g., "function add(uint a, uint b) public pure returns (uint)")
/// # Returns
/// `Some(String)` containing the formatted return type as "Returns <type>" if a `returns` clause exists and contains a non-empty type specification, or `None` if no `returns` clause is found or it is empty.
fn extract_return(signature: &str) -> Option<String> {
    // Solidity: returns (...) at end of function signature
    // e.g., "function add(uint a, uint b) public pure returns (uint)"
    if let Some(ret_idx) = signature.find("returns") {
        let after = signature[ret_idx + 7..].trim();
        // Strip parens
        let inner = after
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim_end_matches('{')
            .trim();
        if !inner.is_empty() {
            let ret_words = crate::nl::tokenize_identifier(inner).join(" ");
            return Some(format!("Returns {}", ret_words));
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "solidity",
    grammar: Some(|| tree_sitter_solidity::LANGUAGE.into()),
    extensions: &["sol"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["contract_body"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,
    common_types: &[
        "address", "bool", "string", "bytes", "uint256", "int256", "uint8", "uint16",
        "uint32", "uint64", "uint128", "int8", "int16", "int32", "int64", "int128",
        "bytes32", "bytes4", "bytes20",
    ],
    container_body_kinds: &["contract_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &["%/test/%", "%.t.sol"],
    structural_matchers: None,
    entry_point_names: &["constructor", "receive", "fallback"],
    trait_method_names: &[],
    injections: &[],
    doc_format: "javadoc",
    doc_convention: "Use NatSpec format: @param, @return, @dev tags.",
    field_style: FieldStyle::NameFirst {
        separators: ";",
        strip_prefixes: "public private internal constant immutable",
    },
    skip_line_prefixes: &["contract ", "struct ", "enum ", "interface "],
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
    fn parse_solidity_contract() {
        let content = r#"
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract Token {
    string public name;
    uint256 public totalSupply;

    function transfer(address to, uint256 amount) public returns (bool) {
        return true;
    }
}
"#;
        let file = write_temp_file(content, "sol");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let contract = chunks.iter().find(|c| c.name == "Token").unwrap();
        assert_eq!(contract.chunk_type, ChunkType::Class);
        let func = chunks.iter().find(|c| c.name == "transfer").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Method);
        assert_eq!(func.parent_type_name.as_deref(), Some("Token"));
    }

    #[test]
    fn parse_solidity_interface() {
        let content = r#"
interface IERC20 {
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
}
"#;
        let file = write_temp_file(content, "sol");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let iface = chunks.iter().find(|c| c.name == "IERC20").unwrap();
        assert_eq!(iface.chunk_type, ChunkType::Interface);
    }

    #[test]
    fn parse_solidity_calls() {
        let content = r#"
contract Caller {
    function doWork() public {
        token.transfer(msg.sender, 100);
        require(true, "failed");
    }
}
"#;
        let file = write_temp_file(content, "sol");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "doWork").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"transfer"), "Expected transfer, got: {:?}", names);
        assert!(names.contains(&"require"), "Expected require, got: {:?}", names);
    }

    #[test]
    fn parse_solidity_struct_and_enum() {
        let content = r#"
struct Position {
    uint256 x;
    uint256 y;
}

enum Status { Active, Paused, Stopped }
"#;
        let file = write_temp_file(content, "sol");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "Position").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
        let e = chunks.iter().find(|c| c.name == "Status").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }

    #[test]
    fn parse_solidity_event() {
        let content = r#"
contract Token {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
}
"#;
        let file = write_temp_file(content, "sol");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let transfer = chunks.iter().find(|c| c.name == "Transfer").unwrap();
        assert_eq!(transfer.chunk_type, ChunkType::Event);
        let approval = chunks.iter().find(|c| c.name == "Approval").unwrap();
        assert_eq!(approval.chunk_type, ChunkType::Event);
    }

    #[test]
    fn test_extract_return_solidity() {
        assert_eq!(
            extract_return("function add(uint a, uint b) public pure returns (uint)"),
            Some("Returns uint".to_string())
        );
        assert_eq!(
            extract_return("function doSomething() public"),
            None
        );
    }
}
