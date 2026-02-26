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
