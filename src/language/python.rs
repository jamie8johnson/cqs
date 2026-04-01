//! Python language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Returns true if the name follows UPPER_CASE convention (all ASCII uppercase/digits/underscores,
/// at least one letter, e.g. MAX_RETRIES, API_URL_V2).
fn is_upper_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.bytes().any(|b| b.is_ascii_uppercase())
}

/// Tree-sitter query for extracting Python code chunks
const CHUNK_QUERY: &str = r#"
(function_definition
  name: (identifier) @name) @function

(class_definition
  name: (identifier) @name) @class

;; Module-level constant assignments (UPPER_CASE convention)
(expression_statement
  (assignment
    left: (identifier) @name
    right: (_))) @const
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call
  function: (identifier) @callee)

(call
  function: (attribute
    attribute: (identifier) @callee))
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Param
(typed_parameter type: (type (identifier) @param_type))
(typed_parameter type: (type (generic_type (identifier) @param_type)))
(typed_default_parameter type: (type (identifier) @param_type))
(typed_default_parameter type: (type (generic_type (identifier) @param_type)))

;; Return
(function_definition return_type: (type (identifier) @return_type))
(function_definition return_type: (type (generic_type (identifier) @return_type)))

;; Field
(assignment type: (type (identifier) @field_type))
(assignment type: (type (generic_type (identifier) @field_type)))

;; Impl (class inheritance)
(class_definition superclasses: (argument_list (identifier) @impl_type))

;; Alias (PEP 695)
(type_alias_statement (type (identifier) @alias_type))

;; Catch-all (scoped to type positions)
(type (identifier) @type_ref)
"#;

/// Doc comment node types (sibling comments and standalone strings before a definition)
const DOC_NODES: &[&str] = &["string", "comment"];

const STOPWORDS: &[&str] = &[
    "def", "class", "self", "return", "if", "elif", "else", "for", "while", "import",
    "from", "as", "with", "try", "except", "finally", "raise", "pass", "break", "continue",
    "and", "or", "not", "in", "is", "true", "false", "none", "lambda", "yield", "global",
    "nonlocal",
];

/// Returns true if the node is nested inside a function/class body.
fn is_inside_function(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        if parent.kind() == "function_definition" {
            return true;
        }
        cursor = parent.parent();
    }
    false
}

/// Post-process Python chunks: only keep `@const` captures whose name is UPPER_CASE
/// and that are at module level (not inside function bodies).
#[allow(clippy::ptr_arg)] // signature must match PostProcessChunkFn type alias
fn post_process_python(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        if is_inside_function(node) {
            return false;
        }
        return is_upper_snake_case(name);
    }
    // __init__ methods are constructors
    if *chunk_type == ChunkType::Method && name == "__init__" {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

/// Extracts the return type from a function signature and formats it as a descriptive string.
/// # Arguments
/// * `signature` - A function signature string that may contain a return type annotation following "->".
/// # Returns
/// Returns `Some(String)` containing a formatted description like "Returns <type>" if a return type is found and non-empty. Returns `None` if no return type annotation exists or if the return type is empty.
fn extract_return(signature: &str) -> Option<String> {
    if let Some(arrow) = signature.rfind("->") {
        let ret = signature[arrow + 2..].trim().trim_end_matches(':');
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "python",
    grammar: Some(|| tree_sitter_python::LANGUAGE.into()),
    extensions: &["py", "pyi"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilColon,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_definition"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/test_{stem}.py")),
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: &[
        "str", "int", "float", "bool", "list", "dict", "set", "tuple", "None", "Any", "Optional",
        "Union", "List", "Dict", "Set", "Tuple", "Type", "Callable", "Iterator", "Generator",
        "Coroutine", "Exception", "ValueError", "TypeError", "KeyError", "IndexError", "Path",
        "Self",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_python as PostProcessChunkFn),
    test_markers: &["def test_", "pytest"],
    test_path_patterns: &["%/tests/%", "%\\_test.py", "%/test\\_%"],
    structural_matchers: None,
    entry_point_names: &["__init__", "setup", "teardown"],
    trait_method_names: &[
        "__str__", "__repr__", "__eq__", "__ne__", "__lt__", "__le__", "__gt__", "__ge__",
        "__hash__", "__bool__", "__len__", "__iter__", "__next__", "__contains__",
        "__getitem__", "__setitem__", "__delitem__", "__call__", "__enter__", "__exit__",
        "__del__", "__new__", "__init_subclass__", "__class_getitem__",
    ],
    injections: &[],
    doc_format: "python_docstring",
    doc_convention: "Format as a Google-style docstring (Args/Returns/Raises sections).",
    field_style: FieldStyle::NameFirst {
        separators: ":=",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["class ", "@property", "def "],
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
    fn parse_python_upper_case_constant() {
        let content = r#"
MAX_RETRIES = 3
API_URL = "https://example.com"
lowercase_var = 42
MixedCase = "nope"

def foo():
    pass
"#;
        let file = write_temp_file(content, "py");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let max = chunks.iter().find(|c| c.name == "MAX_RETRIES");
        assert!(max.is_some(), "Should capture MAX_RETRIES");
        assert_eq!(max.unwrap().chunk_type, ChunkType::Constant);
        let url = chunks.iter().find(|c| c.name == "API_URL");
        assert!(url.is_some(), "Should capture API_URL");
        assert_eq!(url.unwrap().chunk_type, ChunkType::Constant);
        // lowercase and MixedCase should be filtered out
        assert!(
            chunks.iter().find(|c| c.name == "lowercase_var").is_none(),
            "Should not capture lowercase_var"
        );
        assert!(
            chunks.iter().find(|c| c.name == "MixedCase").is_none(),
            "Should not capture MixedCase"
        );
    }

    #[test]
    fn test_is_upper_snake_case() {
        assert!(is_upper_snake_case("MAX_RETRIES"));
        assert!(is_upper_snake_case("API_URL_V2"));
        assert!(is_upper_snake_case("X"));
        assert!(!is_upper_snake_case("lowercase"));
        assert!(!is_upper_snake_case("MixedCase"));
        assert!(!is_upper_snake_case(""));
        assert!(!is_upper_snake_case("123")); // no letters
    }

    #[test]
    fn parse_python_constructor() {
        let content = r#"
class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        print(self.name)
"#;
        let file = write_temp_file(content, "py");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ctor = chunks.iter().find(|c| c.name == "__init__").unwrap();
        assert_eq!(ctor.chunk_type, ChunkType::Constructor);
        // greet should still be a Method
        let method = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }
}
