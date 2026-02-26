//! C# language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting C# code chunks
const CHUNK_QUERY: &str = r#"
;; Functions/methods
(method_declaration name: (identifier) @name) @function
(constructor_declaration name: (identifier) @name) @function
(operator_declaration) @function
(indexer_declaration) @function
(local_function_statement name: (identifier) @name) @function

;; Properties
(property_declaration name: (identifier) @name) @property

;; Delegates
(delegate_declaration name: (identifier) @name) @delegate

;; Events
(event_field_declaration
  (variable_declaration
    (variable_declarator (identifier) @name))) @event
(event_declaration name: (identifier) @name) @event

;; Types
(class_declaration name: (identifier) @name) @class
(struct_declaration name: (identifier) @name) @struct
(record_declaration name: (identifier) @name) @struct
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum
"#;

/// Tree-sitter query for extracting C# function calls
const CALL_QUERY: &str = r#"
(invocation_expression
  function: (member_access_expression name: (identifier) @callee))
(invocation_expression
  function: (identifier) @callee)
(object_creation_expression type: (identifier) @callee)
(object_creation_expression type: (generic_name (identifier) @callee))
(object_creation_expression type: (qualified_name (identifier) @callee))
"#;

/// Tree-sitter query for extracting C# type references
const TYPE_QUERY: &str = r#"
;; Param — method parameters
(parameter type: (identifier) @param_type)
(parameter type: (generic_name (identifier) @param_type))
(parameter type: (qualified_name (identifier) @param_type))
(parameter type: (nullable_type (identifier) @param_type))
(parameter type: (array_type (identifier) @param_type))

;; Return — method_declaration uses "returns" field (not "type"!)
(method_declaration returns: (identifier) @return_type)
(method_declaration returns: (generic_name (identifier) @return_type))
(method_declaration returns: (qualified_name (identifier) @return_type))
(method_declaration returns: (nullable_type (identifier) @return_type))
(delegate_declaration type: (identifier) @return_type)
(delegate_declaration type: (generic_name (identifier) @return_type))
(local_function_statement type: (identifier) @return_type)
(local_function_statement type: (generic_name (identifier) @return_type))

;; Field — field declarations and property types
(field_declaration (variable_declaration type: (identifier) @field_type))
(field_declaration (variable_declaration type: (generic_name (identifier) @field_type)))
(property_declaration type: (identifier) @field_type)
(property_declaration type: (generic_name (identifier) @field_type))

;; Impl — base class, interface implementations
(base_list (identifier) @impl_type)
(base_list (generic_name (identifier) @impl_type))
(base_list (qualified_name (identifier) @impl_type))

;; Bound — generic constraints (where T : IFoo)
(type_parameter_constraint (type (identifier) @bound_type))
(type_parameter_constraint (type (generic_name (identifier) @bound_type)))

;; Alias — using alias directives
(using_directive name: (identifier) @alias_type)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "public", "private", "protected", "internal", "static", "readonly", "sealed", "abstract",
    "virtual", "override", "async", "await", "class", "struct", "interface", "enum", "namespace",
    "using", "return", "if", "else", "for", "foreach", "while", "do", "switch", "case", "break",
    "continue", "new", "this", "base", "try", "catch", "finally", "throw", "var", "void", "int",
    "string", "bool", "true", "false", "null", "get", "set", "value", "where", "partial", "event",
    "delegate", "record", "yield", "in", "out", "ref",
];

const COMMON_TYPES: &[&str] = &[
    "string", "int", "bool", "object", "void", "double", "float", "long", "byte", "char",
    "decimal", "short", "uint", "ulong", "Task", "ValueTask", "List", "Dictionary", "HashSet",
    "Queue", "Stack", "IEnumerable", "IList", "IDictionary", "ICollection", "IQueryable", "Action",
    "Func", "Predicate", "EventHandler", "EventArgs", "IDisposable", "CancellationToken", "ILogger",
    "StringBuilder", "Exception", "Nullable", "Span", "Memory", "ReadOnlySpan", "IServiceProvider",
    "HttpContext", "IConfiguration",
];

fn extract_return(signature: &str) -> Option<String> {
    // C#: return type before method name, like Java
    // e.g., "public async Task<int> GetValue(..." → "Task<int>"
    // Must skip: access modifiers, static, async, virtual, override, etc.
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            let ret_type = words[words.len() - 2];
            if !matches!(
                ret_type,
                "void"
                    | "public"
                    | "private"
                    | "protected"
                    | "internal"
                    | "static"
                    | "abstract"
                    | "virtual"
                    | "override"
                    | "sealed"
                    | "async"
                    | "extern"
                    | "partial"
                    | "new"
                    | "unsafe"
            ) {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "csharp",
    grammar: Some(|| tree_sitter_c_sharp::LANGUAGE.into()),
    extensions: &["cs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[
        "class_declaration",
        "struct_declaration",
        "record_declaration",
        "interface_declaration",
        "declaration_list",
    ],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.cs")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["declaration_list"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_return_csharp() {
        assert_eq!(
            extract_return("public int Add(int a, int b)"),
            Some("Returns int".to_string())
        );
        assert_eq!(extract_return("public void DoSomething()"), None);
        assert_eq!(
            extract_return("private static string GetValue()"),
            Some("Returns string".to_string())
        );
    }
}
