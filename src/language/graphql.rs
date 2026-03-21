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
    /// Parses a GraphQL object type definition and verifies it is correctly identified as a struct chunk.
    /// 
    /// This test function writes a GraphQL type definition to a temporary file, parses it using the Parser, and asserts that the resulting chunk for the "User" type is recognized as a struct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates independently.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that asserts expectations.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, the file cannot be parsed, the "User" chunk is not found in the parsed results, or the chunk type is not `ChunkType::Struct`.

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
    /// Parses a GraphQL interface definition from a temporary file and verifies the parser correctly identifies it as an Interface chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This function is a self-contained test that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that uses assertions to verify parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to parse the file, if a chunk named "Node" is not found in the parsed results, or if the parsed chunk's type is not `ChunkType::Interface`.

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
    /// Parses a GraphQL enum definition and verifies the parser correctly identifies it as an enum type.
    /// 
    /// This test function writes a GraphQL enum definition to a temporary file, parses it using the Parser, and asserts that the resulting chunk has the name "Status" and type ChunkType::Enum.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded GraphQL content.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - No chunk named "Status" is found in the parsed results
    /// - The chunk type is not ChunkType::Enum

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
    /// Parses a GraphQL union type definition and verifies it is correctly identified as a type alias chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate the parsing behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "SearchResult" chunk is not found in the parsed results, or the chunk type assertion fails.

    #[test]
    fn parse_graphql_union() {
        let content = "union SearchResult = User | Post\n";
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let u = chunks.iter().find(|c| c.name == "SearchResult").unwrap();
        assert_eq!(u.chunk_type, ChunkType::TypeAlias);
    }
    /// Parses a GraphQL input type definition and verifies the parser correctly identifies it as a struct chunk.
    /// 
    /// This is a test function that creates a temporary GraphQL file containing an input type definition, parses it using the Parser, and asserts that the resulting chunk is properly identified with the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None. This function takes no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function returns unit type and is intended for testing purposes.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser fails to parse the file
    /// - The "CreateUserInput" chunk is not found in the parsed results
    /// - The chunk type is not `ChunkType::Struct`

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
    /// Parses a GraphQL scalar type definition and verifies it is correctly identified as a type alias.
    /// 
    /// This is a test function that writes a GraphQL scalar declaration to a temporary file, parses it using the Parser, and asserts that the resulting chunk has the name "DateTime" and chunk type of TypeAlias.
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
    /// Panics if the temporary file cannot be created, the file cannot be parsed, the "DateTime" chunk is not found in the parsed results, or the chunk type is not TypeAlias.

    #[test]
    fn parse_graphql_scalar() {
        let content = "scalar DateTime\n";
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "DateTime").unwrap();
        assert_eq!(s.chunk_type, ChunkType::TypeAlias);
    }
    /// Parses a GraphQL directive definition and verifies it is correctly identified as a macro chunk.
    /// 
    /// This test function writes a GraphQL directive to a temporary file, parses it using the Parser, and asserts that the resulting chunk is named "auth" and has the type ChunkType::Macro.
    /// 
    /// # Arguments
    /// 
    /// None - this is a standalone test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None - this is a test function that asserts conditions but does not return a value.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following conditions fail:
    /// - Creating the temporary file fails
    /// - Parsing the file fails
    /// - Finding a chunk named "auth" fails
    /// - The chunk type is not ChunkType::Macro

    #[test]
    fn parse_graphql_directive() {
        let content = "directive @auth(requires: Role!) on FIELD_DEFINITION\n";
        let file = write_temp_file(content, "graphql");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let d = chunks.iter().find(|c| c.name == "auth").unwrap();
        assert_eq!(d.chunk_type, ChunkType::Macro);
    }
    /// Parses a GraphQL query operation from a temporary file and verifies the parser correctly identifies it as a function chunk.
    /// 
    /// This is a test function that writes a GraphQL query named "GetUser" to a temporary file, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None. This function takes no parameters.
    /// 
    /// # Returns
    /// 
    /// Nothing. This function returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following operations fail:
    /// - Creating a new Parser instance
    /// - Parsing the temporary file
    /// - Finding a chunk named "GetUser" in the parsed results
    /// - The assertion that the chunk type equals `ChunkType::Function`

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
    /// Parses a GraphQL fragment definition from a temporary file and verifies it is correctly identified as a function chunk.
    /// 
    /// This test function creates a temporary file containing a GraphQL fragment definition, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function with no return value.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to initialize, if parsing the file fails, if the "UserFields" fragment is not found in the parsed chunks, or if the chunk type is not `ChunkType::Function`.

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
    /// Parses a GraphQL type definition and extracts type references from its fields.
    /// 
    /// This function creates a temporary GraphQL file containing a User type definition with nested type references, parses it using the Parser, and verifies that type references (Post and Address) are correctly extracted from the User chunk's fields.
    /// 
    /// # Returns
    /// 
    /// Returns nothing; validates extracted type references through assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize, the GraphQL file cannot be parsed, the User chunk is not found, or if the expected type references (Post and Address) are not extracted from the User type's fields.

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
