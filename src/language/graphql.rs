//! GraphQL language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting GraphQL definitions.
///
/// Object/Input types → Struct, Interfaces → Interface, Enums → Enum,
/// Unions/Scalars → TypeAlias, Directives → Macro, Operations/Fragments → Function.
const CHUNK_QUERY: &str = r#"
;; Object types (type User { ... })
(object_type_definition
  (name) @name) @struct

;; Interface types (interface Node { ... })
(interface_type_definition
  (name) @name) @interface

;; Enum types (enum Status { ... })
(enum_type_definition
  (name) @name) @enum

;; Union types (union SearchResult = User | Post)
(union_type_definition
  (name) @name) @typealias

;; Input types (input CreateUserInput { ... })
(input_object_type_definition
  (name) @name) @struct

;; Scalar types (scalar DateTime)
(scalar_type_definition
  (name) @name) @typealias

;; Directive definitions (@directive ...)
(directive_definition
  (name) @name) @macro

;; Operations (query GetUser { ... }, mutation CreateUser { ... })
(operation_definition
  (name) @name) @function

;; Fragments (fragment UserFields on User { ... })
(fragment_definition
  (fragment_name
    (name) @name)) @function
"#;

/// Tree-sitter query for extracting type references in GraphQL.
///
/// `named_type` appears in field types, argument types, and type conditions.
const CALL_QUERY: &str = r#"
;; Named type references
(named_type
  (name) @callee)
"#;

/// Doc comment node types — GraphQL uses `description` (triple-quoted strings)
const DOC_NODES: &[&str] = &["description"];

const STOPWORDS: &[&str] = &[
    "type", "interface", "enum", "union", "input", "scalar", "directive", "query", "mutation",
    "subscription", "fragment", "on", "extend", "implements", "schema", "true", "false", "null",
    "repeatable",
];

const COMMON_TYPES: &[&str] = &["String", "Int", "Float", "Boolean", "ID"];

static DEFINITION: LanguageDef = LanguageDef {
    name: "graphql",
    grammar: Some(|| tree_sitter_graphql::LANGUAGE.into()),
    extensions: &["graphql", "gql"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    type_query: None,
    common_types: COMMON_TYPES,
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
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
    fn parse_graphql_object_type() {
        let content = r#"
type User {
  id: ID!
  name: String!
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let user = chunks.iter().find(|c| c.name == "User").unwrap();
        assert_eq!(user.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_graphql_interface() {
        let content = r#"
interface Node {
  id: ID!
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let node = chunks.iter().find(|c| c.name == "Node").unwrap();
        assert_eq!(node.chunk_type, ChunkType::Interface);
    }

    #[test]
    fn parse_graphql_enum() {
        let content = r#"
enum Status {
  ACTIVE
  INACTIVE
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks.iter().find(|c| c.name == "Status").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }

    #[test]
    fn parse_graphql_union() {
        let content = "union SearchResult = User | Post\n";
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let u = chunks.iter().find(|c| c.name == "SearchResult").unwrap();
        assert_eq!(u.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_graphql_input() {
        let content = r#"
input CreateUserInput {
  name: String!
  email: String!
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let input = chunks.iter().find(|c| c.name == "CreateUserInput").unwrap();
        assert_eq!(input.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_graphql_scalar() {
        let content = "scalar DateTime\n";
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "DateTime").unwrap();
        assert_eq!(s.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_graphql_directive() {
        let content = "directive @auth(requires: Role!) on FIELD_DEFINITION\n";
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let d = chunks.iter().find(|c| c.name == "auth").unwrap();
        assert_eq!(d.chunk_type, ChunkType::Macro);
    }

    #[test]
    fn parse_graphql_operation() {
        let content = r#"
query GetUser($id: ID!) {
  user(id: $id) {
    name
  }
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let op = chunks.iter().find(|c| c.name == "GetUser").unwrap();
        assert_eq!(op.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_graphql_fragment() {
        let content = r#"
fragment UserFields on User {
  name
  email
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let frag = chunks.iter().find(|c| c.name == "UserFields").unwrap();
        assert_eq!(frag.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_graphql_calls() {
        let content = r#"
type User {
  id: ID!
  posts: [Post!]!
  address: Address
}
"#;
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let user = chunks.iter().find(|c| c.name == "User").unwrap();
        let calls = parser.extract_calls_from_chunk(user);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"Post"),
            "Expected Post type reference, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Address"),
            "Expected Address type reference, got: {:?}",
            names
        );
    }
}
