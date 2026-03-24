//! VB.NET language definition
//!
//! Visual Basic .NET (.vb files). Grammar: `CodeAnt-AI/tree-sitter-vb-dotnet` — covers
//! classes, modules, structures, interfaces, enums, methods, properties, events, delegates.
//!
//! VB.NET uses `Sub` (void return) and `Function` (typed return) for methods,
//! `Module` instead of `static class`, and `Structure` instead of `struct`.

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting VB.NET code chunks.
///
/// Grammar node types differ from C#:
/// - `class_block` (not `class_declaration`)
/// - `module_block` (VB.NET Module = static class)
/// - `structure_block` (not `struct_declaration`)
/// - `interface_block` (not `interface_declaration`)
/// - `enum_block` (not `enum_declaration`)
/// - `method_declaration` (covers both Sub and Function)
/// - `constructor_declaration` (Sub New)
const CHUNK_QUERY: &str = r#"
;; Methods (Sub and Function)
(method_declaration name: (identifier) @name) @function

;; Constructors (Sub New — name field is "New")
(constructor_declaration) @function

;; Properties
(property_declaration name: (identifier) @name) @property

;; Fields
(field_declaration
  (variable_declarator (identifier) @name)) @property

;; Constants
(const_declaration
  (variable_declarator (identifier) @name)) @constant

;; Events
(event_declaration name: (identifier) @name) @event

;; Delegates
(delegate_declaration name: (identifier) @name) @delegate

;; Types
(class_block name: (identifier) @name) @class
(module_block name: (identifier) @name) @module
(structure_block name: (identifier) @name) @struct
(interface_block name: (identifier) @name) @interface
(enum_block name: (identifier) @name) @enum
"#;

/// Tree-sitter query for extracting VB.NET function calls.
///
/// VB.NET grammar uses `invocation` (not `invocation_expression`) and
/// `member_access` (not `member_access_expression`). Field is `target` not `function`.
const CALL_QUERY: &str = r#"
;; Method calls: obj.Method(args)
(invocation
  target: (member_access
    member: (identifier) @callee))

;; Bare calls: Method(args)
(invocation
  target: (identifier) @callee)

;; Object creation: New ClassName(args) / New ClassName()
(new_expression
  type: (type (namespace_name (identifier) @callee)))
(new_expression
  type: (type (generic_type (namespace_name (identifier) @callee))))
"#;

/// Tree-sitter query for extracting VB.NET type references.
///
/// VB.NET uses `as_clause` for type annotations. The grammar wraps type names in
/// `type` → `namespace_name` → `identifier` (even for simple names like `Integer`).
/// `as_clause` has a `type:` field. Simple type names go through `namespace_name`.
const TYPE_QUERY: &str = r#"
;; Param — method parameters (ByVal/ByRef p As Type)
(parameter
  (as_clause type: (type (namespace_name (identifier) @param_type))))
(parameter
  (as_clause type: (type (generic_type (namespace_name (identifier) @param_type)))))

;; Return — Function ... As Type (return_type field is type, not as_clause)
(method_declaration
  return_type: (type (namespace_name (identifier) @return_type)))
(method_declaration
  return_type: (type (generic_type (namespace_name (identifier) @return_type))))

;; Field — Dim/Private field As Type
(field_declaration
  (variable_declarator
    (as_clause type: (type (namespace_name (identifier) @field_type)))))
(field_declaration
  (variable_declarator
    (as_clause type: (type (generic_type (namespace_name (identifier) @field_type))))))

;; Property — Property Name As Type
(property_declaration
  (as_clause type: (type (namespace_name (identifier) @field_type))))
(property_declaration
  (as_clause type: (type (generic_type (namespace_name (identifier) @field_type)))))

;; Impl — Inherits / Implements
(inherits_clause (type (namespace_name (identifier) @impl_type)))
(inherits_clause (type (generic_type (namespace_name (identifier) @impl_type))))
(implements_clause (type (namespace_name (identifier) @impl_type)))
(implements_clause (type (generic_type (namespace_name (identifier) @impl_type))))

;; Bound — generic type constraint (Of T As IFoo)
(type_constraint (type (namespace_name (identifier) @bound_type)))
(type_constraint (type (generic_type (namespace_name (identifier) @bound_type))))

;; Imports
(imports_statement namespace: (namespace_name (identifier) @alias_type))
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    // VB.NET keywords
    "public", "private", "protected", "friend", "shared", "readonly", "mustinherit", "notinheritable",
    "mustoverride", "overridable", "overrides", "overloads", "shadows",
    "class", "module", "structure", "interface", "enum", "namespace",
    "imports", "return", "if", "then", "else", "elseif", "end", "for", "each", "next",
    "while", "do", "loop", "select", "case", "exit", "continue",
    "new", "me", "mybase", "myclass", "try", "catch", "finally", "throw",
    "dim", "as", "sub", "function", "property", "event", "delegate",
    "integer", "string", "boolean", "double", "single", "long", "byte", "char",
    "decimal", "short", "object", "true", "false", "nothing", "void",
    "get", "set", "value", "where", "partial", "of", "in", "out",
    "byval", "byref", "optional", "paramarray", "handles", "withevents",
    "addhandler", "removehandler", "raiseevent",
    "not", "and", "or", "andalso", "orelse", "xor", "mod", "like", "is", "isnot",
    "with", "using", "synclock", "redim", "preserve", "goto",
];

const COMMON_TYPES: &[&str] = &[
    "String", "Integer", "Boolean", "Object", "Double", "Single", "Long", "Byte", "Char",
    "Decimal", "Short", "UInteger", "ULong", "Task", "ValueTask", "List", "Dictionary", "HashSet",
    "Queue", "Stack", "IEnumerable", "IList", "IDictionary", "ICollection", "IQueryable", "Action",
    "Func", "Predicate", "EventHandler", "EventArgs", "IDisposable", "CancellationToken", "ILogger",
    "StringBuilder", "Exception", "Nullable",
];

/// Post-process: assign "New" name to constructor chunks and reclassify as Constructor.
fn post_process_vbnet(
    name: &mut String,
    kind: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if node.kind() == "constructor_declaration" {
        *name = "New".to_string();
        *kind = ChunkType::Constructor;
    }
    true
}

/// Extracts the return type from a VB.NET function signature and formats it as a documentation string.
/// 
/// Parses a VB.NET function signature to find the return type specified after the closing parenthesis with the "As" keyword. If found, tokenizes and formats the return type as a "Returns" statement.
/// 
/// # Arguments
/// 
/// * `signature` - A VB.NET function signature string to parse
/// 
/// # Returns
/// 
/// `Some(String)` containing the formatted return type as "Returns {type}" if a return type is found after "As" keyword, or `None` if no return type is present or the signature format is invalid.
fn extract_return(signature: &str) -> Option<String> {
    // VB.NET: Function Name(...) As ReturnType
    // Look for "As" after the closing paren
    if let Some(paren_close) = signature.rfind(')') {
        let after = signature[paren_close + 1..].trim();
        if let Some(rest) = after.strip_prefix("As").or_else(|| after.strip_prefix("as")) {
            let ret_type = rest.split_whitespace().next()?;
            if !ret_type.is_empty() {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "vbnet",
    grammar: Some(|| tree_sitter_vb_dotnet::LANGUAGE.into()),
    extensions: &["vb"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[
        "class_block",
        "module_block",
        "structure_block",
        "interface_block",
    ],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.vb")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_vbnet),
    test_markers: &["<Test>", "<Fact>", "<Theory>", "<TestMethod>"],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%Tests.vb"],
    structural_matchers: None,
    entry_point_names: &["Main"],
    trait_method_names: &[
        "Equals", "GetHashCode", "ToString", "CompareTo", "Dispose",
        "GetEnumerator", "MoveNext",
    ],
    injections: &[],
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{types::ChunkType, Parser};
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
    /// Tests that the parser correctly identifies and categorizes VB.NET class definitions and their methods.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded VB.NET source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following assertions fail:
    /// - The parser fails to identify the "Calculator" class chunk
    /// - The parser fails to identify the "New" constructor method chunk
    /// - The parser fails to identify the "Add" method chunk
    /// - The parser fails to identify the "Reset" method chunk

    #[test]
    fn parse_vbnet_class_with_methods() {
        let content = r#"
Public Class Calculator
    Private _value As Integer

    Public Sub New()
        _value = 0
    End Sub

    Public Function Add(a As Integer, b As Integer) As Integer
        Return a + b
    End Function

    Public Sub Reset()
        _value = 0
    End Sub
End Class
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();

        assert!(
            names.iter().any(|(n, t)| *n == "Calculator" && *t == ChunkType::Class),
            "Expected 'Calculator' class, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|(n, t)| *n == "New" && *t == ChunkType::Constructor),
            "Expected 'New' constructor, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|(n, t)| *n == "Add" && *t == ChunkType::Method),
            "Expected 'Add' method, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|(n, t)| *n == "Reset" && *t == ChunkType::Method),
            "Expected 'Reset' method, got: {:?}",
            names
        );
    }
    /// Tests the parser's ability to correctly parse VB.NET module structures, including module declarations, subroutines, and functions. Creates a temporary VB.NET file containing a module with a Main subroutine and an Add function, parses it using the Parser, and verifies that all three top-level definitions (Program module, Main sub, and Add function) are correctly identified and extracted as chunks with their respective names.
    /// 
    /// # Panics
    /// 
    /// Panics if the Parser fails to initialize, if file parsing fails, or if any of the expected module/method names are not found in the parsed chunks.

    #[test]
    fn parse_vbnet_module() {
        let content = r#"
Module Program
    Sub Main()
        Console.WriteLine("Hello")
    End Sub

    Function Add(a As Integer, b As Integer) As Integer
        Return a + b
    End Function
End Module
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();

        assert!(names.contains(&"Program"), "Expected 'Program' module, got: {:?}", names);
        assert!(names.contains(&"Main"), "Expected 'Main' method, got: {:?}", names);
        assert!(names.contains(&"Add"), "Expected 'Add' method, got: {:?}", names);
    }
    /// Verifies that the parser correctly identifies and extracts VB.NET interface definitions.
    /// 
    /// This test creates a temporary VB.NET file containing an interface declaration with a method and property, parses it, and asserts that the parser successfully recognizes the interface by name and type.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the temporary file, or if the parsed chunks do not contain an interface named "IPayable" with type `ChunkType::Interface`.

    #[test]
    fn parse_vbnet_interface() {
        let content = r#"
Public Interface IPayable
    Function CalculatePay() As Decimal
    ReadOnly Property Name As String
End Interface
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();

        assert!(
            names.iter().any(|(n, t)| *n == "IPayable" && *t == ChunkType::Interface),
            "Expected 'IPayable' interface, got: {:?}",
            names
        );
    }
    /// Tests that the parser correctly identifies and extracts VB.NET structure definitions. Writes a temporary VB.NET file containing a Point structure with properties and a constructor, parses it, and verifies that the resulting chunks include a struct chunk named "Point" with the correct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (test function)
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to identify the "Point" structure as a ChunkType::Struct, or if file creation/parsing operations fail.

    #[test]
    fn parse_vbnet_structure() {
        let content = r#"
Public Structure Point
    Public X As Double
    Public Y As Double

    Public Sub New(x As Double, y As Double)
        Me.X = x
        Me.Y = y
    End Sub
End Structure
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();

        assert!(
            names.iter().any(|(n, t)| *n == "Point" && *t == ChunkType::Struct),
            "Expected 'Point' struct, got: {:?}",
            names
        );
    }
    /// Parses a Visual Basic .NET enum definition and verifies correct parsing.
    /// 
    /// This test function creates a temporary VB.NET file containing an enum declaration with three named members, parses it using the Parser, and asserts that the resulting chunks contain an enum named "Status" with the correct ChunkType.
    /// 
    /// # Arguments
    /// 
    /// No parameters.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, if file parsing fails, if the "Status" enum is not found in the parsed chunks, or if the parsed chunk's type is not ChunkType::Enum.

    #[test]
    fn parse_vbnet_enum() {
        let content = r#"
Public Enum Status
    Active = 1
    Inactive = 2
    Pending = 3
End Enum
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let en = chunks.iter().find(|c| c.name == "Status");
        assert!(en.is_some(), "Expected 'Status' enum");
        assert_eq!(en.unwrap().chunk_type, ChunkType::Enum);
    }
    /// Parses a VB.NET property definition and verifies that the parser correctly identifies it as a Property chunk type.
    /// 
    /// # Arguments
    /// 
    /// This function takes no parameters.
    /// 
    /// # Returns
    /// 
    /// This function returns nothing (`()`). It validates parsing behavior through assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create a new instance, fails to parse the temporary file, or if the parsed chunks do not contain a chunk named "Name" with type `ChunkType::Property`.

    #[test]
    fn parse_vbnet_property() {
        let content = r#"
Public Class Employee
    Private _name As String

    Public Property Name As String
        Get
            Return _name
        End Get
        Set(value As String)
            _name = value
        End Set
    End Property
End Class
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();

        assert!(
            names.iter().any(|(n, t)| *n == "Name" && *t == ChunkType::Property),
            "Expected 'Name' property, got: {:?}",
            names
        );
    }
    /// Verifies that the parser correctly identifies and categorizes VB.NET delegates and events.
    /// 
    /// This test parses a VB.NET source file containing a delegate declaration and an event, then asserts that both are properly recognized and classified with their respective chunk types (Delegate and Event).
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to identify the "NotifyHandler" delegate or the "OnNotify" event, or if either has an incorrect chunk type assignment.

    #[test]
    fn parse_vbnet_delegate_event() {
        let content = r#"
Public Class EventSource
    Public Delegate Sub NotifyHandler(message As String)
    Public Event OnNotify As NotifyHandler
End Class
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks
            .iter()
            .map(|c| (c.name.as_str(), c.chunk_type))
            .collect();

        assert!(
            names.iter().any(|(n, t)| *n == "NotifyHandler" && *t == ChunkType::Delegate),
            "Expected 'NotifyHandler' delegate, got: {:?}",
            names
        );
        assert!(
            names.iter().any(|(n, t)| *n == "OnNotify" && *t == ChunkType::Event),
            "Expected 'OnNotify' event, got: {:?}",
            names
        );
    }
    /// Parses a VB.NET source file to extract and verify its call graph.
    /// 
    /// Creates a temporary VB.NET file containing a simple program with a Main subroutine that instantiates a Calculator object and calls its Add method, then writes the result to the console. Uses the Parser to extract the call graph from the file and validates that the Main function correctly identifies calls to Add and WriteLine.
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that asserts expected behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - The 'Main' function is not found in the call graph
    /// - The 'Add' method call is not identified in Main's callees
    /// - The 'WriteLine' method call is not identified in Main's callees

    #[test]
    fn parse_vbnet_call_graph() {
        // NOTE: VB.NET grammar requires full class/module context, so use
        // parse_file_calls (production path) rather than extract_calls_from_chunk.
        let content = r#"
Module Program
    Sub Main()
        Dim calc As New Calculator()
        Dim result As Integer = calc.Add(1, 2)
        Console.WriteLine(result)
    End Sub
End Module
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let function_calls = parser.parse_file_calls(file.path()).unwrap();
        let main_fc = function_calls.iter().find(|fc| fc.name == "Main");
        assert!(main_fc.is_some(), "Expected 'Main' in call graph");
        let main_fc = main_fc.unwrap();
        let callee_names: Vec<_> = main_fc.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            callee_names.contains(&"Add"),
            "Expected 'Add' call, got: {:?}",
            callee_names
        );
        assert!(
            callee_names.contains(&"WriteLine"),
            "Expected 'WriteLine' call, got: {:?}",
            callee_names
        );
    }
    /// Verifies that the parser correctly identifies and extracts a VB.NET function with parameterized generic types.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded VB.NET source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, or the expected "Process" function is not found in the parsed chunks.

    #[test]
    fn parse_vbnet_type_refs() {
        let content = r#"
Public Class DataProcessor
    Public Function Process(input As List(Of String), count As Integer) As Boolean
        Return True
    End Function
End Class
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "Process");
        assert!(func.is_some(), "Expected 'Process' function");
    }
    /// Tests that parsing a VB.NET file containing only comments, options, and imports produces no code chunks. Verifies that the parser correctly ignores non-code declarations and returns an empty set of Class, Method, and Function chunks.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test file and parser.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts expectations and panics if they are not met.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser produces any Class, Method, or Function chunks from a file containing only comments, options, and imports statements.

    #[test]
    fn parse_vbnet_no_code() {
        // Empty file should produce no chunks
        let content = r#"
' This is just a comment
Option Strict On
Imports System
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Should only have imports-related chunks or nothing
        let code_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| matches!(c.chunk_type, ChunkType::Class | ChunkType::Method | ChunkType::Function))
            .collect();
        assert!(
            code_chunks.is_empty(),
            "Expected no code chunks, got: {:?}",
            code_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }
    /// Parses a VB.NET class with field declarations and verifies that private and shared fields are correctly identified.
    /// 
    /// This is a test function that creates a temporary VB.NET file containing a class with field declarations, parses it, and asserts that the expected field names are extracted from the parsed chunks.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts conditions and panics if assertions fail.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, if file parsing fails, or if the expected field name `_timeout` is not found in the parsed chunks.

    #[test]
    fn parse_vbnet_field_declaration() {
        let content = r#"
Public Class Config
    Private _timeout As Integer
    Public Shared MaxRetries As Integer = 3
End Class
"#;
        let file = write_temp_file(content, "vb");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"_timeout"),
            "Expected '_timeout' field, got: {:?}",
            names
        );
    }
    /// Parses and validates the `extract_return` function's ability to identify return types from VB.NET method signatures.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded VB.NET function signatures.
    /// 
    /// # Returns
    /// 
    /// Nothing. This function performs assertions to verify the correctness of the `extract_return` function.
    /// 
    /// # Panics
    /// 
    /// Panics if assertions fail, specifically:
    /// - When `extract_return` does not return `Some("Returns string")` for a Function with return type String
    /// - When `extract_return` does not return `None` for a Sub procedure (which has no return type)

    #[test]
    fn parse_vbnet_return_extraction() {
        let sig = "Public Function GetValue(id As Integer) As String";
        let result = extract_return(sig);
        assert_eq!(result, Some("Returns string".to_string()));

        let sig2 = "Public Sub DoWork()";
        let result2 = extract_return(sig2);
        assert!(result2.is_none(), "Sub should have no return type");
    }
}
