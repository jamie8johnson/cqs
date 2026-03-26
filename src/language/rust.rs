//! Rust language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Rust code chunks
const CHUNK_QUERY: &str = r#"
(function_item
  name: (identifier) @name) @function

(struct_item
  name: (type_identifier) @name) @struct

(enum_item
  name: (type_identifier) @name) @enum

(trait_item
  name: (type_identifier) @name) @trait

(const_item
  name: (identifier) @name) @const

(static_item
  name: (identifier) @name) @const

(macro_definition
  name: (identifier) @name) @macro

(type_item
  name: (type_identifier) @name) @typealias
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))

(call_expression
  function: (scoped_identifier
    name: (identifier) @callee))

(macro_invocation
  macro: (identifier) @callee)
"#;

/// Tree-sitter query for extracting type references
///
/// Classified captures: @param_type, @return_type, @field_type, @impl_type, @bound_type, @alias_type
/// Catch-all: @type_ref (for types inside generics not reached by classified patterns)
const TYPE_QUERY: &str = r#"
;; Param
(parameter type: (type_identifier) @param_type)
(parameter type: (generic_type type: (type_identifier) @param_type))
(parameter type: (reference_type type: (type_identifier) @param_type))
(parameter type: (reference_type type: (generic_type type: (type_identifier) @param_type)))
(parameter type: (scoped_type_identifier name: (type_identifier) @param_type))

;; Return
(function_item return_type: (type_identifier) @return_type)
(function_item return_type: (generic_type type: (type_identifier) @return_type))
(function_item return_type: (reference_type type: (type_identifier) @return_type))
(function_item return_type: (reference_type type: (generic_type type: (type_identifier) @return_type)))
(function_item return_type: (scoped_type_identifier name: (type_identifier) @return_type))

;; Field
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (generic_type type: (type_identifier) @field_type))
(field_declaration type: (reference_type type: (type_identifier) @field_type))
(field_declaration type: (reference_type type: (generic_type type: (type_identifier) @field_type)))
(field_declaration type: (scoped_type_identifier name: (type_identifier) @field_type))

;; Impl
(impl_item type: (type_identifier) @impl_type)
(impl_item type: (generic_type type: (type_identifier) @impl_type))
(impl_item trait: (type_identifier) @impl_type)
(impl_item trait: (scoped_type_identifier name: (type_identifier) @impl_type))

;; Bound
(trait_bounds (type_identifier) @bound_type)
(trait_bounds (scoped_type_identifier name: (type_identifier) @bound_type))

;; Alias
(type_item type: (type_identifier) @alias_type)
(type_item type: (generic_type type: (type_identifier) @alias_type))

;; Catch-all (captures types inside generics, type_arguments, etc.)
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["line_comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "impl", "mod", "struct", "enum", "trait", "type",
    "where", "const", "static", "unsafe", "async", "await", "move", "ref", "self", "super",
    "crate", "return", "if", "else", "for", "while", "loop", "match", "break", "continue",
    "as", "in", "true", "false", "some", "none", "ok", "err",
];

/// Extracts the return type from a function signature and formats it as a documentation string.
/// 
/// Parses a function signature to find the return type annotation (after `->`) and returns a formatted string describing the return type. If no return type is specified or the annotation is empty, returns `None`.
/// 
/// # Arguments
/// 
/// `signature` - A function signature string that may contain a return type annotation.
/// 
/// # Returns
/// 
/// `Some(String)` containing the formatted return type description if a non-empty return type annotation is found; `None` if no return type annotation exists or if the annotation is empty.
fn extract_return(signature: &str) -> Option<String> {
    if let Some(arrow) = signature.find("->") {
        let ret = signature[arrow + 2..].trim();
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

/// Custom container name extraction for Rust.
/// `impl_item` uses `"type"` field (not `"name"`), and may wrap in `generic_type`.
fn extract_container_name_rust(container: tree_sitter::Node, source: &str) -> Option<String> {
    if container.kind() == "impl_item" {
        container.child_by_field_name("type").and_then(|t| {
            if t.kind() == "type_identifier" {
                Some(source[t.byte_range()].to_string())
            } else {
                // generic_type wraps type_identifier: Foo<T>
                let mut cursor = t.walk();
                for child in t.children(&mut cursor) {
                    if child.kind() == "type_identifier" {
                        return Some(source[child.byte_range()].to_string());
                    }
                }
                None
            }
        })
    } else {
        // trait_item: read "name" field
        container
            .child_by_field_name("name")
            .map(|n| source[n.byte_range()].to_string())
    }
}

const COMMON_TYPES: &[&str] = &[
    "String", "Vec", "Result", "Option", "Box", "Arc", "Rc", "HashMap", "HashSet", "BTreeMap",
    "BTreeSet", "Path", "PathBuf", "Value", "Error", "Self", "None", "Some", "Ok", "Err", "Mutex",
    "RwLock", "Cow", "Pin", "Future", "Iterator", "Display", "Debug", "Clone", "Default", "Send",
    "Sync", "Sized", "Copy", "From", "Into", "AsRef", "AsMut", "Deref", "DerefMut", "Read",
    "Write", "Seek", "BufRead", "ToString", "Serialize", "Deserialize",
];

/// Post-process Rust chunks: reclassify `new` methods as Constructor (convention).
fn post_process_rust(
    name: &mut String,
    chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Rust convention: fn new(...) inside an impl block is a constructor
    if *chunk_type == ChunkType::Method && name == "new" {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "rust",
    grammar: Some(|| tree_sitter_rust::LANGUAGE.into()),
    extensions: &["rs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["impl_item", "trait_item"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/{stem}_test.rs")),
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &[],
    extract_container_name: Some(extract_container_name_rust),
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_rust as PostProcessChunkFn),
    test_markers: &["#[test]", "#[cfg(test)]"],
    test_path_patterns: &["%/tests/%", "%\\_test.rs"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        // std::fmt
        "fmt",
        // std::convert
        "from", "into", "try_from", "try_into",
        // std::ops
        "deref", "deref_mut", "drop", "index", "index_mut",
        "add", "sub", "mul", "div", "rem", "neg", "not",
        "bitor", "bitand", "bitxor", "shl", "shr",
        // std::cmp
        "eq", "ne", "partial_cmp", "cmp",
        // std::hash
        "hash",
        // std::clone
        "clone", "clone_from",
        // std::default
        "default",
        // std::iter
        "next", "into_iter",
        // std::io
        "read", "write", "flush",
        // std::str
        "from_str",
        // std::convert / std::borrow
        "as_ref", "as_mut", "borrow", "borrow_mut",
        // serde
        "serialize", "deserialize",
        // std::error
        "source",
        // std::future
        "poll",
    ],
    injections: &[],
    doc_format: "triple_slash",
    doc_convention: "Use `# Arguments`, `# Returns`, `# Errors`, `# Panics` sections as appropriate.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "pub pub(crate) pub(super)",
    },
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
    /// Parses a Rust type alias definition and verifies it is correctly identified as a TypeAlias chunk.
    /// 
    /// This test function creates a temporary Rust file containing a type alias declaration, parses it using the Parser, and asserts that the resulting chunk has the correct name and type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize or parse the file
    /// - No chunk named "Result" is found in the parsed output
    /// - The chunk type is not ChunkType::TypeAlias

    #[test]
    fn parse_rust_type_alias() {
        let content = "type Result<T> = std::result::Result<T, MyError>;\n";
        let file = write_temp_file(content, "rs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "Result").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_rust_constructor() {
        let content = r#"
struct Config {
    path: String,
}

impl Config {
    fn new(path: String) -> Self {
        Config { path }
    }

    fn validate(&self) -> bool {
        true
    }
}
"#;
        let file = write_temp_file(content, "rs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ctor = chunks.iter().find(|c| c.name == "new").unwrap();
        assert_eq!(ctor.chunk_type, ChunkType::Constructor);
        // validate should still be a Method
        let method = chunks.iter().find(|c| c.name == "validate").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }
}
