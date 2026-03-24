//! Protobuf language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Protobuf code chunks.
///
/// Messages → Struct, Services → Interface, RPCs → Function (reclassified to Method
/// when inside a service via `method_containers`), Enums → Enum.
const CHUNK_QUERY: &str = r#"
;; Messages
(message
  (message_name
    (identifier) @name)) @struct

;; Services
(service
  (service_name
    (identifier) @name)) @interface

;; RPCs (inside services → Method via method_containers)
(rpc
  (rpc_name
    (identifier) @name)) @function

;; Enums
(enum
  (enum_name
    (identifier) @name)) @enum
"#;

/// Tree-sitter query for extracting type references in Protobuf.
///
/// `message_or_enum_type` appears in field types and RPC input/output types.
const CALL_QUERY: &str = r#"
;; Type references in fields and RPCs
(message_or_enum_type) @callee
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "syntax", "package", "import", "option", "message", "service", "rpc", "enum", "oneof", "map",
    "repeated", "optional", "required", "reserved", "returns", "stream", "extend", "true", "false",
    "string", "bytes", "bool", "int32", "int64", "uint32", "uint64", "sint32", "sint64", "fixed32",
    "fixed64", "sfixed32", "sfixed64", "float", "double", "google",
];

/// Extract service name from a service node.
///
/// The proto grammar uses `service_name` children (not a `name` field),
/// so the default container name extractor won't work.
fn extract_container_name(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "service_name" {
            return Some(source[child.byte_range()].to_string());
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "protobuf",
    grammar: Some(|| tree_sitter_proto::LANGUAGE.into()),
    extensions: &["proto"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["service"],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: Some(extract_container_name),
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
    injections: &[],
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
    /// Parses a protobuf3 message definition and verifies the parser correctly identifies it as a struct chunk.
    /// 
    /// This test function creates a temporary protobuf file containing a User message definition, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (unit test function)
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, the file cannot be parsed, the User message chunk is not found, or the chunk type assertion fails.

    #[test]
    fn parse_proto_message() {
        let content = r#"
syntax = "proto3";

message User {
  string name = 1;
  int32 age = 2;
}
"#;
        let file = write_temp_file(content, "proto");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let msg = chunks.iter().find(|c| c.name == "User").unwrap();
        assert_eq!(msg.chunk_type, ChunkType::Struct);
    }
    /// Parses a proto3 service definition and verifies it is correctly identified as an Interface chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This function is a self-contained test that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function is a test assertion helper.
    /// 
    /// # Panics
    /// 
    /// Panics if the proto file cannot be written, if parsing fails, or if the UserService is not found in the parsed chunks with ChunkType::Interface.

    #[test]
    fn parse_proto_service() {
        let content = r#"
syntax = "proto3";

service UserService {
  rpc GetUser (GetUserRequest) returns (GetUserResponse);
}
"#;
        let file = write_temp_file(content, "proto");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let svc = chunks
            .iter()
            .find(|c| c.name == "UserService" && c.chunk_type == ChunkType::Interface);
        assert!(svc.is_some(), "Should find 'UserService' as Interface");
    }
    /// Parses a protobuf file containing RPC service definitions and verifies that RPC methods are correctly identified with their associated service.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser fails to initialize
    /// - The parser fails to parse the proto file
    /// - The "GetUser" RPC method is not found in the parsed chunks
    /// - The parsed RPC method does not have `ChunkType::Method` type
    /// - The parsed RPC method's parent is not "UserService"

    #[test]
    fn parse_proto_rpc() {
        let content = r#"
syntax = "proto3";

service UserService {
  rpc GetUser (GetUserRequest) returns (GetUserResponse);
  rpc ListUsers (ListUsersRequest) returns (stream User);
}
"#;
        let file = write_temp_file(content, "proto");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let rpc = chunks.iter().find(|c| c.name == "GetUser").unwrap();
        assert_eq!(rpc.chunk_type, ChunkType::Method);
        assert_eq!(rpc.parent_type_name.as_deref(), Some("UserService"));
    }
    /// Verifies that the parser correctly identifies and parses a protobuf enum definition from a proto3 file.
    /// 
    /// This is a test function that creates a temporary proto file containing an enum definition, parses it using the Parser, and asserts that the resulting chunks contain the expected enum with the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a standalone test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the file, or fails to find a chunk named "Status" with `ChunkType::Enum` in the parsed results.

    #[test]
    fn parse_proto_enum() {
        let content = r#"
syntax = "proto3";

enum Status {
  UNKNOWN = 0;
  ACTIVE = 1;
  INACTIVE = 2;
}
"#;
        let file = write_temp_file(content, "proto");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks
            .iter()
            .find(|c| c.name == "Status" && c.chunk_type == ChunkType::Enum);
        assert!(e.is_some(), "Should find 'Status' as Enum");
    }
    /// Parses a Protocol Buffer file and verifies that message type references are correctly extracted.
    /// 
    /// This is a test function that creates a temporary proto3 file with nested message types, parses it to extract chunks, and validates that cross-message references (specifically that User references Address) are properly identified in the call extraction process.
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
    /// Panics if the temporary file cannot be written, the parser cannot be initialized, the proto file cannot be parsed, the User message chunk is not found, or the Address type reference is not found in the extracted calls.

    #[test]
    fn parse_proto_calls() {
        let content = r#"
syntax = "proto3";

message User {
  string name = 1;
  Address address = 2;
}

message Address {
  string street = 1;
}
"#;
        let file = write_temp_file(content, "proto");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let user = chunks.iter().find(|c| c.name == "User").unwrap();
        let calls = parser.extract_calls_from_chunk(user);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"Address"),
            "Expected Address type reference, got: {:?}",
            names
        );
    }
}
