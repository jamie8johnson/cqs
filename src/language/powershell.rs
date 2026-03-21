//! PowerShell language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting PowerShell code chunks
const CHUNK_QUERY: &str = r#"
;; Functions
(function_statement
  (function_name) @name) @function

;; Classes
(class_statement
  (simple_name) @name) @class

;; Class methods
(class_method_definition
  (simple_name) @name) @function

;; Class properties
(class_property_definition
  (variable) @name) @property

;; Enums
(enum_statement
  (simple_name) @name) @enum
"#;

/// Tree-sitter query for extracting PowerShell function calls
const CALL_QUERY: &str = r#"
;; Command calls: Get-Process, Invoke-WebRequest, etc.
(command
  command_name: (command_name) @callee)

;; .NET method invocations: $obj.Method()
;; Note: grammar uses "invokation" (typo in grammar, not our code)
(invokation_expression
  (member_name
    (simple_name) @callee))

;; Member access: $obj.Property or [Type]::StaticMethod
(member_access
  (member_name
    (simple_name) @callee))
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "param", "begin", "process", "end", "if", "else", "elseif", "switch", "for",
    "foreach", "while", "do", "until", "try", "catch", "finally", "throw", "return", "exit",
    "break", "continue", "class", "enum", "using", "namespace", "hidden", "static", "void", "new",
    "true", "false", "null",
];

const COMMON_TYPES: &[&str] = &[
    "string", "int", "bool", "object", "void", "double", "float", "long", "byte", "char",
    "decimal", "array", "hashtable", "PSObject", "PSCustomObject", "ScriptBlock", "DateTime",
    "TimeSpan", "Guid", "IPAddress", "SecureString", "PSCredential", "ErrorRecord",
];

/// Extracts the return type from a PowerShell function signature.
/// 
/// # Arguments
/// 
/// * `signature` - A PowerShell function signature string to parse
/// 
/// # Returns
/// 
/// Returns `None` because PowerShell function signatures do not include explicit return type annotations.
fn extract_return(_signature: &str) -> Option<String> {
    // PowerShell doesn't have return type syntax in function signatures
    None
}

/// Extract container type name for PowerShell classes.
/// `class_statement` stores the name in a `simple_name` child (no "name" field).
fn extract_container_name_ps(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "simple_name" {
            return Some(source[child.byte_range()].to_string());
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "powershell",
    grammar: Some(|| tree_sitter_powershell::LANGUAGE.into()),
    extensions: &["ps1", "psm1"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_statement"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.Tests.ps1")),
    type_query: None,
    common_types: COMMON_TYPES,
    container_body_kinds: &[],
    extract_container_name: Some(extract_container_name_ps),
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["Describe ", "It ", "Context "],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%.Tests.ps1"],
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
    /// Parses a PowerShell function definition and verifies the parser correctly identifies it as a Function chunk type.
    /// 
    /// This is a unit test that creates a temporary PowerShell file containing a function definition, parses it using the Parser, and asserts that the resulting chunk is properly recognized as a Function with the expected name.
    /// 
    /// # Arguments
    /// 
    /// None. This is a self-contained test function.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to parse the file, the function chunk cannot be found, or the chunk type is not Function.

    #[test]
    fn parse_powershell_function() {
        let content = r#"
function Get-UserInfo {
    param([string]$Name)
    Write-Output "Hello $Name"
}
"#;
        let file = write_temp_file(content, "ps1");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "Get-UserInfo").unwrap();
        assert_eq!(func.chunk_type, crate::parser::ChunkType::Function);
    }
    /// Parses a PowerShell class definition and verifies it is correctly identified as a Class chunk.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts the parsing result rather than returning a value.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize, file parsing fails, the Calculator class chunk is not found, or the chunk type is not Class.

    #[test]
    fn parse_powershell_class() {
        let content = r#"
class Calculator {
    [int] Add([int]$a, [int]$b) {
        return $a + $b
    }
}
"#;
        let file = write_temp_file(content, "ps1");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
        assert_eq!(class.chunk_type, crate::parser::ChunkType::Class);
    }
    /// Verifies that the parser correctly identifies and extracts PowerShell class methods with their associated metadata.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded PowerShell class content.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any assertion fails, indicating the parser did not correctly identify the method name, chunk type, or parent class name.

    #[test]
    fn parse_powershell_method() {
        let content = r#"
class Calculator {
    [int] Add([int]$a, [int]$b) {
        return $a + $b
    }
}
"#;
        let file = write_temp_file(content, "ps1");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "Add").unwrap();
        assert_eq!(method.chunk_type, crate::parser::ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }
    /// Tests that the parser correctly identifies and extracts PowerShell class properties.
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
    /// Panics if the parser fails to create a temporary file, parse the file, find the expected "Name" property chunk, or if the chunk type assertion fails.

    #[test]
    fn parse_powershell_property() {
        let content = r#"
class Person {
    [string]$Name
    [int]$Age
}
"#;
        let file = write_temp_file(content, "ps1");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let prop = chunks
            .iter()
            .find(|c| c.name.contains("Name") && c.chunk_type == crate::parser::ChunkType::Property)
            .unwrap();
        assert_eq!(prop.chunk_type, crate::parser::ChunkType::Property);
    }
    /// Tests parsing of PowerShell enum declarations from a file.
    /// 
    /// # Arguments
    /// 
    /// This function takes no arguments.
    /// 
    /// # Returns
    /// 
    /// This function returns nothing (unit type). It is a test function that verifies parser behavior through assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - An enum named "Color" is not found in the parsed chunks
    /// - The found chunk is not of type `ChunkType::Enum`

    #[test]
    fn parse_powershell_enum() {
        let content = r#"
enum Color {
    Red
    Green
    Blue
}
"#;
        let file = write_temp_file(content, "ps1");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let en = chunks.iter().find(|c| c.name == "Color").unwrap();
        assert_eq!(en.chunk_type, crate::parser::ChunkType::Enum);
    }
    /// Parses PowerShell code and verifies that function calls are correctly extracted.
    /// 
    /// This is a test function that creates a PowerShell code sample containing a function definition with a Get-Process cmdlet call and a static method invocation. It then uses a Parser to extract all calls from the code and asserts that the "Get-Process" call is found in the extracted results.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if the Parser fails to initialize or if the "Get-Process" call is not found in the extracted calls, with an assertion message showing the actual calls that were found.

    #[test]
    fn parse_powershell_calls() {
        let content = r#"
function Process-Data {
    Get-Process -Name "foo"
    $result = [System.IO.File]::ReadAllText("test.txt")
}
"#;
        let parser = Parser::new().unwrap();
        let lang = crate::parser::Language::PowerShell;
        let calls = parser.extract_calls(content, lang, 0, content.len(), 0);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"Get-Process"),
            "Expected Get-Process call, got: {:?}",
            names
        );
    }
}
