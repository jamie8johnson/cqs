//! Consolidated language definitions.
//!
//! Each language is a `LanguageDef` static with queries loaded from `queries/*.scm`.
//! Custom functions (post_process, extract_return, detect_language) are defined
//! alongside the language they serve.
//!
//! # Adding a language
//!
//! 1. Create `queries/<lang>.chunks.scm` (and optionally `.calls.scm`, `.types.scm`)
//! 2. Add a `LanguageDef` static and `pub fn definition_<lang>()` below
//! 3. Add the variant to `define_languages!` in `mod.rs`
//! 4. Add the feature flag to `Cargo.toml`

#![allow(clippy::needless_update)] // ..DEFAULTS used for clarity even when all fields set

use super::{
    ChunkType, FieldStyle, InjectionRule, LanguageDef, PostProcessChunkFn, SignatureStyle,
};

// ============================================================================
// Defaults — used with `..DEFAULTS` to avoid repeating None/&[] on every language
// ============================================================================

const DEFAULTS: LanguageDef = LanguageDef {
    name: "",
    grammar: None,
    extensions: &[],
    chunk_query: "",
    call_query: None,
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: &[],
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: &[],
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
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
    doc_format: "default",
    doc_convention: "",
    field_style: FieldStyle::None,
    skip_line_prefixes: &[],
};

// ============================================================================
// Bash
// ============================================================================

static LANG_BASH: LanguageDef = LanguageDef {
    name: "bash",
    grammar: Some(|| tree_sitter_bash::LANGUAGE.into()),
    extensions: &["sh", "bash"],
    chunk_query: include_str!("queries/bash.chunks.scm"),
    call_query: Some(include_str!("queries/bash.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        "if", "then", "else", "elif", "fi", "for", "do", "done", "while", "until", "case", "esac",
        "in", "function", "return", "exit", "export", "local", "declare", "readonly", "unset",
        "shift", "set", "eval", "exec", "source", "true", "false", "echo", "printf", "read",
        "test",
    ],
    test_path_patterns: &["%/tests/%", "%\\_test.sh", "%.bats"],
    entry_point_names: &["main"],
    ..DEFAULTS
};

pub fn definition_bash() -> &'static LanguageDef {
    &LANG_BASH
}

// ============================================================================
// C (c)
// ============================================================================

/// Extracts the return type from a C function signature and formats it as documentation text.
/// # Arguments
/// `signature` - A C function signature string, expected to contain a return type, function name, and parameter list in parentheses (e.g., "int add(int a, int b)").
/// # Returns
/// `Some(String)` containing the formatted return type documentation (e.g., "Returns int") if a non-void return type is found after filtering out storage class specifiers (static, inline, extern, const, volatile). Returns `None` if the signature is malformed, has no return type, or the return type is void.
fn extract_return_c(signature: &str) -> Option<String> {
    // C: return type is before the function name, e.g., "int add(int a, int b)"
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        // Last word is function name, everything before is return type + modifiers
        if words.len() >= 2 {
            // Filter out storage class specifiers
            let type_words: Vec<&str> = words[..words.len() - 1]
                .iter()
                .filter(|w| !matches!(**w, "static" | "inline" | "extern" | "const" | "volatile"))
                .copied()
                .collect();
            if !type_words.is_empty() && type_words != ["void"] {
                let ret = type_words.join(" ");
                let ret_words = crate::nl::tokenize_identifier(&ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static LANG_C: LanguageDef = LanguageDef {
    name: "c",
    grammar: Some(|| tree_sitter_c::LANGUAGE.into()),
    extensions: &["c", "h"],
    chunk_query: include_str!("queries/c.chunks.scm"),
    call_query: Some(include_str!("queries/c.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        "if", "else", "for", "while", "do", "switch", "case", "break", "continue", "return",
        "typedef", "struct", "enum", "union", "void", "int", "char", "float", "double", "long",
        "short", "unsigned", "signed", "static", "extern", "const", "volatile", "sizeof", "null",
        "true", "false",
    ],
    extract_return_nl: extract_return_c,
    type_query: Some(include_str!("queries/c.types.scm")),
    common_types: &[
        "int",
        "char",
        "float",
        "double",
        "void",
        "long",
        "short",
        "unsigned",
        "size_t",
        "ssize_t",
        "ptrdiff_t",
        "FILE",
        "bool",
    ],
    test_path_patterns: &["%/tests/%", "%\\_test.c"],
    entry_point_names: &["main"],
    doc_format: "javadoc",
    doc_convention: "Use Doxygen format: @param, @return, @throws tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes: "static const volatile extern unsigned signed",
    },
    skip_line_prefixes: &["struct ", "union ", "enum ", "typedef "],
    ..DEFAULTS
};

pub fn definition_c() -> &'static LanguageDef {
    &LANG_C
}

// ============================================================================
// Cpp (cpp)
// ============================================================================

/// Extract parent type from a function's own declarator.
/// For out-of-class methods: `void MyClass::method()` → Some("MyClass").
fn extract_qualified_method_cpp(node: tree_sitter::Node, source: &str) -> Option<String> {
    // function_definition > declarator: function_declarator > declarator: qualified_identifier
    let func_decl = node.child_by_field_name("declarator")?;
    let inner_decl = func_decl.child_by_field_name("declarator")?;
    if inner_decl.kind() != "qualified_identifier" {
        return None;
    }
    let scope = inner_decl.child_by_field_name("scope")?;
    Some(source[scope.byte_range()].to_string())
}

/// Extracts the return type from a function signature in either Rust or C-style syntax.
/// # Arguments
/// * `signature` - A string slice containing a function signature to parse
/// # Returns
/// Returns `Some(String)` containing a formatted return type description (e.g., "returns i32") if a non-void return type is found. Returns `None` if no return type is detected or the return type is void.
/// # Description
/// Attempts two parsing strategies:
/// 1. Rust-style: Looks for `->` return type annotation after the closing parenthesis
/// 2. C-style: Extracts the type specifier(s) preceding the function name (filtered to exclude storage class and qualifier keywords)
/// The extracted type is tokenized and formatted with a "returns " prefix.
fn extract_return_cpp(signature: &str) -> Option<String> {
    // Check for trailing return type: auto foo() -> ReturnType
    if let Some(paren) = signature.rfind(')') {
        let after = &signature[paren + 1..];
        if let Some(arrow) = after.find("->") {
            let ret_part = after[arrow + 2..].trim();
            // Take until '{' or end
            let end = ret_part.find('{').unwrap_or(ret_part.len());
            let ret_type = ret_part[..end].trim();
            if !ret_type.is_empty() {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }

    // C-style prefix extraction: return type before function name
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            let type_words: Vec<&str> = words[..words.len() - 1]
                .iter()
                .filter(|w| {
                    !matches!(
                        **w,
                        "static"
                            | "inline"
                            | "extern"
                            | "const"
                            | "volatile"
                            | "virtual"
                            | "explicit"
                            | "friend"
                            | "constexpr"
                            | "consteval"
                            | "constinit"
                            | "auto"
                    )
                })
                .copied()
                .collect();
            if !type_words.is_empty() && type_words != ["void"] {
                let ret = type_words.join(" ");
                let ret_words = crate::nl::tokenize_identifier(&ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

/// Post-process C++ chunks: detect constructors.
/// A `function_definition` with no return type (no type child before the declarator)
/// is a constructor. Destructors (name starts with `~`) are excluded.
#[allow(clippy::ptr_arg)] // signature must match PostProcessChunkFn type alias
fn post_process_cpp_cpp(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if !matches!(*chunk_type, ChunkType::Function | ChunkType::Method) {
        return true;
    }
    // Skip destructors
    if name.starts_with('~') {
        return true;
    }
    // C++ constructors: function_definition with no return type before the declarator.
    // Regular methods have a type child (e.g., primitive_type, type_identifier).
    if node.kind() == "function_definition" {
        let has_return_type = node.child_by_field_name("type").is_some();
        if !has_return_type {
            *chunk_type = ChunkType::Constructor;
        }
    }
    true
}

static LANG_CPP: LanguageDef = LanguageDef {
    name: "cpp",
    grammar: Some(|| tree_sitter_cpp::LANGUAGE.into()),
    extensions: &["cpp", "cxx", "cc", "hpp", "hxx", "hh", "ipp"],
    chunk_query: include_str!("queries/cpp.chunks.scm"),
    call_query: Some(include_str!("queries/cpp.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["class_specifier", "struct_specifier"],
    stopwords: &[
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "class",
        "struct",
        "enum",
        "namespace",
        "template",
        "typename",
        "using",
        "typedef",
        "virtual",
        "override",
        "final",
        "const",
        "static",
        "inline",
        "explicit",
        "extern",
        "friend",
        "public",
        "private",
        "protected",
        "void",
        "int",
        "char",
        "float",
        "double",
        "long",
        "short",
        "unsigned",
        "signed",
        "auto",
        "new",
        "delete",
        "this",
        "true",
        "false",
        "nullptr",
        "sizeof",
        "dynamic_cast",
        "static_cast",
        "reinterpret_cast",
        "const_cast",
        "throw",
        "try",
        "catch",
        "noexcept",
        "operator",
        "concept",
        "requires",
        "constexpr",
        "consteval",
        "constinit",
        "mutable",
        "volatile",
        "co_await",
        "co_yield",
        "co_return",
        "decltype",
    ],
    extract_return_nl: extract_return_cpp,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/{stem}_test.cpp")),
    type_query: Some(include_str!("queries/cpp.types.scm")),
    common_types: &[
        "string",
        "wstring",
        "string_view",
        "vector",
        "map",
        "unordered_map",
        "set",
        "unordered_set",
        "multimap",
        "multiset",
        "list",
        "deque",
        "array",
        "forward_list",
        "pair",
        "tuple",
        "optional",
        "variant",
        "any",
        "expected",
        "shared_ptr",
        "unique_ptr",
        "weak_ptr",
        "function",
        "size_t",
        "ptrdiff_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "nullptr_t",
        "span",
        "basic_string",
        "iterator",
        "const_iterator",
        "reverse_iterator",
        "ostream",
        "istream",
        "iostream",
        "fstream",
        "ifstream",
        "ofstream",
        "stringstream",
        "istringstream",
        "ostringstream",
        "thread",
        "mutex",
        "recursive_mutex",
        "condition_variable",
        "atomic",
        "future",
        "promise",
        "exception",
        "runtime_error",
        "logic_error",
        "invalid_argument",
        "out_of_range",
        "overflow_error",
        "bad_alloc",
        "type_info",
        "initializer_list",
        "allocator",
        "hash",
        "equal_to",
        "less",
        "greater",
        "reference_wrapper",
        "bitset",
        "complex",
        "regex",
        "chrono",
    ],
    container_body_kinds: &["field_declaration_list"],
    extract_qualified_method: Some(extract_qualified_method_cpp),
    post_process_chunk: Some(post_process_cpp_cpp as PostProcessChunkFn),
    test_markers: &["TEST(", "TEST_F(", "EXPECT_", "ASSERT_"],
    test_path_patterns: &["%/tests/%", "%\\_test.cpp", "%\\_test.cc"],
    entry_point_names: &["main"],
    doc_format: "javadoc",
    doc_convention: "Use Doxygen format: @param, @return, @throws tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes: "static const volatile mutable virtual inline",
    },
    skip_line_prefixes: &["class ", "struct ", "union ", "enum ", "template"],
    ..DEFAULTS
};

pub fn definition_cpp() -> &'static LanguageDef {
    &LANG_CPP
}

// ============================================================================
// Csharp (csharp)
// ============================================================================

/// Extracts the return type from a C# method signature and formats it as documentation text.
/// Parses a C# method signature to identify and extract the return type, skipping access modifiers and keywords like `static`, `async`, and `virtual`. The return type is the second-to-last word before the opening parenthesis of the parameter list.
/// # Arguments
/// * `signature` - A C# method signature string, e.g., `"public async Task<int> GetValue(...)"`
/// # Returns
/// Returns `Some(String)` containing the formatted return type as `"Returns <type>"` if a valid return type is found. Returns `None` if the signature cannot be parsed, has fewer than two words before the opening parenthesis, or the extracted type is a modifier keyword or `void`.
fn extract_return_csharp(signature: &str) -> Option<String> {
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

/// Post-process C# chunks: reclassify `constructor_declaration` nodes as Constructor.
fn post_process_csharp_csharp(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if node.kind() == "constructor_declaration"
        && matches!(*chunk_type, ChunkType::Function | ChunkType::Method)
    {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

static LANG_CSHARP: LanguageDef = LanguageDef {
    name: "csharp",
    grammar: Some(|| tree_sitter_c_sharp::LANGUAGE.into()),
    extensions: &["cs"],
    chunk_query: include_str!("queries/csharp.chunks.scm"),
    call_query: Some(include_str!("queries/csharp.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &[
        "class_declaration",
        "struct_declaration",
        "record_declaration",
        "interface_declaration",
        "declaration_list",
    ],
    stopwords: &[
        "public",
        "private",
        "protected",
        "internal",
        "static",
        "readonly",
        "sealed",
        "abstract",
        "virtual",
        "override",
        "async",
        "await",
        "class",
        "struct",
        "interface",
        "enum",
        "namespace",
        "using",
        "return",
        "if",
        "else",
        "for",
        "foreach",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "new",
        "this",
        "base",
        "try",
        "catch",
        "finally",
        "throw",
        "var",
        "void",
        "int",
        "string",
        "bool",
        "true",
        "false",
        "null",
        "get",
        "set",
        "value",
        "where",
        "partial",
        "event",
        "delegate",
        "record",
        "yield",
        "in",
        "out",
        "ref",
    ],
    extract_return_nl: extract_return_csharp,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.cs")),
    test_name_suggestion: Some(|name| {
        let pn = super::pascal_test_name("", name);
        format!("{pn}_ShouldWork")
    }),
    type_query: Some(include_str!("queries/csharp.types.scm")),
    common_types: &[
        "string",
        "int",
        "bool",
        "object",
        "void",
        "double",
        "float",
        "long",
        "byte",
        "char",
        "decimal",
        "short",
        "uint",
        "ulong",
        "Task",
        "ValueTask",
        "List",
        "Dictionary",
        "HashSet",
        "Queue",
        "Stack",
        "IEnumerable",
        "IList",
        "IDictionary",
        "ICollection",
        "IQueryable",
        "Action",
        "Func",
        "Predicate",
        "EventHandler",
        "EventArgs",
        "IDisposable",
        "CancellationToken",
        "ILogger",
        "StringBuilder",
        "Exception",
        "Nullable",
        "Span",
        "Memory",
        "ReadOnlySpan",
        "IServiceProvider",
        "HttpContext",
        "IConfiguration",
    ],
    container_body_kinds: &["declaration_list"],
    post_process_chunk: Some(post_process_csharp_csharp as PostProcessChunkFn),
    test_markers: &["[Test]", "[Fact]", "[Theory]", "[TestMethod]"],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%Tests.cs"],
    entry_point_names: &["Main"],
    trait_method_names: &[
        "Equals",
        "GetHashCode",
        "ToString",
        "CompareTo",
        "Dispose",
        "GetEnumerator",
        "MoveNext",
    ],
    doc_format: "javadoc",
    doc_convention: "Use XML doc comments: <summary>, <param>, <returns>, <exception> tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes:
            "private protected public internal static readonly virtual override abstract sealed new",
    },
    skip_line_prefixes: &["class ", "struct ", "interface ", "enum ", "record "],
    ..DEFAULTS
};

pub fn definition_csharp() -> &'static LanguageDef {
    &LANG_CSHARP
}

// ============================================================================
// Css (css)
// ============================================================================

/// Post-process CSS chunks to set correct types.
fn post_process_css_css(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "rule_set" => *chunk_type = ChunkType::Property,
        "keyframes_statement" => *chunk_type = ChunkType::Section,
        "media_statement" => {
            *chunk_type = ChunkType::Section;
            // Media statements don't have a named child captured as @name,
            // so extract a summary from the source text
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            // Extract the condition: @media (max-width: 600px) → "(max-width: 600px)"
            if let Some(brace) = text.find('{') {
                // Extract everything between @media and { as the query
                let after_media = if text.starts_with("@media") { 6 } else { 0 };
                if after_media < brace {
                    let query = text[after_media..brace].trim();
                    if !query.is_empty() {
                        *name = format!("@media {query}");
                        return true;
                    }
                }
            }
            *name = "@media".to_string();
        }
        _ => {}
    }
    true
}

/// Extracts the return type from a function signature.
/// # Arguments
/// * `signature` - A function signature string to parse
/// # Returns
/// Returns `None` as CSS does not support function return types. Always returns `None` regardless of input.
fn extract_return_css(_signature: &str) -> Option<String> {
    // CSS has no functions or return types
    None
}

static LANG_CSS: LanguageDef = LanguageDef {
    name: "css",
    grammar: Some(|| tree_sitter_css::LANGUAGE.into()),
    extensions: &["css"],
    chunk_query: include_str!("queries/css.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "auto",
        "inherit",
        "initial",
        "unset",
        "none",
        "block",
        "inline",
        "flex",
        "grid",
        "absolute",
        "relative",
        "fixed",
        "sticky",
        "hidden",
        "visible",
        "solid",
        "dashed",
        "dotted",
        "normal",
        "bold",
        "italic",
        "center",
        "left",
        "right",
        "top",
        "bottom",
        "transparent",
        "currentColor",
        "important",
        "media",
        "keyframes",
        "from",
        "to",
    ],
    extract_return_nl: extract_return_css,
    post_process_chunk: Some(post_process_css_css as PostProcessChunkFn),
    ..DEFAULTS
};

pub fn definition_css() -> &'static LanguageDef {
    &LANG_CSS
}

// ============================================================================
// Cuda (cuda)
// ============================================================================

/// Extracts and formats the return type from a function signature.
/// This function handles both C++ trailing return type syntax (after `->`) and C-style prefix return types (before the function name). It tokenizes the extracted return type and formats it as a documentation string.
/// # Arguments
/// `signature` - A function signature string to parse for return type information.
/// # Returns
/// Returns `Some(String)` containing a formatted return type description (e.g., "returns int") if a non-void return type is found, or `None` if no return type is present or the return type is void.
fn extract_return_cuda(signature: &str) -> Option<String> {
    // Reuse C++ trailing return type logic
    if let Some(paren) = signature.rfind(')') {
        let after = &signature[paren + 1..];
        if let Some(arrow) = after.find("->") {
            let ret_part = after[arrow + 2..].trim();
            let end = ret_part.find('{').unwrap_or(ret_part.len());
            let ret_type = ret_part[..end].trim();
            if !ret_type.is_empty() {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }

    // C-style prefix extraction
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            let type_words: Vec<&str> = words[..words.len() - 1]
                .iter()
                .filter(|w| {
                    !matches!(
                        **w,
                        "static"
                            | "inline"
                            | "extern"
                            | "const"
                            | "volatile"
                            | "virtual"
                            | "explicit"
                            | "__global__"
                            | "__device__"
                            | "__host__"
                            | "__forceinline__"
                            | "__noinline__"
                            | "auto"
                    )
                })
                .copied()
                .collect();
            if !type_words.is_empty() && type_words != ["void"] {
                let ret = type_words.join(" ");
                let ret_words = crate::nl::tokenize_identifier(&ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

/// Extract parent type from out-of-class method: `void MyClass::method()` → Some("MyClass")
fn extract_qualified_method_cuda(node: tree_sitter::Node, source: &str) -> Option<String> {
    let func_decl = node.child_by_field_name("declarator")?;
    let inner_decl = func_decl.child_by_field_name("declarator")?;
    if inner_decl.kind() != "qualified_identifier" {
        return None;
    }
    let scope = inner_decl.child_by_field_name("scope")?;
    Some(source[scope.byte_range()].to_string())
}

static LANG_CUDA: LanguageDef = LanguageDef {
    name: "cuda",
    grammar: Some(|| tree_sitter_cuda::LANGUAGE.into()),
    extensions: &["cu", "cuh"],
    chunk_query: include_str!("queries/cuda.chunks.scm"),
    call_query: Some(include_str!("queries/cuda.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["class_specifier", "struct_specifier"],
    stopwords: &[
        // C++ stopwords
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "class",
        "struct",
        "enum",
        "namespace",
        "template",
        "typename",
        "using",
        "typedef",
        "virtual",
        "override",
        "final",
        "const",
        "static",
        "inline",
        "explicit",
        "extern",
        "friend",
        "public",
        "private",
        "protected",
        "void",
        "int",
        "char",
        "float",
        "double",
        "long",
        "short",
        "unsigned",
        "signed",
        "auto",
        "new",
        "delete",
        "this",
        "true",
        "false",
        "nullptr",
        "sizeof",
        // CUDA-specific qualifiers
        "__global__",
        "__device__",
        "__host__",
        "__shared__",
        "__constant__",
        "__managed__",
        "__restrict__",
        "__noinline__",
        "__forceinline__",
        "dim3",
        "blockIdx",
        "threadIdx",
        "blockDim",
        "gridDim",
        "warpSize",
        "cudaMalloc",
        "cudaFree",
        "cudaMemcpy",
    ],
    extract_return_nl: extract_return_cuda,
    common_types: &[
        "int",
        "char",
        "float",
        "double",
        "void",
        "long",
        "short",
        "unsigned",
        "size_t",
        "dim3",
        "cudaError_t",
        "cudaStream_t",
        "cudaEvent_t",
        "float2",
        "float3",
        "float4",
        "int2",
        "int3",
        "int4",
        "uint2",
        "uint3",
        "uint4",
        "half",
        "__half",
        "__half2",
    ],
    container_body_kinds: &["field_declaration_list"],
    extract_qualified_method: Some(extract_qualified_method_cuda),
    test_markers: &["TEST(", "TEST_F(", "EXPECT_", "ASSERT_"],
    test_path_patterns: &["%/tests/%", "%\\_test.cu"],
    entry_point_names: &["main"],
    doc_format: "javadoc",
    doc_convention: "Use Doxygen format: @param, @return, @throws tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes: "static const volatile mutable virtual inline",
    },
    skip_line_prefixes: &["class ", "struct ", "union ", "enum ", "template"],
    ..DEFAULTS
};

pub fn definition_cuda() -> &'static LanguageDef {
    &LANG_CUDA
}

// ============================================================================
// Dart
// ============================================================================

#[cfg(feature = "lang-dart")]
static LANG_DART: LanguageDef = LanguageDef {
    name: "dart",
    grammar: Some(|| tree_sitter_dart::LANGUAGE.into()),
    extensions: &["dart"],
    chunk_query: include_str!("queries/dart.chunks.scm"),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: &["comment", "documentation_comment"],
    method_node_kinds: &[],
    method_containers: &["class_body", "extension_body"],
    stopwords: &[
        "if",
        "else",
        "for",
        "while",
        "do",
        "return",
        "class",
        "extends",
        "implements",
        "import",
        "void",
        "var",
        "final",
        "const",
        "static",
        "this",
        "super",
        "new",
        "null",
        "true",
        "false",
        "async",
        "await",
        "switch",
        "case",
        "break",
        "continue",
        "try",
        "catch",
        "throw",
        "with",
        "abstract",
        "mixin",
        "enum",
        "late",
        "required",
        "dynamic",
        "override",
    ],
    common_types: &[
        "String", "int", "double", "bool", "List", "Map", "Set", "Future", "Stream", "void",
        "dynamic", "Object", "Iterable", "Function", "Type", "Null", "num", "Never",
    ],
    container_body_kinds: &["class_body", "extension_body", "enum_body"],
    test_markers: &["@test", "test("],
    test_path_patterns: &["%_test.dart", "%/test/%"],
    entry_point_names: &["main"],
    doc_format: "triple_slash",
    doc_convention:
        "Use /// for documentation comments. Follow Effective Dart documentation guidelines.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "final late var static const",
    },
    extract_return_nl: extract_return_dart,
    ..DEFAULTS
};

#[cfg(feature = "lang-dart")]
fn extract_return_dart(sig: &str) -> Option<String> {
    // Dart: ReturnType functionName(params) — type is the first token before the name
    // For void, int, String, Future<X>, etc.
    let sig = sig.trim();
    // Skip if starts with a keyword that's not a return type
    if sig.starts_with("class ")
        || sig.starts_with("enum ")
        || sig.starts_with("mixin ")
        || sig.starts_with("extension ")
    {
        return None;
    }
    // Look for the type before the function name
    // Pattern: [modifiers] Type name(params)
    let paren = sig.find('(')?;
    let before_paren = sig[..paren].trim();
    let parts: Vec<&str> = before_paren.split_whitespace().collect();
    if parts.len() >= 2 {
        let type_part = parts[parts.len() - 2];
        if type_part == "void" {
            return None;
        }
        // Skip modifiers
        if [
            "static", "abstract", "external", "factory", "get", "set", "operator",
        ]
        .contains(&type_part)
        {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(type_part).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

#[cfg(feature = "lang-dart")]
pub fn definition_dart() -> &'static LanguageDef {
    &LANG_DART
}

// ============================================================================
// Elixir (elixir)
// ============================================================================

/// Post-process Elixir chunks to set correct chunk types.
fn post_process_elixir_elixir(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    // Get the keyword from the target identifier
    let keyword = node
        .child_by_field_name("target")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .unwrap_or("");

    match keyword {
        "def" | "defp" | "defguard" | "defguardp" | "defdelegate" => {
            *chunk_type = ChunkType::Function;
        }
        "defmacro" | "defmacrop" => {
            *chunk_type = ChunkType::Macro;
        }
        "defmodule" => {
            *chunk_type = ChunkType::Module;
        }
        "defprotocol" => {
            *chunk_type = ChunkType::Interface;
        }
        "defimpl" => {
            *chunk_type = ChunkType::Object;
        }
        "defstruct" => {
            // defstruct has no name argument — use enclosing module name if possible
            *chunk_type = ChunkType::Struct;
            // Walk up to find enclosing defmodule call
            let mut parent = node.parent();
            while let Some(p) = parent {
                if p.kind() == "call" {
                    if let Some(target) = p.child_by_field_name("target") {
                        if target.utf8_text(source.as_bytes()).ok() == Some("defmodule") {
                            // Find alias in arguments by walking children
                            let mut cursor = p.walk();
                            for child in p.named_children(&mut cursor) {
                                if child.kind() == "arguments" {
                                    let mut inner_cursor = child.walk();
                                    for arg in child.named_children(&mut inner_cursor) {
                                        if arg.kind() == "alias" {
                                            if let Ok(mod_name) = arg.utf8_text(source.as_bytes()) {
                                                *name = mod_name.to_string();
                                                return true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                parent = p.parent();
            }
            // If no enclosing module, discard
            return false;
        }
        _ => {}
    }
    true
}

/// Attempts to extract a return type annotation from a function signature.
/// # Arguments
/// * `_signature` - A function signature string to parse
/// # Returns
/// Returns `Option<String>` containing the extracted return type, or `None` if no return type annotation exists. In Elixir, this always returns `None` since the language does not support return type annotations in function signatures.
fn extract_return_elixir(_signature: &str) -> Option<String> {
    // Elixir is dynamically typed — no return type annotations in signatures
    None
}

static LANG_ELIXIR: LanguageDef = LanguageDef {
    name: "elixir",
    grammar: Some(|| tree_sitter_elixir::LANGUAGE.into()),
    extensions: &["ex", "exs"],
    chunk_query: include_str!("queries/elixir.chunks.scm"),
    call_query: Some(include_str!("queries/elixir.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "def",
        "defp",
        "defmodule",
        "defprotocol",
        "defimpl",
        "defmacro",
        "defmacrop",
        "defstruct",
        "defguard",
        "defguardp",
        "defdelegate",
        "defexception",
        "defoverridable",
        "do",
        "end",
        "fn",
        "case",
        "cond",
        "if",
        "else",
        "unless",
        "when",
        "with",
        "for",
        "receive",
        "try",
        "catch",
        "rescue",
        "after",
        "raise",
        "throw",
        "import",
        "require",
        "use",
        "alias",
        "nil",
        "true",
        "false",
        "and",
        "or",
        "not",
        "in",
        "is",
        "self",
        "super",
        "send",
        "spawn",
        "apply",
        "Enum",
        "List",
        "Map",
        "String",
        "IO",
        "Kernel",
        "Agent",
        "Task",
        "GenServer",
    ],
    extract_return_nl: extract_return_elixir,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_test.exs")),
    test_name_suggestion: Some(|name| format!("test \"{}\"", name)),
    container_body_kinds: &["do_block"],
    post_process_chunk: Some(post_process_elixir_elixir as PostProcessChunkFn),
    test_markers: &["test ", "describe "],
    test_path_patterns: &["%/test/%", "%_test.exs"],
    entry_point_names: &["start", "init", "handle_call", "handle_cast", "handle_info"],
    trait_method_names: &[
        "init",
        "handle_call",
        "handle_cast",
        "handle_info",
        "terminate",
        "code_change",
    ],
    doc_format: "elixir_doc",
    doc_convention: "Use @doc with ## Examples section per Elixir conventions.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["defmodule", "defstruct"],
    ..DEFAULTS
};

pub fn definition_elixir() -> &'static LanguageDef {
    &LANG_ELIXIR
}

// ============================================================================
// Erlang (erlang)
// ============================================================================

/// Post-process Erlang chunks to set correct chunk types.
fn post_process_erlang_erlang(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "fun_decl" => *chunk_type = ChunkType::Function,
        "module_attribute" => *chunk_type = ChunkType::Module,
        "type_alias" | "opaque" => *chunk_type = ChunkType::TypeAlias,
        "record_decl" => *chunk_type = ChunkType::Struct,
        "behaviour_attribute" => *chunk_type = ChunkType::Interface,
        "callback" => *chunk_type = ChunkType::Interface,
        "pp_define" => *chunk_type = ChunkType::Macro,
        _ => {}
    }
    true
}

/// Extracts the return type from an Erlang function signature.
/// # Arguments
/// * `signature` - A function signature string to parse
/// # Returns
/// Returns `None` because Erlang is dynamically typed and function signatures do not include explicit return type annotations.
fn extract_return_erlang(_signature: &str) -> Option<String> {
    // Erlang is dynamically typed — no return types in function heads
    None
}

static LANG_ERLANG: LanguageDef = LanguageDef {
    name: "erlang",
    grammar: Some(|| tree_sitter_erlang::LANGUAGE.into()),
    extensions: &["erl", "hrl"],
    chunk_query: include_str!("queries/erlang.chunks.scm"),
    call_query: Some(include_str!("queries/erlang.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "module",
        "export",
        "import",
        "behaviour",
        "behavior",
        "callback",
        "spec",
        "type",
        "opaque",
        "record",
        "define",
        "ifdef",
        "ifndef",
        "endif",
        "include",
        "include_lib",
        "fun",
        "end",
        "case",
        "of",
        "if",
        "receive",
        "after",
        "when",
        "try",
        "catch",
        "throw",
        "begin",
        "and",
        "or",
        "not",
        "band",
        "bor",
        "bxor",
        "bnot",
        "bsl",
        "bsr",
        "div",
        "rem",
        "true",
        "false",
        "undefined",
        "ok",
        "error",
        "self",
        "lists",
        "maps",
        "io",
        "gen_server",
        "gen_statem",
        "supervisor",
        "application",
        "ets",
        "mnesia",
        "erlang",
        "string",
        "binary",
    ],
    extract_return_nl: extract_return_erlang,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_SUITE.erl")),
    post_process_chunk: Some(post_process_erlang_erlang as PostProcessChunkFn),
    test_path_patterns: &["%/test/%", "%_SUITE.erl", "%_tests.erl"],
    entry_point_names: &[
        "start",
        "start_link",
        "init",
        "handle_call",
        "handle_cast",
        "handle_info",
    ],
    trait_method_names: &[
        "init",
        "handle_call",
        "handle_cast",
        "handle_info",
        "terminate",
        "code_change",
    ],
    doc_format: "erlang_edoc",
    doc_convention: "Use EDoc format: @param, @returns, @throws tags.",
    skip_line_prefixes: &["-record"],
    ..DEFAULTS
};

pub fn definition_erlang() -> &'static LanguageDef {
    &LANG_ERLANG
}

// ============================================================================
// Fsharp (fsharp)
// ============================================================================

/// Extracts the return type annotation from an F# function signature and formats it as a documentation string.
/// # Arguments
/// * `signature` - An F# function signature string (e.g., "let processData (input: string) : int =")
/// # Returns
/// Returns `Some(String)` containing a formatted return type description (e.g., "Returns int") if a non-unit return type annotation exists after the last colon outside of parentheses. Returns `None` if no '=' is found, no return type annotation exists, the return type is empty, or the return type is "unit".
fn extract_return_fsharp(signature: &str) -> Option<String> {
    // F#: optional return type annotation after last ':' before '='
    // e.g., "let processData (input: string) : int =" → "int"
    // Must handle nested parens (parameter types also use ':')
    let eq_pos = signature.find('=')?;
    let before_eq = &signature[..eq_pos];

    // Find the last ':' that's outside parentheses
    let mut paren_depth = 0i32;
    let mut last_colon_outside = None;
    for (i, ch) in before_eq.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            ':' if paren_depth == 0 => last_colon_outside = Some(i),
            _ => {}
        }
    }

    let colon_pos = last_colon_outside?;
    let ret_type = before_eq[colon_pos + 1..].trim();
    if ret_type.is_empty() || ret_type == "unit" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

/// Extract container type name for F# type definitions.
/// F# containers (anon_type_defn, record_type_defn, etc.) store the name
/// in a child `type_name` node's `type_name` field — not a direct `name` field.
fn extract_container_name_fsharp_fsharp(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "type_name" {
            if let Some(name) = child.child_by_field_name("type_name") {
                return Some(source[name.byte_range()].to_string());
            }
        }
    }
    None
}

static LANG_FSHARP: LanguageDef = LanguageDef {
    name: "fsharp",
    grammar: Some(|| tree_sitter_fsharp::LANGUAGE_FSHARP.into()),
    extensions: &["fs", "fsi"],
    chunk_query: include_str!("queries/fsharp.chunks.scm"),
    call_query: Some(include_str!("queries/fsharp.calls.scm")),
    doc_nodes: &["line_comment", "block_comment"],
    method_containers: &[
        "anon_type_defn",
        "interface_type_defn",
        "record_type_defn",
        "union_type_defn",
    ],
    stopwords: &[
        "let",
        "in",
        "if",
        "then",
        "else",
        "match",
        "with",
        "fun",
        "function",
        "type",
        "module",
        "open",
        "do",
        "for",
        "while",
        "yield",
        "return",
        "mutable",
        "rec",
        "and",
        "or",
        "not",
        "true",
        "false",
        "null",
        "abstract",
        "member",
        "override",
        "static",
        "private",
        "public",
        "internal",
        "val",
        "new",
        "inherit",
        "interface",
        "end",
        "begin",
        "of",
        "as",
        "when",
        "upcast",
        "downcast",
        "use",
        "try",
        "finally",
        "raise",
        "async",
        "task",
    ],
    extract_return_nl: extract_return_fsharp,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.fs")),
    type_query: Some(include_str!("queries/fsharp.types.scm")),
    common_types: &[
        "string",
        "int",
        "bool",
        "float",
        "decimal",
        "byte",
        "char",
        "unit",
        "obj",
        "int64",
        "uint",
        "int16",
        "double",
        "nativeint",
        "bigint",
        "seq",
        "list",
        "array",
        "option",
        "voption",
        "result",
        "Map",
        "Set",
        "Dictionary",
        "HashSet",
        "ResizeArray",
        "Task",
        "Async",
        "IDisposable",
        "IEnumerable",
        "IComparable",
        "Exception",
        "StringBuilder",
        "CancellationToken",
    ],
    extract_container_name: Some(extract_container_name_fsharp_fsharp),
    test_markers: &["[<Test>]", "[<Fact>]", "[<Theory>]"],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%Tests.fs"],
    entry_point_names: &["main"],
    trait_method_names: &["Equals", "GetHashCode", "ToString", "CompareTo", "Dispose"],
    doc_format: "triple_slash",
    doc_convention: "Use XML doc comments: <summary>, <param>, <returns> tags.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "mutable",
    },
    skip_line_prefixes: &["type "],
    ..DEFAULTS
};

pub fn definition_fsharp() -> &'static LanguageDef {
    &LANG_FSHARP
}

// ============================================================================
// Gleam (gleam)
// ============================================================================

/// Post-process Gleam chunks to set correct chunk types.
fn post_process_gleam_gleam(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "function" => *chunk_type = ChunkType::Function,
        "type_definition" => *chunk_type = ChunkType::Enum,
        "type_alias" => *chunk_type = ChunkType::TypeAlias,
        "constant" => *chunk_type = ChunkType::Constant,
        _ => {}
    }
    true
}

/// Extract return type from Gleam function signatures.
/// Gleam signatures: `fn add_gleam(x: Int, y: Int) -> Int {`
/// Return type is after `->`.
fn extract_return_gleam(signature: &str) -> Option<String> {
    let trimmed = signature.trim();

    // fn name_gleam(params) -> ReturnType {
    let arrow = trimmed.find("->")?;
    let after = trimmed[arrow + 2..].trim();

    // Remove opening brace
    let ret = after.split('{').next()?.trim();

    if ret.is_empty() {
        return None;
    }

    // Skip Nil (void equivalent)
    if ret == "Nil" {
        return None;
    }

    let words = crate::nl::tokenize_identifier(ret).join(" ");
    Some(format!("Returns {}", words.to_lowercase()))
}

static LANG_GLEAM: LanguageDef = LanguageDef {
    name: "gleam",
    grammar: Some(|| tree_sitter_gleam::LANGUAGE.into()),
    extensions: &["gleam"],
    chunk_query: include_str!("queries/gleam.chunks.scm"),
    call_query: Some(include_str!("queries/gleam.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["module_comment", "statement_comment", "comment"],
    stopwords: &[
        "fn", "pub", "let", "assert", "case", "if", "else", "use", "import", "type", "const",
        "opaque", "external", "todo", "panic", "as", "try", "Ok", "Error", "True", "False", "Nil",
        "Int", "Float", "String", "Bool", "List", "Result", "Option", "BitArray", "Dict", "io",
        "int", "float", "string", "list", "result", "option", "dict", "map",
    ],
    extract_return_nl: extract_return_gleam,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_test.gleam")),
    common_types: &[
        "Int", "Float", "String", "Bool", "List", "Result", "Option", "Nil", "BitArray", "Dict",
    ],
    post_process_chunk: Some(post_process_gleam_gleam as PostProcessChunkFn),
    test_path_patterns: &["%/test/%", "%_test.gleam"],
    entry_point_names: &["main"],
    doc_convention: "Use /// doc comments describing parameters and return values.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "pub",
    },
    skip_line_prefixes: &["type ", "pub type"],
    ..DEFAULTS
};

pub fn definition_gleam() -> &'static LanguageDef {
    &LANG_GLEAM
}

// ============================================================================
// Glsl (glsl)
// ============================================================================

/// Extracts the return type from a C-style function signature and formats it as a documentation string.
/// Parses a function signature to identify the return type (the portion before the opening parenthesis). Filters out storage class and precision qualifiers (static, inline, const, volatile, highp, mediump, lowp). Skips void return types and signatures without a clear return type. Tokenizes the resulting type identifier and formats it as a returns documentation string.
/// # Arguments
/// `signature` - A function signature string in C-style format (return type followed by function name and parameters).
/// # Returns
/// `Some(String)` containing a formatted returns documentation string if a non-void return type is found, or `None` if the signature has no return type, contains only void, or is malformed.
fn extract_return_glsl(signature: &str) -> Option<String> {
    // C-style: return type before function name
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            let type_words: Vec<&str> = words[..words.len() - 1]
                .iter()
                .filter(|w| {
                    !matches!(
                        **w,
                        "static" | "inline" | "const" | "volatile" | "highp" | "mediump" | "lowp"
                    )
                })
                .copied()
                .collect();
            if !type_words.is_empty() && type_words != ["void"] {
                let ret = type_words.join(" ");
                let ret_words = crate::nl::tokenize_identifier(&ret).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static LANG_GLSL: LanguageDef = LanguageDef {
    name: "glsl",
    grammar: Some(|| tree_sitter_glsl::LANGUAGE_GLSL.into()),
    extensions: &["glsl", "vert", "frag", "geom", "comp", "tesc", "tese"],
    chunk_query: include_str!("queries/glsl.chunks.scm"),
    call_query: Some(include_str!("queries/glsl.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        // C stopwords
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "typedef",
        "struct",
        "enum",
        "union",
        "void",
        "int",
        "char",
        "float",
        "double",
        "const",
        "static",
        "sizeof",
        "true",
        "false",
        // GLSL-specific qualifiers and types
        "uniform",
        "varying",
        "attribute",
        "in",
        "out",
        "inout",
        "flat",
        "smooth",
        "noperspective",
        "centroid",
        "sample",
        "patch",
        "layout",
        "location",
        "binding",
        "set",
        "push_constant",
        "precision",
        "lowp",
        "mediump",
        "highp",
        "vec2",
        "vec3",
        "vec4",
        "ivec2",
        "ivec3",
        "ivec4",
        "uvec2",
        "uvec3",
        "uvec4",
        "bvec2",
        "bvec3",
        "bvec4",
        "mat2",
        "mat3",
        "mat4",
        "mat2x3",
        "mat3x4",
        "sampler2D",
        "sampler3D",
        "samplerCube",
        "sampler2DShadow",
        "texture",
        "discard",
        "gl_Position",
        "gl_FragColor",
    ],
    extract_return_nl: extract_return_glsl,
    common_types: &[
        "int",
        "float",
        "double",
        "void",
        "bool",
        "vec2",
        "vec3",
        "vec4",
        "ivec2",
        "ivec3",
        "ivec4",
        "uvec2",
        "uvec3",
        "uvec4",
        "bvec2",
        "bvec3",
        "bvec4",
        "mat2",
        "mat3",
        "mat4",
        "mat2x3",
        "mat2x4",
        "mat3x2",
        "mat3x4",
        "mat4x2",
        "mat4x3",
        "sampler2D",
        "sampler3D",
        "samplerCube",
        "sampler2DShadow",
    ],
    entry_point_names: &["main"],
    doc_format: "javadoc",
    doc_convention: "Use Doxygen format: @param, @return tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes: "static const volatile extern unsigned signed",
    },
    skip_line_prefixes: &["struct "],
    ..DEFAULTS
};

pub fn definition_glsl() -> &'static LanguageDef {
    &LANG_GLSL
}

// ============================================================================
// Go (go)
// ============================================================================

/// Extracts the return type from a Go function signature string.
/// # Arguments
/// * `signature` - A Go function signature string, potentially including the trailing `{` brace
/// # Returns
/// Returns `Some(String)` containing a formatted return type description if a return type is found in the signature. The returned string is prefixed with "Returns " and contains either the multi-return tuple (e.g., "(string, error)") or a single return type with tokenized identifiers. Returns `None` if no return type is present or the signature format is invalid.
fn extract_return_go(signature: &str) -> Option<String> {
    // Go: `func name(params) returnType {` or `func (recv) name(params) returnType {`
    // Strip trailing { first
    let sig = signature.trim_end_matches('{').trim();

    if sig.ends_with(')') {
        // Check if it's a multi-return like (string, error)
        // Find the matching ( for the final )
        let mut depth = 0;
        let mut start_idx = None;
        for (i, c) in sig.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        start_idx = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(start) = start_idx {
            // Check if there's a ) before this ( - that would be the params close
            let before = &sig[..start].trim();
            if before.ends_with(')') {
                // Multi-return: extract the (...)
                let ret = &sig[start..];
                if !ret.is_empty() {
                    return Some(format!("Returns {}", ret));
                }
            }
        }
        return None;
    } else {
        // Plain return type after last )
        if let Some(paren) = sig.rfind(')') {
            let ret = sig[paren + 1..].trim();
            if ret.is_empty() {
                return None;
            }
            let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
            return Some(format!("Returns {}", ret_words));
        }
    }
    None
}

/// Post-process Go chunks: reclassify `New*` functions as Constructor (convention).
/// Go convention: `func NewTypeName(...)` is a constructor for TypeName.
#[allow(clippy::ptr_arg)] // signature must match PostProcessChunkFn type alias
fn post_process_go_go(
    name: &mut String,
    chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Go convention: top-level func NewFoo(...) is a constructor
    if *chunk_type == ChunkType::Function && name.starts_with("New") && name.len() > 3 {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

static LANG_GO: LanguageDef = LanguageDef {
    name: "go",
    grammar: Some(|| tree_sitter_go::LANGUAGE.into()),
    extensions: &["go"],
    chunk_query: include_str!("queries/go.chunks.scm"),
    call_query: Some(include_str!("queries/go.calls.scm")),
    doc_nodes: &["comment"],
    method_node_kinds: &["method_declaration"],
    stopwords: &[
        "func",
        "var",
        "const",
        "type",
        "struct",
        "interface",
        "return",
        "if",
        "else",
        "for",
        "range",
        "switch",
        "case",
        "break",
        "continue",
        "go",
        "defer",
        "select",
        "chan",
        "map",
        "package",
        "import",
        "true",
        "false",
        "nil",
    ],
    extract_return_nl: extract_return_go,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}_test.go")),
    test_name_suggestion: Some(|name| super::pascal_test_name("Test", name)),
    type_query: Some(include_str!("queries/go.types.scm")),
    common_types: &[
        "string",
        "int",
        "int8",
        "int16",
        "int32",
        "int64",
        "uint",
        "uint8",
        "uint16",
        "uint32",
        "uint64",
        "float32",
        "float64",
        "bool",
        "byte",
        "rune",
        "error",
        "any",
        "comparable",
        "Context",
    ],
    post_process_chunk: Some(post_process_go_go as PostProcessChunkFn),
    test_path_patterns: &["%\\_test.go"],
    entry_point_names: &["main", "init"],
    trait_method_names: &[
        "String",
        "Error",
        "Close",
        "Read",
        "Write",
        "ServeHTTP",
        "Len",
        "Less",
        "Swap",
        "MarshalJSON",
        "UnmarshalJSON",
    ],
    doc_format: "go_comment",
    doc_convention: "Start with the function name per Go conventions.",
    field_style: FieldStyle::NameFirst {
        separators: " ",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["type ", "func "],
    ..DEFAULTS
};

pub fn definition_go() -> &'static LanguageDef {
    &LANG_GO
}

// ============================================================================
// Graphql (graphql)
// ============================================================================

static LANG_GRAPHQL: LanguageDef = LanguageDef {
    name: "graphql",
    grammar: Some(|| tree_sitter_graphql::LANGUAGE.into()),
    extensions: &["graphql", "gql"],
    chunk_query: include_str!("queries/graphql.chunks.scm"),
    call_query: Some(include_str!("queries/graphql.calls.scm")),
    doc_nodes: &["description"],
    stopwords: &[
        "type",
        "interface",
        "enum",
        "union",
        "input",
        "scalar",
        "directive",
        "query",
        "mutation",
        "subscription",
        "fragment",
        "on",
        "extend",
        "implements",
        "schema",
        "true",
        "false",
        "null",
        "repeatable",
    ],
    common_types: &["String", "Int", "Float", "Boolean", "ID"],
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["type ", "input ", "interface ", "enum "],
    ..DEFAULTS
};

pub fn definition_graphql() -> &'static LanguageDef {
    &LANG_GRAPHQL
}

// ============================================================================
// Haskell (haskell)
// ============================================================================

/// Post-process Haskell chunks to set correct chunk types.
fn post_process_haskell_haskell(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "function" => *chunk_type = ChunkType::Function,
        "data_type" => *chunk_type = ChunkType::Enum,
        "newtype" => *chunk_type = ChunkType::Struct,
        "type_synomym" => *chunk_type = ChunkType::TypeAlias,
        "class" => *chunk_type = ChunkType::Trait,
        "instance" => *chunk_type = ChunkType::Impl,
        _ => {}
    }
    true
}

/// Extract return type from Haskell type signatures.
/// Haskell signatures: `foo :: Int -> Bool -> String`
/// Return type is the last type after the final `->`.
fn extract_return_haskell(signature: &str) -> Option<String> {
    // Look for :: to find the type signature part
    let type_part = signature.split("::").nth(1)?;

    // The return type is after the last ->
    let return_type = if type_part.contains("->") {
        type_part.rsplit("->").next()?.trim()
    } else {
        // No arrows — single type (e.g., `foo :: Int`)
        type_part.trim()
    };

    // Clean up: strip leading/trailing whitespace and "where" clauses
    let return_type = return_type.split("where").next()?.trim();

    if return_type.is_empty() {
        return None;
    }

    // Skip IO/monadic wrappers — extract inner type if wrapped
    let clean = return_type.strip_prefix("IO ").unwrap_or(return_type);

    // Strip parentheses
    let clean = clean.trim_start_matches('(').trim_end_matches(')').trim();

    if clean.is_empty() || clean == "()" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(clean).join(" ");
    Some(format!("Returns {}", ret_words.to_lowercase()))
}

static LANG_HASKELL: LanguageDef = LanguageDef {
    name: "haskell",
    grammar: Some(|| tree_sitter_haskell::LANGUAGE.into()),
    extensions: &["hs"],
    chunk_query: include_str!("queries/haskell.chunks.scm"),
    call_query: Some(include_str!("queries/haskell.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "module",
        "where",
        "import",
        "qualified",
        "as",
        "hiding",
        "data",
        "type",
        "newtype",
        "class",
        "instance",
        "deriving",
        "do",
        "let",
        "in",
        "case",
        "of",
        "if",
        "then",
        "else",
        "forall",
        "infixl",
        "infixr",
        "infix",
        "default",
        "foreign",
        "True",
        "False",
        "Nothing",
        "Just",
        "Maybe",
        "Either",
        "Left",
        "Right",
        "IO",
        "Int",
        "Integer",
        "Float",
        "Double",
        "Char",
        "String",
        "Bool",
        "Show",
        "Read",
        "Eq",
        "Ord",
        "Num",
        "Monad",
        "Functor",
        "Applicative",
        "Foldable",
        "Traversable",
        "return",
        "pure",
        "putStrLn",
        "print",
        "map",
        "filter",
        "fmap",
    ],
    extract_return_nl: extract_return_haskell,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}Spec.hs")),
    common_types: &[
        "Int", "Integer", "Float", "Double", "Char", "String", "Bool", "IO", "Maybe", "Either",
        "Show", "Read", "Eq", "Ord", "Num",
    ],
    container_body_kinds: &["class_declarations", "instance_declarations"],
    post_process_chunk: Some(post_process_haskell_haskell as PostProcessChunkFn),
    test_markers: &["hspec", "describe", "it ", "prop "],
    test_path_patterns: &["%/test/%", "%Spec.hs", "%Test.hs"],
    entry_point_names: &["main"],
    trait_method_names: &[
        "show",
        "read",
        "readsPrec",
        "showsPrec",
        "compare",
        "fmap",
        "pure",
        "return",
        "fromInteger",
    ],
    doc_format: "haskell_haddock",
    doc_convention: "Use Haddock format with -- | comments.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["data ", "newtype ", "type "],
    ..DEFAULTS
};

pub fn definition_haskell() -> &'static LanguageDef {
    &LANG_HASKELL
}

// ============================================================================
// Hcl (hcl)
// ============================================================================

/// Heredoc identifiers that suggest shell script content.
const SHELL_HEREDOC_IDS_HCL: &[&str] = &[
    "BASH",
    "SHELL",
    "SH",
    "SCRIPT",
    "EOT",
    "EOF",
    "USERDATA",
    "USER_DATA",
];

/// Detect the language of an HCL heredoc based on its identifier.
/// Checks `heredoc_identifier` child of the `heredoc_template` node.
/// Shell-like identifiers (BASH, SHELL, EOT, EOF, etc.) return `None`
/// (use default bash). `PYTHON` returns `Some("python")`. Unrecognized
/// identifiers return `Some("_skip")`.
fn detect_heredoc_language_hcl(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "heredoc_identifier" {
            let ident = source[child.byte_range()].trim().to_uppercase();
            if SHELL_HEREDOC_IDS_HCL.contains(&ident.as_str()) {
                tracing::debug!(identifier = %ident, "HCL heredoc identified as shell");
                return None; // Use default bash
            }
            if ident == "PYTHON" || ident == "PY" {
                tracing::debug!(identifier = %ident, "HCL heredoc identified as python");
                return Some("python");
            }
            tracing::debug!(identifier = %ident, "HCL heredoc identifier not recognized, skipping");
            return Some("_skip");
        }
    }
    // No heredoc_identifier found — might be a template_literal, skip
    Some("_skip")
}

/// Post-process HCL blocks to determine correct name and ChunkType.
/// HCL's tree-sitter grammar represents all blocks as generic `block` nodes.
/// This hook walks the block's children to extract the block type (first identifier)
/// and string labels, then assigns the correct ChunkType and qualified name.
/// Filters out:
/// - Nested blocks (provisioner/lifecycle inside resources)
/// - Blocks with no labels (locals, terraform, required_providers)
fn post_process_hcl_hcl(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let _span = tracing::debug_span!("post_process_hcl", name = %name).entered();

    // Filter nested blocks: if parent is a body whose parent is another block, skip.
    // This prevents provisioner/lifecycle/connection inside resources from becoming chunks.
    if let Some(parent) = node.parent() {
        if parent.kind() == "body" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "block" {
                    tracing::debug!("Skipping nested block inside parent block");
                    return false;
                }
            }
        }
    }

    let mut block_type = None;
    let mut labels: Vec<String> = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" if block_type.is_none() => {
                block_type = Some(source[child.byte_range()].to_string());
            }
            "string_lit" => {
                // Extract template_literal content (quote-free)
                let mut inner = child.walk();
                let mut found = false;
                for c in child.children(&mut inner) {
                    if c.kind() == "template_literal" {
                        labels.push(source[c.byte_range()].to_string());
                        found = true;
                    }
                }
                if !found {
                    // string_lit with no template_literal (empty string or interpolation-only)
                    tracing::trace!("string_lit with no template_literal child, skipping label");
                }
            }
            _ => {}
        }
    }

    let bt = block_type.as_deref().unwrap_or("");

    // Skip blocks with no labels (locals, terraform, required_providers)
    if labels.is_empty() {
        tracing::debug!(block_type = bt, "Skipping block with no labels");
        return false;
    }

    // Safe label access — guaranteed non-empty after check above
    let last_label = &labels[labels.len() - 1];

    match bt {
        "resource" | "data" => {
            *chunk_type = ChunkType::Struct;
            *name = if labels.len() >= 2 {
                format!("{}.{}", labels[0], labels[1])
            } else {
                last_label.clone()
            };
        }
        "variable" | "output" => {
            *chunk_type = ChunkType::Constant;
            *name = last_label.clone();
        }
        "module" => {
            *chunk_type = ChunkType::Module;
            *name = last_label.clone();
        }
        _ => {
            // provider, backend, unknown block types → Struct
            *chunk_type = ChunkType::Struct;
            *name = last_label.clone();
        }
    }

    tracing::debug!(
        block_type = bt,
        name = %name,
        chunk_type = ?chunk_type,
        "Reclassified HCL block"
    );
    true
}

static LANG_HCL: LanguageDef = LanguageDef {
    name: "hcl",
    grammar: Some(|| tree_sitter_hcl::LANGUAGE.into()),
    extensions: &["tf", "tfvars", "hcl"],
    chunk_query: include_str!("queries/hcl.chunks.scm"),
    call_query: Some(include_str!("queries/hcl.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        "resource",
        "data",
        "variable",
        "output",
        "module",
        "provider",
        "terraform",
        "locals",
        "backend",
        "required_providers",
        "required_version",
        "count",
        "for_each",
        "depends_on",
        "lifecycle",
        "provisioner",
        "connection",
        "source",
        "version",
        "type",
        "default",
        "description",
        "sensitive",
        "validation",
        "condition",
        "error_message",
        "true",
        "false",
        "null",
        "each",
        "self",
        "var",
        "local",
        "path",
    ],
    post_process_chunk: Some(post_process_hcl_hcl),
    injections: &[
        // Heredoc templates with shell-like identifiers (EOT, BASH, etc.)
        // contain bash scripts. detect_heredoc_language checks the identifier
        // and skips non-shell content.
        InjectionRule {
            container_kind: "heredoc_template",
            content_kind: "template_literal",
            target_language: "bash",
            detect_language: Some(detect_heredoc_language_hcl),
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_hcl() -> &'static LanguageDef {
    &LANG_HCL
}

// ============================================================================
// Html (html)
// ============================================================================

/// Semantic landmark tags that become Section chunks.
const LANDMARK_TAGS_HTML: &[&str] = &[
    "nav", "main", "header", "footer", "section", "article", "aside", "form",
];

/// Tags to filter out as structural noise (unless they have an id).
const NOISE_TAGS_HTML: &[&str] = &[
    "html",
    "head",
    "body",
    "div",
    "span",
    "p",
    "ul",
    "ol",
    "li",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "br",
    "hr",
    "img",
    "a",
    "em",
    "strong",
    "b",
    "i",
    "u",
    "small",
    "sub",
    "sup",
    "abbr",
    "code",
    "pre",
    "blockquote",
    "dl",
    "dt",
    "dd",
    "link",
    "meta",
    "title",
    "base",
];

/// Post-process HTML chunks: classify by semantic role, filter noise.
fn post_process_html_html(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let tag = name.as_str();

    // Headings → Section
    if matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        *chunk_type = ChunkType::Section;
        // Try to extract heading text content
        if let Some(text) = extract_element_text_html(node, source) {
            if !text.is_empty() {
                *name = text;
            }
        }
        return true;
    }

    // Script and style → Module
    if tag == "script" || tag == "style" {
        *chunk_type = ChunkType::Module;
        // Try to get script type/src attribute for a better name
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(start) = start_tag {
            if let Some(attr_val) = find_attribute_value_html(start, "src", source) {
                *name = format!("script:{attr_val}");
            } else if let Some(attr_val) = find_attribute_value_html(start, "type", source) {
                *name = format!("{tag}:{attr_val}");
            }
        }
        return true;
    }

    // Semantic landmarks → Section
    if LANDMARK_TAGS_HTML.contains(&tag) {
        *chunk_type = ChunkType::Section;
        // Check for id or aria-label
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(start) = start_tag {
            if let Some(id) = find_attribute_value_html(start, "id", source) {
                *name = format!("{tag}#{id}");
            } else if let Some(label) = find_attribute_value_html(start, "aria-label", source) {
                *name = format!("{tag}:{label}");
            }
        }
        return true;
    }

    // Check if this noise tag has an id — keep it as Property
    if NOISE_TAGS_HTML.contains(&tag) {
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(start) = start_tag {
            if let Some(id) = find_attribute_value_html(start, "id", source) {
                *name = format!("{tag}#{id}");
                *chunk_type = ChunkType::Property;
                return true;
            }
        }
        // No id — filter out
        return false;
    }

    // Everything else: keep as Property
    true
}

/// Find a direct child node by kind.
pub(crate) fn find_child_by_kind_html<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    crate::parser::find_child_by_kind(node, kind)
}

/// Check if a start_tag has an attribute by name (including boolean/valueless attributes like `setup`).
pub(crate) fn has_attribute_html(
    start_tag: tree_sitter::Node,
    attr_name: &str,
    source: &str,
) -> bool {
    let mut cursor = start_tag.walk();
    for child in start_tag.children(&mut cursor) {
        if child.kind() == "attribute" {
            let mut attr_cursor = child.walk();
            for attr_child in child.children(&mut attr_cursor) {
                if attr_child.kind() == "attribute_name" {
                    let name_text = attr_child.utf8_text(source.as_bytes()).unwrap_or("");
                    if name_text == attr_name {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Find an attribute's value within a start_tag node.
pub(crate) fn find_attribute_value_html(
    start_tag: tree_sitter::Node,
    attr_name: &str,
    source: &str,
) -> Option<String> {
    let mut cursor = start_tag.walk();
    for child in start_tag.children(&mut cursor) {
        if child.kind() == "attribute" {
            // attribute has attribute_name and optionally quoted_attribute_value children
            let mut attr_cursor = child.walk();
            let mut found_name = false;
            for attr_child in child.children(&mut attr_cursor) {
                if attr_child.kind() == "attribute_name" {
                    let name_text = attr_child.utf8_text(source.as_bytes()).unwrap_or("");
                    if name_text == attr_name {
                        found_name = true;
                    }
                } else if found_name
                    && (attr_child.kind() == "quoted_attribute_value"
                        || attr_child.kind() == "attribute_value")
                {
                    let val = attr_child.utf8_text(source.as_bytes()).unwrap_or("");
                    // Strip quotes if present
                    let val = val.trim_matches('"').trim_matches('\'');
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// Extract text content from an element (for heading text).
fn extract_element_text_html(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "text" {
            let text = child.utf8_text(source.as_bytes()).unwrap_or("").trim();
            if !text.is_empty() {
                // Truncate long heading text
                let truncated = if text.len() > 80 {
                    format!("{}...", &text[..text.floor_char_boundary(77)])
                } else {
                    text.to_string()
                };
                return Some(truncated);
            }
        }
    }
    None
}

/// Extracts the return type from a function signature.
/// # Arguments
/// * `signature` - A string slice containing a function signature to parse
/// # Returns
/// Returns `None` if no return type can be extracted (e.g., for HTML which has no functions or return types).
fn extract_return_html(_signature: &str) -> Option<String> {
    // HTML has no functions or return types
    None
}

/// Detect script language from `<script>` element attributes.
/// Checks for `lang="ts"`, `type="text/typescript"`, or similar attributes
/// that indicate TypeScript instead of the default JavaScript.
/// Shared between HTML and Svelte — both use `<script lang="ts">` for TypeScript.
pub(crate) fn detect_script_language_html(
    node: tree_sitter::Node,
    source: &str,
) -> Option<&'static str> {
    // Find the start_tag child
    let start_tag = find_child_by_kind_html(node, "start_tag")?;

    // Check lang attribute: <script lang="ts">
    if let Some(lang_val) = find_attribute_value_html(start_tag, "lang", source) {
        let lower = lang_val.to_lowercase();
        if lower == "ts" || lower == "typescript" {
            tracing::debug!("Detected TypeScript from lang attribute");
            return Some("typescript");
        }
    }

    // Check type attribute: <script type="text/typescript">
    if let Some(type_val) = find_attribute_value_html(start_tag, "type", source) {
        let lower = type_val.to_lowercase();
        if lower.contains("typescript") {
            tracing::debug!("Detected TypeScript from type attribute");
            return Some("typescript");
        }
        // Skip non-JS script types (JSON-LD, templates, shaders, etc.)
        if !lower.is_empty()
            && !matches!(
                lower.as_str(),
                "text/javascript"
                    | "application/javascript"
                    | "module"
                    | "text/ecmascript"
                    | "application/ecmascript"
            )
        {
            tracing::debug!(r#type = %type_val, "Skipping non-JS script type");
            return Some("_skip"); // sentinel: caller will skip injection
        }
    }

    None // Use default (javascript)
}

static LANG_HTML: LanguageDef = LanguageDef {
    name: "html",
    grammar: Some(|| tree_sitter_html::LANGUAGE.into()),
    extensions: &["html", "htm", "xhtml"],
    chunk_query: include_str!("queries/html.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "div",
        "span",
        "class",
        "style",
        "href",
        "src",
        "alt",
        "title",
        "type",
        "value",
        "name",
        "content",
        "http",
        "equiv",
        "charset",
        "viewport",
        "width",
        "height",
        "rel",
        "stylesheet",
    ],
    extract_return_nl: extract_return_html,
    post_process_chunk: Some(post_process_html_html as PostProcessChunkFn),
    injections: &[
        InjectionRule {
            container_kind: "script_element",
            content_kind: "raw_text",
            target_language: "javascript",
            detect_language: Some(detect_script_language_html),
            content_scoped_lines: false,
        },
        InjectionRule {
            container_kind: "style_element",
            content_kind: "raw_text",
            target_language: "css",
            detect_language: None,
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_html() -> &'static LanguageDef {
    &LANG_HTML
}

// ============================================================================
// Ini (ini)
// ============================================================================

/// Extracts the return type from a function signature.
/// # Arguments
/// * `_signature` - A function signature string (unused, as INI format does not support functions)
/// # Returns
/// Returns `None`, as INI files do not contain function definitions or return types.
fn extract_return_ini(_signature: &str) -> Option<String> {
    // INI has no functions or return types
    None
}

static LANG_INI: LanguageDef = LanguageDef {
    name: "ini",
    grammar: Some(|| tree_sitter_ini::LANGUAGE.into()),
    extensions: &["ini", "cfg"],
    chunk_query: include_str!("queries/ini.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &["true", "false", "yes", "no", "on", "off"],
    extract_return_nl: extract_return_ini,
    ..DEFAULTS
};

pub fn definition_ini() -> &'static LanguageDef {
    &LANG_INI
}

// ============================================================================
// Java (java)
// ============================================================================

/// Post-process Java chunks: promote `static final` fields from Property to Constant,
/// and reclassify `constructor_declaration` nodes as Constructor.
fn post_process_java_java(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    if *chunk_type == ChunkType::Property && node.kind() == "field_declaration" {
        // Check if modifiers contain both "static" and "final"
        let field_text = &source[node.start_byte()..node.end_byte()];
        // Look at text before the type/name: modifiers come first
        let has_static = field_text.contains("static");
        let has_final = field_text.contains("final");
        if has_static && has_final {
            *chunk_type = ChunkType::Constant;
        }
    }
    // constructor_declaration nodes are constructors
    if node.kind() == "constructor_declaration"
        && matches!(*chunk_type, ChunkType::Function | ChunkType::Method)
    {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

/// Extracts the return type from a Java method signature and formats it as a documentation string.
/// Parses a Java method signature to identify the return type by finding the opening parenthesis and analyzing the words preceding it. The return type is assumed to be the second-to-last word before the parenthesis (the last word being the method name). Filters out Java modifiers and keywords that are not actual return types.
/// # Arguments
/// * `signature` - A Java method signature string (e.g., "public int add(int a, int b)")
/// # Returns
/// `Some(String)` containing a formatted return type description if a valid return type is found, or `None` if the signature cannot be parsed or the return type is a modifier/keyword rather than an actual type.
fn extract_return_java(signature: &str) -> Option<String> {
    // Java: return type is before the method name, similar to C
    // e.g., "public int add(int a, int b)" or "private static String getName()"
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            // Last word is method name, second-to-last is return type
            let ret_type = words[words.len() - 2];
            if !matches!(
                ret_type,
                "void"
                    | "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "final"
                    | "abstract"
                    | "synchronized"
                    | "native"
            ) {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static LANG_JAVA: LanguageDef = LanguageDef {
    name: "java",
    grammar: Some(|| tree_sitter_java::LANGUAGE.into()),
    extensions: &["java"],
    chunk_query: include_str!("queries/java.chunks.scm"),
    call_query: Some(include_str!("queries/java.calls.scm")),
    doc_nodes: &["line_comment", "block_comment"],
    method_containers: &["class_body", "class_declaration"],
    stopwords: &[
        "public",
        "private",
        "protected",
        "static",
        "final",
        "abstract",
        "class",
        "interface",
        "extends",
        "implements",
        "return",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "new",
        "this",
        "super",
        "try",
        "catch",
        "finally",
        "throw",
        "throws",
        "import",
        "package",
        "void",
        "int",
        "boolean",
        "string",
        "true",
        "false",
        "null",
    ],
    extract_return_nl: extract_return_java,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.java")),
    test_name_suggestion: Some(|name| super::pascal_test_name("test", name)),
    type_query: Some(include_str!("queries/java.types.scm")),
    common_types: &[
        "String",
        "Object",
        "Integer",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "Byte",
        "Character",
        "List",
        "ArrayList",
        "Map",
        "HashMap",
        "Set",
        "HashSet",
        "Collection",
        "Iterator",
        "Iterable",
        "Optional",
        "Stream",
        "Exception",
        "RuntimeException",
        "IOException",
        "Class",
        "Void",
        "Comparable",
        "Serializable",
        "Cloneable",
    ],
    container_body_kinds: &["class_body"],
    post_process_chunk: Some(post_process_java_java as PostProcessChunkFn),
    test_markers: &["@Test", "@ParameterizedTest", "@RepeatedTest"],
    test_path_patterns: &["%/test/%", "%/tests/%", "%Test.java"],
    entry_point_names: &["main"],
    trait_method_names: &[
        "equals",
        "hashCode",
        "toString",
        "compareTo",
        "clone",
        "iterator",
        "run",
        "call",
        "close",
        "accept",
        "apply",
        "get",
    ],
    doc_format: "javadoc",
    doc_convention: "Use Javadoc format: @param, @return, @throws tags.",
    field_style: FieldStyle::TypeFirst {
        strip_prefixes: "private protected public static final volatile transient",
    },
    skip_line_prefixes: &[
        "class ",
        "interface ",
        "enum ",
        "public class",
        "abstract class",
    ],
    ..DEFAULTS
};

pub fn definition_java() -> &'static LanguageDef {
    &LANG_JAVA
}

// ============================================================================
// Javascript (javascript)
// ============================================================================

/// Returns true if the node is nested inside a function/method/arrow body.
fn is_inside_function_javascript(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        match parent.kind() {
            "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function" => return true,
            _ => {}
        }
        cursor = parent.parent();
    }
    false
}

/// Post-process JavaScript chunks: skip `@const` captures whose value is an arrow_function
/// or function_expression (already captured as Function), and skip const inside function bodies.
fn post_process_javascript_javascript(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        // Skip const declarations inside function bodies — only capture module-level
        if is_inside_function_javascript(node) {
            return false;
        }
        // node is the variable_declarator; check if the value child is a function
        if let Some(value) = node.child_by_field_name("value") {
            let kind = value.kind();
            if kind == "arrow_function" || kind == "function_expression" || kind == "function" {
                return false;
            }
        }
    }
    true
}

/// Extracts the return type from a JavaScript function signature.
/// # Arguments
/// * `_signature` - A string slice containing a JavaScript function signature
/// # Returns
/// Always returns `None`, as JavaScript function signatures do not contain type annotations. Return type information should be extracted from JSDoc comments instead, which are handled separately during natural language generation.
fn extract_return_javascript(_signature: &str) -> Option<String> {
    // JavaScript doesn't have type annotations in signatures.
    // JSDoc parsing is handled separately in NL generation.
    None
}

static LANG_JAVASCRIPT: LanguageDef = LanguageDef {
    name: "javascript",
    grammar: Some(|| tree_sitter_javascript::LANGUAGE.into()),
    extensions: &["js", "jsx", "mjs", "cjs"],
    chunk_query: include_str!("queries/javascript.chunks.scm"),
    call_query: Some(include_str!("queries/javascript.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["class_body", "class_declaration"],
    stopwords: &[
        "function",
        "const",
        "let",
        "var",
        "return",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "new",
        "this",
        "class",
        "extends",
        "import",
        "export",
        "from",
        "default",
        "try",
        "catch",
        "finally",
        "throw",
        "async",
        "await",
        "true",
        "false",
        "null",
        "undefined",
        "typeof",
        "instanceof",
        "void",
    ],
    extract_return_nl: extract_return_javascript,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.test.js")),
    test_name_suggestion: Some(|name| format!("test('{}', ...)", name)),
    common_types: &[
        "Array", "Map", "Set", "Promise", "Date", "Error", "RegExp", "Function", "Object",
        "Symbol", "WeakMap", "WeakSet",
    ],
    container_body_kinds: &["class_body"],
    post_process_chunk: Some(post_process_javascript_javascript as PostProcessChunkFn),
    test_markers: &["describe(", "it(", "test("],
    test_path_patterns: &["%.test.%", "%.spec.%", "%/tests/%"],
    entry_point_names: &[
        "handler",
        "middleware",
        "beforeEach",
        "afterEach",
        "beforeAll",
        "afterAll",
    ],
    trait_method_names: &["toString", "valueOf", "toJSON"],
    doc_format: "javadoc",
    doc_convention: "Use JSDoc format: @param {type} name, @returns {type}, @throws {type}.",
    field_style: FieldStyle::NameFirst {
        separators: ":=;",
        strip_prefixes: "public private protected readonly static",
    },
    skip_line_prefixes: &["class ", "export "],
    ..DEFAULTS
};

pub fn definition_javascript() -> &'static LanguageDef {
    &LANG_JAVASCRIPT
}

// ============================================================================
// Json (json)
// ============================================================================

/// Extracts the return type from a function signature.
/// # Arguments
/// * `_signature` - A function signature string to parse
/// # Returns
/// Returns `None` if no return type is found or the signature format is not supported. This function currently always returns `None` as it's designed for formats like JSON that do not have function return types.
fn extract_return_json(_signature: &str) -> Option<String> {
    // JSON has no functions or return types
    None
}

/// Post-process JSON chunks: only keep top-level pairs.
/// A top-level pair's parent is an `object` whose parent is `document`.
fn post_process_json_json(
    _name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // pair > object > document
    if let Some(parent) = node.parent() {
        if parent.kind() == "object" {
            if let Some(grandparent) = parent.parent() {
                return grandparent.kind() == "document";
            }
        }
    }
    false
}

static LANG_JSON: LanguageDef = LanguageDef {
    name: "json",
    grammar: Some(|| tree_sitter_json::LANGUAGE.into()),
    extensions: &["json", "jsonc"],
    chunk_query: include_str!("queries/json.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &["true", "false", "null"],
    extract_return_nl: extract_return_json,
    post_process_chunk: Some(post_process_json_json),
    ..DEFAULTS
};

pub fn definition_json() -> &'static LanguageDef {
    &LANG_JSON
}

// ============================================================================
// Julia (julia)
// ============================================================================

/// Post-process Julia chunks to set correct chunk types.
fn post_process_julia_julia(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "function_definition" => *chunk_type = ChunkType::Function,
        "struct_definition" => *chunk_type = ChunkType::Struct,
        "abstract_definition" => *chunk_type = ChunkType::TypeAlias,
        "module_definition" => *chunk_type = ChunkType::Module,
        "macro_definition" => *chunk_type = ChunkType::Macro,
        _ => {}
    }
    true
}

/// Extract return type from Julia function signatures.
/// Julia signatures: `function add(x::Int, y::Int)::Int`
/// Return type is after `)::`
fn extract_return_julia(signature: &str) -> Option<String> {
    let trimmed = signature.trim();

    // function foo(x, y)::ReturnType
    let paren_pos = trimmed.rfind(')')?;
    let after = trimmed[paren_pos + 1..].trim();
    let ret = after.strip_prefix("::")?.trim();

    // Remove trailing 'where' clause
    let ret = ret.split_whitespace().next()?;

    if ret.is_empty() {
        return None;
    }

    // Skip Nothing (void equivalent)
    if ret == "Nothing" {
        return None;
    }

    let words = crate::nl::tokenize_identifier(ret).join(" ");
    Some(format!("Returns {}", words.to_lowercase()))
}

static LANG_JULIA: LanguageDef = LanguageDef {
    name: "julia",
    grammar: Some(|| tree_sitter_julia::LANGUAGE.into()),
    extensions: &["jl"],
    chunk_query: include_str!("queries/julia.chunks.scm"),
    call_query: Some(include_str!("queries/julia.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["line_comment", "block_comment"],
    stopwords: &[
        "function",
        "end",
        "module",
        "struct",
        "mutable",
        "abstract",
        "type",
        "macro",
        "begin",
        "let",
        "const",
        "if",
        "elseif",
        "else",
        "for",
        "while",
        "do",
        "try",
        "catch",
        "finally",
        "return",
        "break",
        "continue",
        "import",
        "using",
        "export",
        "true",
        "false",
        "nothing",
        "where",
        "in",
        "isa",
        "typeof",
        "Int",
        "Int64",
        "Float64",
        "String",
        "Bool",
        "Char",
        "Vector",
        "Array",
        "Dict",
        "Set",
        "Tuple",
        "Nothing",
        "Any",
        "Union",
        "AbstractFloat",
        "AbstractString",
        "println",
        "print",
        "push!",
        "pop!",
        "length",
        "size",
        "map",
        "filter",
    ],
    extract_return_nl: extract_return_julia,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_test.jl")),
    common_types: &[
        "Int", "Int64", "Float64", "String", "Bool", "Char", "Vector", "Array", "Dict", "Set",
        "Tuple", "Nothing", "Any",
    ],
    post_process_chunk: Some(post_process_julia_julia as PostProcessChunkFn),
    test_markers: &["@test", "@testset"],
    test_path_patterns: &["%/test/%", "%_test.jl"],
    entry_point_names: &["main"],
    trait_method_names: &[
        "show",
        "convert",
        "promote_rule",
        "iterate",
        "length",
        "getindex",
        "setindex!",
    ],
    doc_convention: "Use triple-quoted docstrings with # Arguments, # Returns sections.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["struct ", "mutable struct"],
    ..DEFAULTS
};

pub fn definition_julia() -> &'static LanguageDef {
    &LANG_JULIA
}

// ============================================================================
// Kotlin (kotlin)
// ============================================================================

/// Post-process Kotlin chunks to reclassify `class_declaration` nodes.
/// The kotlin-ng grammar uses `class_declaration` for both classes and interfaces.
/// This hook checks:
/// 1. If an anonymous "interface" keyword child exists -> Interface
/// 2. If `modifiers` contains a `class_modifier` with text "enum" -> Enum
fn post_process_kotlin_kotlin(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    // Reclassify secondary_constructor and anonymous_initializer (init blocks)
    match node.kind() {
        "secondary_constructor" => {
            *chunk_type = ChunkType::Constructor;
            *name = "constructor".to_string();
            return true;
        }
        "anonymous_initializer" => {
            *chunk_type = ChunkType::Constructor;
            *name = "init".to_string();
            return true;
        }
        _ => {}
    }

    // Only reclassify class_declarations below
    if node.kind() != "class_declaration" {
        return true;
    }

    let mut has_enum_modifier = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "modifiers" => {
                let mut mod_cursor = child.walk();
                for modifier in child.children(&mut mod_cursor) {
                    if modifier.kind() == "class_modifier" {
                        let text = &source[modifier.byte_range()];
                        if text == "enum" {
                            has_enum_modifier = true;
                        }
                    }
                }
            }
            "interface" => {
                *chunk_type = ChunkType::Interface;
                return true;
            }
            _ => {}
        }
    }

    if has_enum_modifier {
        *chunk_type = ChunkType::Enum;
    }
    // else: stays as Class
    true
}

/// Extracts the return type from a Kotlin function signature and formats it as a documentation string.
/// # Arguments
/// * `signature` - A Kotlin function signature string to parse for return type information
/// # Returns
/// Returns `Some(String)` containing a formatted return type description (e.g., "Returns SomeType") if a non-Unit return type is found after the closing parenthesis and colon. Returns `None` if no closing parenthesis exists, no colon is present, the return type is empty, or the return type is "Unit".
fn extract_return_kotlin(signature: &str) -> Option<String> {
    // Kotlin: fun name(params): ReturnType { ... }
    // Look for `: ReturnType` after last `)` and before `{` or `=`
    let paren_pos = signature.rfind(')')?;
    let after_paren = &signature[paren_pos + 1..];

    // Find the terminator ({ or =)
    let end_pos = after_paren
        .find('{')
        .or_else(|| after_paren.find('='))
        .unwrap_or(after_paren.len());
    let between = &after_paren[..end_pos];

    // Look for colon
    let colon_pos = between.find(':')?;
    let ret_type = between[colon_pos + 1..].trim();
    if ret_type.is_empty() || ret_type == "Unit" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

static LANG_KOTLIN: LanguageDef = LanguageDef {
    name: "kotlin",
    grammar: Some(|| tree_sitter_kotlin::LANGUAGE.into()),
    extensions: &["kt", "kts"],
    chunk_query: include_str!("queries/kotlin.chunks.scm"),
    call_query: Some(include_str!("queries/kotlin.calls.scm")),
    doc_nodes: &["line_comment", "multiline_comment"],
    method_containers: &["class_body"],
    stopwords: &[
        "fun",
        "val",
        "var",
        "class",
        "interface",
        "object",
        "companion",
        "data",
        "sealed",
        "enum",
        "abstract",
        "open",
        "override",
        "private",
        "protected",
        "public",
        "internal",
        "return",
        "if",
        "else",
        "when",
        "for",
        "while",
        "do",
        "break",
        "continue",
        "this",
        "super",
        "import",
        "package",
        "is",
        "as",
        "in",
        "null",
        "true",
        "false",
        "typealias",
        "const",
        "lateinit",
        "suspend",
        "inline",
        "reified",
    ],
    extract_return_nl: extract_return_kotlin,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.kt")),
    test_name_suggestion: Some(|name| super::pascal_test_name("test", name)),
    type_query: Some(include_str!("queries/kotlin.types.scm")),
    common_types: &[
        "String",
        "Int",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "Byte",
        "Short",
        "Char",
        "Unit",
        "Nothing",
        "Any",
        "List",
        "ArrayList",
        "Map",
        "HashMap",
        "Set",
        "HashSet",
        "Collection",
        "MutableList",
        "MutableMap",
        "MutableSet",
        "Sequence",
        "Array",
        "Pair",
        "Triple",
        "Comparable",
        "Iterable",
    ],
    container_body_kinds: &["class_body"],
    post_process_chunk: Some(post_process_kotlin_kotlin),
    test_markers: &["@Test", "@ParameterizedTest"],
    test_path_patterns: &["%/test/%", "%/tests/%", "%Test.kt"],
    entry_point_names: &["main"],
    trait_method_names: &["equals", "hashCode", "toString", "compareTo", "iterator"],
    doc_format: "javadoc",
    doc_convention: "Use KDoc format: @param, @return, @throws tags.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "val var private protected public internal override lateinit",
    },
    skip_line_prefixes: &[
        "class ",
        "data class",
        "sealed class",
        "enum class",
        "interface ",
    ],
    ..DEFAULTS
};

pub fn definition_kotlin() -> &'static LanguageDef {
    &LANG_KOTLIN
}

// ============================================================================
// Latex (latex)
// ============================================================================

/// Map minted/lstlisting language names to cqs language identifiers.
/// Returns `None` if the language name maps to the default target,
/// `Some("_skip")` if unrecognized, or `Some(lang)` for a specific language.
fn map_code_language_latex(lang: &str) -> Option<&'static str> {
    match lang.to_lowercase().as_str() {
        "python" | "python3" | "py" => Some("python"),
        "rust" => Some("rust"),
        "c" => Some("c"),
        "cpp" | "c++" => Some("cpp"),
        "java" => Some("java"),
        "javascript" | "js" => Some("javascript"),
        "typescript" | "ts" => Some("typescript"),
        "go" | "golang" => Some("go"),
        "bash" | "sh" | "shell" => Some("bash"),
        "ruby" | "rb" => Some("ruby"),
        "sql" => Some("sql"),
        "haskell" | "hs" => Some("haskell"),
        "lua" => Some("lua"),
        "scala" => Some("scala"),
        "r" => Some("r"),
        _ => {
            tracing::debug!(
                language = lang,
                "Unrecognized code listing language, skipping"
            );
            Some("_skip")
        }
    }
}

/// Detect code language from a `minted_environment` node.
/// Checks the `begin` child's `language` field (`\begin{minted}{python}`).
fn detect_minted_language_latex(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "begin" {
            // Look for the language field (curly_group_text)
            let mut begin_cursor = child.walk();
            let mut found_name = false;
            for begin_child in child.children(&mut begin_cursor) {
                if begin_child.kind() == "curly_group_text" {
                    if !found_name {
                        // First curly_group_text is the environment name (minted)
                        found_name = true;
                        continue;
                    }
                    // Second curly_group_text is the language
                    let text = source[begin_child.byte_range()].trim();
                    // Strip braces: {python} → python
                    let lang = text
                        .strip_prefix('{')
                        .and_then(|s| s.strip_suffix('}'))
                        .unwrap_or(text)
                        .trim();
                    if !lang.is_empty() {
                        tracing::debug!(language = lang, "Minted environment language detected");
                        return map_code_language_latex(lang);
                    }
                }
            }
        }
    }
    Some("_skip")
}

/// Detect code language from a `listing_environment` node.
/// The LaTeX grammar includes `[language=X]` options in the `source_code`
/// content (not as a parsed `begin` attribute). This function checks the
/// `source_code` content prefix for `[language=X]`.
fn detect_listing_language_latex(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "source_code" {
            let text = &source[child.byte_range()];
            let trimmed = text.trim_start();
            // Check for [language=X] prefix
            if trimmed.starts_with('[') {
                let text_lower = trimmed.to_ascii_lowercase();
                if let Some(pos) = text_lower.find("language=") {
                    let after = &trimmed[pos + 9..];
                    let lang: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '+')
                        .collect();
                    if !lang.is_empty() {
                        tracing::debug!(
                            language = %lang,
                            "Listing environment language detected"
                        );
                        return map_code_language_latex(&lang);
                    }
                }
            }
        }
    }
    // No language option found — skip (don't guess)
    Some("_skip")
}

/// Post-process LaTeX chunks: clean up names by stripping braces and backslashes.
fn post_process_latex_latex(
    name: &mut String,
    _chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Strip surrounding braces from curly_group captures: {Title} → Title
    if name.starts_with('{') && name.ends_with('}') {
        *name = name[1..name.len() - 1].trim().to_string();
    }
    // Strip leading backslash from command names: \mycommand → mycommand
    if name.starts_with('\\') {
        *name = name[1..].to_string();
    }
    // Skip empty names
    !name.is_empty()
}

static LANG_LATEX: LanguageDef = LanguageDef {
    name: "latex",
    grammar: Some(|| tree_sitter_latex::LANGUAGE.into()),
    extensions: &["tex", "sty", "cls"],
    chunk_query: include_str!("queries/latex.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "begin",
        "end",
        "documentclass",
        "usepackage",
        "input",
        "include",
        "label",
        "ref",
        "cite",
        "bibliography",
        "maketitle",
        "tableofcontents",
        "textbf",
        "textit",
        "emph",
        "item",
        "hline",
        "vspace",
        "hspace",
        "newline",
        "newpage",
        "par",
    ],
    post_process_chunk: Some(post_process_latex_latex),
    injections: &[
        // \begin{minted}{python} ... \end{minted} — language from argument
        InjectionRule {
            container_kind: "minted_environment",
            content_kind: "source_code",
            target_language: "python", // default, overridden by detect_minted_language
            detect_language: Some(detect_minted_language_latex),
            content_scoped_lines: false,
        },
        // \begin{lstlisting}[language=Python] ... \end{lstlisting}
        InjectionRule {
            container_kind: "listing_environment",
            content_kind: "source_code",
            target_language: "c", // default, overridden by detect_listing_language
            detect_language: Some(detect_listing_language_latex),
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_latex() -> &'static LanguageDef {
    &LANG_LATEX
}

// ============================================================================
// Lua (lua)
// ============================================================================

/// Returns true if the name follows UPPER_CASE convention (all ASCII uppercase/digits/underscores,
/// at least one letter, e.g. MAX_RETRIES, API_URL_V2).
fn is_upper_snake_case_lua(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.bytes().any(|b| b.is_ascii_uppercase())
}

/// Returns true if the node is nested inside a function body.
fn is_inside_function_lua(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        match parent.kind() {
            "function_declaration" | "function_definition" => return true,
            _ => {}
        }
        cursor = parent.parent();
    }
    false
}

/// Post-process Lua chunks: only keep `@const` captures whose name is UPPER_CASE
/// and that are at module level (not inside function bodies). Also skip assignments
/// whose RHS is a function_definition (already captured as Function), and deduplicate
/// assignment_statement nodes that are already captured via their parent variable_declaration.
#[allow(clippy::ptr_arg)] // signature must match PostProcessChunkFn type alias
fn post_process_lua_lua(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        // Deduplicate: if this assignment_statement is inside a variable_declaration,
        // skip it — the variable_declaration match already captures the same constant.
        if node.kind() == "assignment_statement" {
            if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declaration" {
                    return false;
                }
            }
        }
        // Skip constants inside function bodies — only capture module-level
        if is_inside_function_lua(node) {
            return false;
        }
        // Skip if RHS is a function_definition (already captured as Function)
        if has_function_value_lua(node) {
            return false;
        }
        return is_upper_snake_case_lua(name);
    }
    true
}

/// Check if any value in the assignment is a function_definition.
fn has_function_value_lua(node: tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "expression_list" || child.kind() == "assignment_statement" {
            // Recurse into expression_list or nested assignment_statement
            if has_function_value_lua(child) {
                return true;
            }
        }
        if child.kind() == "function_definition" {
            return true;
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    false
}

/// Extracts the return type from a function signature.
/// # Arguments
/// * `_signature` - A function signature string to parse
/// # Returns
/// Returns `None` as Lua does not support type annotations in function signatures, so return types cannot be extracted from the signature itself.
fn extract_return_lua(_signature: &str) -> Option<String> {
    // Lua has no type annotations in signatures
    None
}

static LANG_LUA: LanguageDef = LanguageDef {
    name: "lua",
    grammar: Some(|| tree_sitter_lua::LANGUAGE.into()),
    extensions: &["lua"],
    chunk_query: include_str!("queries/lua.chunks.scm"),
    call_query: Some(include_str!("queries/lua.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        "function",
        "end",
        "local",
        "return",
        "if",
        "then",
        "else",
        "elseif",
        "for",
        "do",
        "while",
        "repeat",
        "until",
        "break",
        "in",
        "and",
        "or",
        "not",
        "nil",
        "true",
        "false",
        "self",
        "require",
        "module",
        "print",
        "pairs",
        "ipairs",
        "table",
        "string",
        "math",
        "io",
        "os",
        "type",
        "tostring",
        "tonumber",
        "error",
        "pcall",
        "xpcall",
        "setmetatable",
        "getmetatable",
    ],
    extract_return_nl: extract_return_lua,
    post_process_chunk: Some(post_process_lua_lua as PostProcessChunkFn),
    test_path_patterns: &["%/tests/%", "%/test/%", "%_test.lua", "%_spec.lua"],
    doc_format: "lua_ldoc",
    doc_convention: "Use LDoc format: @param, @return tags.",
    field_style: FieldStyle::NameFirst {
        separators: "=",
        strip_prefixes: "local",
    },
    ..DEFAULTS
};

pub fn definition_lua() -> &'static LanguageDef {
    &LANG_LUA
}

// ============================================================================
// Make (make)
// ============================================================================

static LANG_MAKE: LanguageDef = LanguageDef {
    name: "make",
    grammar: Some(|| tree_sitter_make::LANGUAGE.into()),
    extensions: &["mk", "mak"],
    chunk_query: include_str!("queries/make.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "all",
        "clean",
        "install",
        "uninstall",
        "dist",
        "distclean",
        "check",
        "test",
        "phony",
        "default",
        "ifdef",
        "ifndef",
        "ifeq",
        "ifneq",
        "else",
        "endif",
        "include",
        "override",
        "export",
        "unexport",
        "define",
        "endef",
        "wildcard",
        "patsubst",
        "subst",
        "filter",
        "sort",
        "word",
        "words",
        "foreach",
        "call",
        "eval",
        "origin",
        "shell",
        "info",
        "warning",
        "error",
    ],
    entry_point_names: &["all", "default"],
    injections: &[InjectionRule {
        container_kind: "recipe",
        content_kind: "shell_text",
        target_language: "bash",
        detect_language: None,
        content_scoped_lines: false,
    }],
    ..DEFAULTS
};

pub fn definition_make() -> &'static LanguageDef {
    &LANG_MAKE
}

// ============================================================================
// Markdown (markdown)
// ============================================================================

static LANG_MARKDOWN: LanguageDef = LanguageDef {
    name: "markdown",
    grammar: None, // No tree-sitter — custom line-by-line heading parser
    extensions: &["md", "mdx"],
    signature_style: SignatureStyle::Breadcrumb,
    stopwords: &[
        "the",
        "and",
        "for",
        "with",
        "that",
        "this",
        "from",
        "are",
        "was",
        "will",
        "can",
        "has",
        "have",
        "been",
        "being",
        "also",
        "such",
        "each",
        "when",
        "which",
        "would",
        "about",
        "into",
        "over",
        "after",
        "before",
        "more",
        "than",
        "then",
        "only",
        "very",
        "just",
        "may",
        "must",
        "should",
        "could",
        "does",
        "did",
        "had",
        "not",
        "but",
        "all",
        "any",
        "both",
        "its",
        "our",
        "their",
        "there",
        "here",
        "where",
        "what",
        "how",
        "who",
        "see",
        "use",
        "used",
        "using",
        "following",
        "example",
        "note",
        "important",
        "below",
        "above",
        "refer",
        "section",
        "page",
        "chapter",
        "figure",
        "table",
    ],
    ..DEFAULTS
};

pub fn definition_markdown() -> &'static LanguageDef {
    &LANG_MARKDOWN
}

// ============================================================================
// Nix (nix)
// ============================================================================

/// Nix binding names that contain shell scripts.
/// In Nix derivations, these attribute bindings hold shell code:
/// build phases, hooks, and script fields. We only inject bash for
/// indented strings in these contexts to avoid false positives.
const SHELL_CONTEXTS_NIX: &[&str] = &[
    "buildPhase",
    "installPhase",
    "configurePhase",
    "checkPhase",
    "unpackPhase",
    "patchPhase",
    "fixupPhase",
    "distPhase",
    "shellHook",
    "preBuild",
    "postBuild",
    "preInstall",
    "postInstall",
    "preCheck",
    "postCheck",
    "preConfigure",
    "postConfigure",
    "preUnpack",
    "postUnpack",
    "prePatch",
    "postPatch",
    "preFixup",
    "postFixup",
    "script",
    "buildCommand",
    "installCommand",
];

/// Detect whether an `indented_string_expression` contains shell code.
/// Walks up from the container node to find the parent `binding` and
/// checks the attribute name against known shell contexts (build phases,
/// hooks, etc.). Returns `None` (use default bash) for shell contexts,
/// `Some("_skip")` for everything else.
fn detect_nix_shell_context_nix(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    // Walk up to find the binding parent
    let parent = match node.parent() {
        Some(p) if p.kind() == "binding" => p,
        _ => {
            tracing::debug!("Nix indented string not in binding context, skipping injection");
            return Some("_skip");
        }
    };

    // Find attrpath child of binding → get last identifier
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if child.kind() == "attrpath" {
            let mut inner_cursor = child.walk();
            let mut last_ident = None;
            for attr_child in child.children(&mut inner_cursor) {
                if attr_child.kind() == "identifier" {
                    last_ident = Some(&source[attr_child.byte_range()]);
                }
            }
            if let Some(ident) = last_ident {
                if SHELL_CONTEXTS_NIX.contains(&ident) {
                    tracing::debug!(
                        binding = ident,
                        "Nix shell context detected, injecting bash"
                    );
                    return None; // Use default target (bash)
                }
                tracing::debug!(binding = ident, "Nix binding not a shell context, skipping");
                return Some("_skip");
            }
        }
    }

    Some("_skip")
}

static LANG_NIX: LanguageDef = LanguageDef {
    name: "nix",
    grammar: Some(|| tree_sitter_nix::LANGUAGE.into()),
    extensions: &["nix"],
    chunk_query: include_str!("queries/nix.chunks.scm"),
    call_query: Some(include_str!("queries/nix.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        "true", "false", "null", "if", "then", "else", "let", "in", "with", "rec", "inherit",
        "import", "assert", "builtins", "throw", "abort",
    ],
    injections: &[
        // Indented strings (''...'') in shell-context bindings contain bash.
        // detect_nix_shell_context checks the parent binding's attrpath name
        // against known shell contexts (buildPhase, installPhase, etc.).
        InjectionRule {
            container_kind: "indented_string_expression",
            content_kind: "string_fragment",
            target_language: "bash",
            detect_language: Some(detect_nix_shell_context_nix),
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_nix() -> &'static LanguageDef {
    &LANG_NIX
}

// ============================================================================
// Objc (objc)
// ============================================================================

/// Extracts the return type from a function signature.
/// Currently returns `None` for all inputs as Objective-C method signatures use `- (ReturnType)methodName` syntax that is not amenable to simple text-based extraction.
/// # Arguments
/// * `_signature` - A function signature string to parse
/// # Returns
/// `None` in all cases, as return type extraction is not yet implemented.
fn extract_return_objc(_signature: &str) -> Option<String> {
    // ObjC methods use `- (ReturnType)methodName` syntax which doesn't lend itself
    // to simple text-based extraction. Return None.
    None
}

/// Post-process Objective-C chunks to reclassify categories as Extension.
/// ObjC categories (`@interface Type (Category)` / `@implementation Type (Category)`)
/// use the same `class_interface` / `class_implementation` nodes as regular classes,
/// but have a `category` field. When present, reclassify as Extension.
fn post_process_objc_objc(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    match node.kind() {
        "class_interface" | "class_implementation" => {
            if node.child_by_field_name("category").is_some() {
                *chunk_type = ChunkType::Extension;
                tracing::debug!("Reclassified {} as Extension (has category)", node.kind());
            }
        }
        _ => {}
    }
    true
}

static LANG_OBJC: LanguageDef = LanguageDef {
    name: "objc",
    grammar: Some(|| tree_sitter_objc::LANGUAGE.into()),
    extensions: &["m", "mm"],
    chunk_query: include_str!("queries/objc.chunks.scm"),
    call_query: Some(include_str!("queries/objc.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &[
        "class_interface",
        "implementation_definition",
        "protocol_declaration",
    ],
    stopwords: &[
        "self",
        "super",
        "nil",
        "NULL",
        "YES",
        "NO",
        "true",
        "false",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "void",
        "int",
        "float",
        "double",
        "char",
        "long",
        "short",
        "unsigned",
        "signed",
        "static",
        "extern",
        "const",
        "typedef",
        "struct",
        "enum",
        "union",
        "id",
        "Class",
        "SEL",
        "IMP",
        "BOOL",
        "NSObject",
        "NSString",
        "NSInteger",
        "NSUInteger",
        "CGFloat",
        "nonatomic",
        "strong",
        "weak",
        "copy",
        "assign",
        "readonly",
        "readwrite",
        "atomic",
        "property",
        "synthesize",
        "dynamic",
        "interface",
        "implementation",
        "protocol",
        "end",
        "optional",
        "required",
        "import",
        "include",
    ],
    extract_return_nl: extract_return_objc,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.m")),
    container_body_kinds: &["implementation_definition"],
    post_process_chunk: Some(post_process_objc_objc),
    test_markers: &["- (void)test"],
    test_path_patterns: &["%/Tests/%", "%Tests.m"],
    entry_point_names: &["main"],
    trait_method_names: &[
        "init",
        "dealloc",
        "description",
        "hash",
        "isEqual",
        "copyWithZone",
        "encodeWithCoder",
        "initWithCoder",
    ],
    doc_format: "javadoc",
    doc_convention: "Use Doxygen format: @param, @return, @throws tags.",
    skip_line_prefixes: &["@interface", "@implementation", "@protocol"],
    ..DEFAULTS
};

pub fn definition_objc() -> &'static LanguageDef {
    &LANG_OBJC
}

// ============================================================================
// Ocaml (ocaml)
// ============================================================================

/// Post-process OCaml chunks to set correct chunk types.
fn post_process_ocaml_ocaml(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "value_definition" => *chunk_type = ChunkType::Function,
        "type_definition" => {
            // Classify based on type body content
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            // Variant types use | for constructors (not inside strings/comments)
            // Check for pattern: = Constructor | Constructor or = | Constructor
            if let Some(eq_pos) = text.find('=') {
                let after_eq = &text[eq_pos + 1..];
                if after_eq.contains('|') {
                    *chunk_type = ChunkType::Enum;
                } else if after_eq.contains('{') {
                    *chunk_type = ChunkType::Struct;
                } else {
                    *chunk_type = ChunkType::TypeAlias;
                }
            } else {
                *chunk_type = ChunkType::TypeAlias;
            }
        }
        "module_definition" => *chunk_type = ChunkType::Module,
        _ => {}
    }
    true
}

/// Extract return type from OCaml type signatures.
/// Handles val specifications: `val add : int -> int -> int`
/// Return type is the last type after the final `->`.
fn extract_return_ocaml(signature: &str) -> Option<String> {
    let trimmed = signature.trim();

    // val specification: val name : t1 -> t2 -> return_type
    if trimmed.starts_with("val ") {
        let type_part = trimmed.split_once(':')?.1.trim();
        let ret = if type_part.contains("->") {
            type_part.rsplit("->").next()?.trim()
        } else {
            type_part
        };
        if ret.is_empty() {
            return None;
        }
        let words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", words.to_lowercase()));
    }

    None
}

static LANG_OCAML: LanguageDef = LanguageDef {
    name: "ocaml",
    grammar: Some(|| tree_sitter_ocaml::LANGUAGE_OCAML.into()),
    extensions: &["ml", "mli"],
    chunk_query: include_str!("queries/ocaml.chunks.scm"),
    call_query: Some(include_str!("queries/ocaml.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &[
        "let",
        "in",
        "val",
        "type",
        "module",
        "struct",
        "sig",
        "end",
        "fun",
        "function",
        "match",
        "with",
        "when",
        "if",
        "then",
        "else",
        "begin",
        "do",
        "done",
        "for",
        "to",
        "downto",
        "while",
        "open",
        "include",
        "rec",
        "and",
        "of",
        "mutable",
        "ref",
        "try",
        "raise",
        "exception",
        "external",
        "true",
        "false",
        "unit",
        "int",
        "float",
        "string",
        "bool",
        "char",
        "list",
        "option",
        "array",
        "Some",
        "None",
        "Ok",
        "Error",
        "failwith",
        "Printf",
        "Scanf",
        "List",
        "Array",
        "Map",
        "Set",
        "Hashtbl",
        "Buffer",
        "String",
    ],
    extract_return_nl: extract_return_ocaml,
    test_file_suggestion: Some(|stem, _parent| format!("test/test_{stem}.ml")),
    common_types: &[
        "int", "float", "string", "bool", "char", "unit", "list", "option", "array", "ref",
    ],
    container_body_kinds: &["structure"],
    post_process_chunk: Some(post_process_ocaml_ocaml as PostProcessChunkFn),
    test_markers: &["let%test", "let%expect_test", "let test_"],
    test_path_patterns: &["%/test/%", "%_test.ml"],
    entry_point_names: &["main"],
    trait_method_names: &[
        "compare",
        "equal",
        "hash",
        "pp",
        "show",
        "to_string",
        "of_string",
    ],
    doc_format: "ocaml_doc",
    doc_convention: "Use OCamldoc format with (** *) comments.",
    skip_line_prefixes: &["type "],
    ..DEFAULTS
};

pub fn definition_ocaml() -> &'static LanguageDef {
    &LANG_OCAML
}

// ============================================================================
// Perl (perl)
// ============================================================================

/// Post-process Perl chunks to set correct chunk types.
fn post_process_perl_perl(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "function_definition" => *chunk_type = ChunkType::Function,
        "package_statement" => {
            *chunk_type = ChunkType::Module;
            // Extract package name from text: "package Foo::Bar;"
            let text = node.utf8_text(source.as_bytes()).unwrap_or("");
            let text = text.trim();
            if let Some(rest) = text.strip_prefix("package") {
                let rest = rest.trim();
                // Take until ; or { or whitespace
                let pkg_name: String = rest
                    .chars()
                    .take_while(|c| *c != ';' && *c != '{' && !c.is_whitespace())
                    .collect();
                if !pkg_name.is_empty() {
                    *name = pkg_name;
                }
            }
        }
        _ => {}
    }
    true
}

/// Extract return type from Perl signatures.
/// Perl doesn't have static return types, so this always returns None.
fn extract_return_perl(_signature: &str) -> Option<String> {
    None
}

static LANG_PERL: LanguageDef = LanguageDef {
    name: "perl",
    grammar: Some(|| tree_sitter_perl::LANGUAGE.into()),
    extensions: &["pl", "pm"],
    chunk_query: include_str!("queries/perl.chunks.scm"),
    call_query: Some(include_str!("queries/perl.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comments", "pod"],
    stopwords: &[
        "sub", "my", "our", "local", "use", "require", "package", "return", "if", "elsif", "else",
        "unless", "while", "until", "for", "foreach", "do", "eval", "die", "warn", "print", "say",
        "chomp", "chop", "push", "pop", "shift", "unshift", "splice", "join", "split", "map",
        "grep", "sort", "keys", "values", "each", "exists", "delete", "defined", "ref", "bless",
        "new", "BEGIN", "END", "AUTOLOAD", "DESTROY", "open", "close", "read", "write", "seek",
        "tell", "Carp", "Exporter", "Scalar", "List", "File", "IO", "POSIX", "Data", "Dumper",
        "strict", "warnings", "utf8", "Encode", "Getopt", "Test", "More",
    ],
    extract_return_nl: extract_return_perl,
    test_file_suggestion: Some(|stem, _parent| format!("t/{stem}.t")),
    post_process_chunk: Some(post_process_perl_perl as PostProcessChunkFn),
    test_path_patterns: &["%/t/%", "%.t"],
    entry_point_names: &["main"],
    trait_method_names: &["new", "AUTOLOAD", "DESTROY", "import", "BEGIN", "END"],
    doc_format: "hash_comment",
    doc_convention: "Use POD format for documentation sections.",
    field_style: FieldStyle::NameFirst {
        separators: "=",
        strip_prefixes: "my our local",
    },
    ..DEFAULTS
};

pub fn definition_perl() -> &'static LanguageDef {
    &LANG_PERL
}

// ============================================================================
// Php (php)
// ============================================================================

/// Strip `$` prefix from PHP property names.
/// PHP properties are declared as `$name`, but callers reference them without `$`.
/// This hook strips the prefix so property names match call sites.
fn post_process_php_php(
    name: &mut String,
    chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if let Some(stripped) = name.strip_prefix('$') {
        *name = stripped.to_string();
    }
    // PHP __construct is a constructor
    if *chunk_type == ChunkType::Method && name == "__construct" {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

/// Extracts and formats the return type from a PHP function signature.
/// Parses a PHP function signature to find the return type annotation (the type following `:` after the parameter list). Filters out void and mixed types, strips nullable prefixes, and returns a formatted description string.
/// # Arguments
/// * `signature` - A PHP function signature string, expected to contain parameter list and optional return type annotation
/// # Returns
/// Returns `Some(String)` containing a formatted return type description (e.g., "Returns string") if a valid, non-void return type is found. Returns `None` if no return type annotation exists, the type is void/mixed, the colon appears after the opening brace, or the signature is malformed.
fn extract_return_php(signature: &str) -> Option<String> {
    // PHP: function name(params): ReturnType { ... }
    // Look for ): ReturnType after last )
    let paren_pos = signature.rfind(')')?;
    let after_paren = &signature[paren_pos + 1..];
    let colon_pos = after_paren.find(':')?;
    let end_pos = after_paren.find('{').unwrap_or(after_paren.len());
    // Colon must come before brace
    if colon_pos + 1 >= end_pos {
        return None;
    }
    let ret_type = after_paren[colon_pos + 1..end_pos].trim();
    if ret_type.is_empty() || ret_type == "void" || ret_type == "mixed" {
        return None;
    }
    // Strip nullable prefix
    let ret_type = ret_type.strip_prefix('?').unwrap_or(ret_type);
    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

static LANG_PHP: LanguageDef = LanguageDef {
    name: "php",
    grammar: Some(|| tree_sitter_php::LANGUAGE_PHP.into()),
    extensions: &["php"],
    chunk_query: include_str!("queries/php.chunks.scm"),
    call_query: Some(include_str!("queries/php.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["declaration_list"],
    stopwords: &[
        "function",
        "class",
        "interface",
        "trait",
        "enum",
        "namespace",
        "use",
        "extends",
        "implements",
        "abstract",
        "final",
        "static",
        "public",
        "protected",
        "private",
        "return",
        "if",
        "else",
        "elseif",
        "for",
        "foreach",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "new",
        "try",
        "catch",
        "finally",
        "throw",
        "echo",
        "print",
        "var",
        "const",
        "true",
        "false",
        "null",
        "self",
        "parent",
        "this",
        "array",
        "string",
        "int",
        "float",
        "bool",
        "void",
        "mixed",
        "never",
        "callable",
        "iterable",
        "object",
        "isset",
        "unset",
        "empty",
    ],
    extract_return_nl: extract_return_php,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.php")),
    type_query: Some(include_str!("queries/php.types.scm")),
    common_types: &[
        "string",
        "int",
        "float",
        "bool",
        "array",
        "object",
        "callable",
        "iterable",
        "void",
        "null",
        "mixed",
        "never",
        "self",
        "parent",
        "static",
        "false",
        "true",
        "Closure",
        "Iterator",
        "Generator",
        "Traversable",
        "Countable",
        "Throwable",
        "Exception",
        "RuntimeException",
        "InvalidArgumentException",
        "stdClass",
    ],
    container_body_kinds: &["declaration_list"],
    post_process_chunk: Some(post_process_php_php),
    test_markers: &["@test", "function test"],
    test_path_patterns: &["%/tests/%", "%/Tests/%", "%Test.php"],
    trait_method_names: &[
        "__construct",
        "__destruct",
        "__toString",
        "__get",
        "__set",
        "__call",
        "__isset",
        "__unset",
        "__sleep",
        "__wakeup",
        "__clone",
        "__invoke",
    ],
    injections: &[
        // PHP files contain HTML in `text` nodes. Two patterns exist:
        //
        // 1. Leading HTML before first `<?php`: `program` → `text` (direct child)
        // 2. HTML after `?>` tags: `program` → `text_interpolation` → `text`
        //
        // `content_scoped_lines: true` ensures only chunks within each `text`
        // region are replaced, preserving PHP chunks on adjacent lines.
        // HTML's own injection rules then extract JS/CSS recursively.
        InjectionRule {
            container_kind: "program",
            content_kind: "text",
            target_language: "html",
            detect_language: None,
            content_scoped_lines: true,
        },
        InjectionRule {
            container_kind: "text_interpolation",
            content_kind: "text",
            target_language: "html",
            detect_language: None,
            content_scoped_lines: true,
        },
    ],
    doc_format: "javadoc",
    doc_convention: "Use PHPDoc format: @param, @return, @throws tags.",
    field_style: FieldStyle::NameFirst {
        separators: "=;",
        strip_prefixes: "public private protected static var",
    },
    skip_line_prefixes: &["class ", "interface ", "trait ", "enum "],
    ..DEFAULTS
};

pub fn definition_php() -> &'static LanguageDef {
    &LANG_PHP
}

// ============================================================================
// Powershell (powershell)
// ============================================================================

/// Extracts the return type from a PowerShell function signature.
/// # Arguments
/// * `signature` - A PowerShell function signature string to parse
/// # Returns
/// Returns `None` because PowerShell function signatures do not include explicit return type annotations.
fn extract_return_powershell(_signature: &str) -> Option<String> {
    // PowerShell doesn't have return type syntax in function signatures
    None
}

/// Extract container type name for PowerShell classes.
/// `class_statement` stores the name in a `simple_name` child (no "name" field).
fn extract_container_name_ps_powershell(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "simple_name" {
            return Some(source[child.byte_range()].to_string());
        }
    }
    None
}

static LANG_POWERSHELL: LanguageDef = LanguageDef {
    name: "powershell",
    grammar: Some(|| tree_sitter_powershell::LANGUAGE.into()),
    extensions: &["ps1", "psm1"],
    chunk_query: include_str!("queries/powershell.chunks.scm"),
    call_query: Some(include_str!("queries/powershell.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["class_statement"],
    stopwords: &[
        "function",
        "param",
        "begin",
        "process",
        "end",
        "if",
        "else",
        "elseif",
        "switch",
        "for",
        "foreach",
        "while",
        "do",
        "until",
        "try",
        "catch",
        "finally",
        "throw",
        "return",
        "exit",
        "break",
        "continue",
        "class",
        "enum",
        "using",
        "namespace",
        "hidden",
        "static",
        "void",
        "new",
        "true",
        "false",
        "null",
    ],
    extract_return_nl: extract_return_powershell,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.Tests.ps1")),
    common_types: &[
        "string",
        "int",
        "bool",
        "object",
        "void",
        "double",
        "float",
        "long",
        "byte",
        "char",
        "decimal",
        "array",
        "hashtable",
        "PSObject",
        "PSCustomObject",
        "ScriptBlock",
        "DateTime",
        "TimeSpan",
        "Guid",
        "IPAddress",
        "SecureString",
        "PSCredential",
        "ErrorRecord",
    ],
    extract_container_name: Some(extract_container_name_ps_powershell),
    test_markers: &["Describe ", "It ", "Context "],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%.Tests.ps1"],
    doc_convention: "Use comment-based help: .SYNOPSIS, .PARAMETER, .OUTPUTS sections.",
    ..DEFAULTS
};

pub fn definition_powershell() -> &'static LanguageDef {
    &LANG_POWERSHELL
}

// ============================================================================
// Protobuf (protobuf)
// ============================================================================

/// Extract service name from a service node.
/// The proto grammar uses `service_name` children (not a `name` field),
/// so the default container name extractor won't work.
fn extract_container_name_protobuf(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "service_name" {
            return Some(source[child.byte_range()].to_string());
        }
    }
    None
}

static LANG_PROTOBUF: LanguageDef = LanguageDef {
    name: "protobuf",
    grammar: Some(|| tree_sitter_proto::LANGUAGE.into()),
    extensions: &["proto"],
    chunk_query: include_str!("queries/protobuf.chunks.scm"),
    call_query: Some(include_str!("queries/protobuf.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["service"],
    stopwords: &[
        "syntax", "package", "import", "option", "message", "service", "rpc", "enum", "oneof",
        "map", "repeated", "optional", "required", "reserved", "returns", "stream", "extend",
        "true", "false", "string", "bytes", "bool", "int32", "int64", "uint32", "uint64", "sint32",
        "sint64", "fixed32", "fixed64", "sfixed32", "sfixed64", "float", "double", "google",
    ],
    extract_container_name: Some(extract_container_name_protobuf),
    field_style: FieldStyle::NameFirst {
        separators: " ",
        strip_prefixes: "optional repeated required",
    },
    skip_line_prefixes: &["message ", "enum ", "service "],
    ..DEFAULTS
};

pub fn definition_protobuf() -> &'static LanguageDef {
    &LANG_PROTOBUF
}

// ============================================================================
// Python (python)
// ============================================================================

/// Returns true if the name follows UPPER_CASE convention (all ASCII uppercase/digits/underscores,
/// at least one letter, e.g. MAX_RETRIES, API_URL_V2).
fn is_upper_snake_case_python(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.bytes().any(|b| b.is_ascii_uppercase())
}

/// Returns true if the node is nested inside a function/class body.
fn is_inside_function_python(node: tree_sitter::Node) -> bool {
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
fn post_process_python_python(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        if is_inside_function_python(node) {
            return false;
        }
        return is_upper_snake_case_python(name);
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
fn extract_return_python(signature: &str) -> Option<String> {
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

static LANG_PYTHON: LanguageDef = LanguageDef {
    name: "python",
    grammar: Some(|| tree_sitter_python::LANGUAGE.into()),
    extensions: &["py", "pyi"],
    chunk_query: include_str!("queries/python.chunks.scm"),
    call_query: Some(include_str!("queries/python.calls.scm")),
    signature_style: SignatureStyle::UntilColon,
    doc_nodes: &["string", "comment"],
    method_containers: &["class_definition"],
    stopwords: &[
        "def", "class", "self", "return", "if", "elif", "else", "for", "while", "import", "from",
        "as", "with", "try", "except", "finally", "raise", "pass", "break", "continue", "and",
        "or", "not", "in", "is", "true", "false", "none", "lambda", "yield", "global", "nonlocal",
    ],
    extract_return_nl: extract_return_python,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/test_{stem}.py")),
    type_query: Some(include_str!("queries/python.types.scm")),
    common_types: &[
        "str",
        "int",
        "float",
        "bool",
        "list",
        "dict",
        "set",
        "tuple",
        "None",
        "Any",
        "Optional",
        "Union",
        "List",
        "Dict",
        "Set",
        "Tuple",
        "Type",
        "Callable",
        "Iterator",
        "Generator",
        "Coroutine",
        "Exception",
        "ValueError",
        "TypeError",
        "KeyError",
        "IndexError",
        "Path",
        "Self",
    ],
    post_process_chunk: Some(post_process_python_python as PostProcessChunkFn),
    test_markers: &["def test_", "pytest"],
    test_path_patterns: &["%/tests/%", "%\\_test.py", "%/test\\_%"],
    entry_point_names: &["__init__", "setup", "teardown"],
    trait_method_names: &[
        "__str__",
        "__repr__",
        "__eq__",
        "__ne__",
        "__lt__",
        "__le__",
        "__gt__",
        "__ge__",
        "__hash__",
        "__bool__",
        "__len__",
        "__iter__",
        "__next__",
        "__contains__",
        "__getitem__",
        "__setitem__",
        "__delitem__",
        "__call__",
        "__enter__",
        "__exit__",
        "__del__",
        "__new__",
        "__init_subclass__",
        "__class_getitem__",
    ],
    doc_format: "python_docstring",
    doc_convention: "Format as a Google-style docstring (Args/Returns/Raises sections).",
    field_style: FieldStyle::NameFirst {
        separators: ":=",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["class ", "@property", "def "],
    ..DEFAULTS
};

pub fn definition_python() -> &'static LanguageDef {
    &LANG_PYTHON
}

// ============================================================================
// R (r)
// ============================================================================

/// Returns true if the name follows UPPER_CASE convention (all ASCII uppercase/digits/underscores,
/// at least one letter, e.g. MAX_RETRIES, API_URL_V2).
fn is_upper_snake_case_r(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && name.bytes().any(|b| b.is_ascii_uppercase())
}

/// Extracts the return type from an R function signature.
/// Returns `None` — R functions do not have type annotations in their signatures.
fn extract_return_r(_signature: &str) -> Option<String> {
    None
}

/// Returns true if the node is nested inside a function body.
fn is_inside_function_r(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        if parent.kind() == "function_definition" {
            return true;
        }
        cursor = parent.parent();
    }
    false
}

/// Extract the first string argument from a `call` node's arguments.
/// For `setClass("Person", ...)` returns `Some("Person")`.
fn first_string_arg_r<'a>(node: tree_sitter::Node, source: &'a str) -> Option<&'a str> {
    let args = node.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        if child.kind() == "argument" {
            // Look for the string inside the argument
            let mut inner = child.walk();
            if inner.goto_first_child() {
                loop {
                    let ic = inner.node();
                    if ic.kind() == "string" {
                        // Extract string_content child
                        let mut sc = ic.walk();
                        if sc.goto_first_child() {
                            loop {
                                if sc.node().kind() == "string_content" {
                                    return Some(&source[sc.node().byte_range()]);
                                }
                                if !sc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                        return None;
                    }
                    if !inner.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    None
}

/// Check if a `call` node's function identifier matches the given name.
fn call_function_name_r<'a>(node: tree_sitter::Node, source: &'a str) -> Option<&'a str> {
    let func = node.child_by_field_name("function")?;
    if func.kind() == "identifier" {
        return Some(&source[func.byte_range()]);
    }
    // Also handle namespaced: R6::R6Class
    if func.kind() == "namespace_operator" {
        let mut cursor = func.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.is_named() && child.kind() == "identifier" {
                    // Take the last identifier (rhs of ::)
                    let text = &source[child.byte_range()];
                    // Keep going to find the rhs
                    if !cursor.goto_next_sibling() {
                        return Some(text);
                    }
                    // Skip :: operator
                    loop {
                        let next = cursor.node();
                        if next.is_named() && next.kind() == "identifier" {
                            return Some(&source[next.byte_range()]);
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                    return Some(text);
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    None
}

/// S4 class-defining functions whose first string argument is a class name.
const S4_CLASS_FUNCTIONS_R: &[&str] = &["setClass", "setRefClass"];

/// Post-process R chunks:
/// - `@class` (call nodes): keep only S4 class-defining calls, extract class name
/// - `@const` (binary_operator): detect R6Class → Class, else keep only UPPER_CASE constants
/// - `@function`: pass through unchanged
#[allow(clippy::ptr_arg)]
fn post_process_r_r(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match *chunk_type {
        ChunkType::Class => {
            // This is a top-level `call` node captured by @class.
            // Only keep S4 class-defining calls; extract class name from first string arg.
            if !S4_CLASS_FUNCTIONS_R.contains(&name.as_str()) {
                return false;
            }
            if let Some(class_name) = first_string_arg_r(node, source) {
                *name = class_name.to_string();
                true
            } else {
                // Can't extract class name — discard
                false
            }
        }
        ChunkType::Constant => {
            // This is a binary_operator with non-function rhs.
            // Could be R6Class assignment or a constant.
            if is_inside_function_r(node) {
                return false;
            }

            // Check if rhs is a call to R6Class
            let rhs = node.child_by_field_name("rhs");
            if let Some(rhs_node) = rhs {
                if rhs_node.kind() == "call" {
                    if let Some(fn_name) = call_function_name_r(rhs_node, source) {
                        if fn_name == "R6Class" {
                            *chunk_type = ChunkType::Class;
                            return true;
                        }
                    }
                }
            }

            // Not R6 — only keep UPPER_CASE constants
            is_upper_snake_case_r(name)
        }
        _ => true,
    }
}

static LANG_R: LanguageDef = LanguageDef {
    name: "r",
    grammar: Some(|| tree_sitter_r::LANGUAGE.into()),
    extensions: &["r", "R"],
    chunk_query: include_str!("queries/r.chunks.scm"),
    call_query: Some(include_str!("queries/r.calls.scm")),
    doc_nodes: &["comment"],
    stopwords: &[
        "function", "if", "else", "for", "in", "while", "repeat", "break", "next", "return",
        "library", "require", "source", "TRUE", "FALSE", "NULL", "NA", "Inf", "NaN", "print",
        "cat", "paste", "paste0", "sprintf", "message", "warning", "stop", "tryCatch", "c", "list",
        "data", "frame", "matrix", "vector", "length", "nrow", "ncol",
    ],
    extract_return_nl: extract_return_r,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/testthat/test-{stem}.R")),
    post_process_chunk: Some(post_process_r_r as PostProcessChunkFn),
    test_markers: &["test_that", "expect_"],
    test_path_patterns: &["%/tests/%", "%/testthat/%", "test-%.R", "test_%.R"],
    doc_format: "r_roxygen",
    doc_convention: "Use roxygen2 format: @param, @return, @export tags.",
    field_style: FieldStyle::NameFirst {
        separators: "=<",
        strip_prefixes: "",
    },
    ..DEFAULTS
};

pub fn definition_r() -> &'static LanguageDef {
    &LANG_R
}

// ============================================================================
// Razor (razor)
// ============================================================================

/// Detect language for `<script>` and `<style>` elements.
/// Fires for every `element` node — returns `_skip` for non-script/style elements.
/// Checks for TypeScript via `lang="ts"` or `type="text/typescript"` attributes.
fn detect_razor_element_language_razor(
    node: tree_sitter::Node,
    source: &str,
) -> Option<&'static str> {
    let text = &source[node.byte_range()];
    // Only check the opening tag (first ~200 bytes) to avoid scanning large elements
    let prefix = &text[..text.len().min(200)];
    let lower = prefix.to_ascii_lowercase();
    if lower.starts_with("<script") {
        if lower.contains("lang=\"ts\"") || lower.contains("type=\"text/typescript\"") {
            tracing::debug!("Razor <script> detected as TypeScript");
            return Some("typescript");
        }
        tracing::debug!("Razor <script> detected as JavaScript");
        None // default: javascript
    } else if lower.starts_with("<style") {
        tracing::debug!("Razor <style> detected as CSS");
        Some("css")
    } else {
        Some("_skip") // not script or style
    }
}

/// Extract the HTML tag name from an element node's source text.
/// Returns the tag name (lowercase) from `<tagname ...>`.
fn extract_tag_name_razor(node: tree_sitter::Node, source: &str) -> Option<String> {
    let text = &source[node.byte_range()];
    if !text.starts_with('<') {
        return None;
    }
    let after_lt = &text[1..];
    let name: String = after_lt
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    Some(name.to_lowercase())
}

/// Extract text content from an element, skipping nested child elements.
/// Used for heading elements (h1-h6) to get the visible text.
fn extract_text_content_razor(node: tree_sitter::Node, source: &str) -> String {
    let full = &source[node.byte_range()];
    // Strip opening tag
    let after_open = if let Some(pos) = full.find('>') {
        &full[pos + 1..]
    } else {
        return String::new();
    };
    // Strip closing tag
    let content = if let Some(pos) = after_open.rfind("</") {
        &after_open[..pos]
    } else {
        after_open
    };
    // Strip any HTML tags from content for clean text
    let mut result = String::new();
    let mut in_tag = false;
    for ch in content.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result.trim().to_string()
}

/// Extract an attribute value from an element's opening tag text.
fn extract_attribute_from_text_razor(text: &str, attr_name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let pattern = format!("{}=\"", attr_name);
    if let Some(pos) = lower.find(&pattern) {
        let after = &text[pos + pattern.len()..];
        if let Some(end) = after.find('"') {
            let value = &after[..end];
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Heading tags (h1-h6)
const HEADING_TAGS_RAZOR: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// HTML5 landmark elements
const LANDMARK_TAGS_RAZOR: &[&str] = &["header", "nav", "main", "footer", "aside", "article"];

/// Post-process Razor chunks: assign names to razor_block and element nodes, filter noise.
fn post_process_razor_razor(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    match node.kind() {
        "razor_block" => {
            // Name from source text prefix
            let text = &source[node.byte_range()];
            if text.starts_with("@code") {
                *name = "code".to_string();
            } else if text.starts_with("@functions") {
                *name = "functions".to_string();
            } else {
                // Anonymous @{ } block — skip, methods inside are captured individually
                tracing::debug!("Skipping anonymous razor block");
                return false;
            }
            *chunk_type = ChunkType::Module;
            true
        }
        "element" => {
            let tag = match extract_tag_name_razor(node, source) {
                Some(t) => t,
                None => return false,
            };

            if HEADING_TAGS_RAZOR.contains(&tag.as_str()) {
                // Heading → Section with text content as name
                let text = extract_text_content_razor(node, source);
                if text.is_empty() {
                    return false;
                }
                *name = text;
                *chunk_type = ChunkType::Section;
                tracing::debug!(tag = %tag, name = %name, "Razor heading element");
                true
            } else if LANDMARK_TAGS_RAZOR.contains(&tag.as_str()) {
                // Landmark → Section with id or aria-label as name
                let text = &source[node.byte_range()];
                let label = extract_attribute_from_text_razor(text, "id")
                    .or_else(|| extract_attribute_from_text_razor(text, "aria-label"));
                *name = label.unwrap_or_else(|| tag.clone());
                *chunk_type = ChunkType::Section;
                tracing::debug!(tag = %tag, name = %name, "Razor landmark element");
                true
            } else {
                // All other elements → filter out (noise)
                false
            }
        }
        // C# constructor_declaration nodes inside razor_block
        "constructor_declaration"
            if matches!(*chunk_type, ChunkType::Function | ChunkType::Method) =>
        {
            *chunk_type = ChunkType::Constructor;
            true
        }
        _ => true, // Pass through C# chunks unchanged
    }
}

/// Extracts the return type from a C# method signature and formats it as documentation text.
/// Parses a C# method signature to identify the return type, which appears before the method name in C#. Filters out common C# modifiers and keywords to isolate the actual return type. The return type is then tokenized and formatted into a documentation string.
/// # Arguments
/// `signature` - A C# method signature string to parse for the return type.
/// # Returns
/// `Some(String)` containing the formatted return type documentation if a valid non-void return type is found, or `None` if the signature does not contain a recognizable return type.
fn extract_return_razor(signature: &str) -> Option<String> {
    // C#: return type before method name
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

static LANG_RAZOR: LanguageDef = LanguageDef {
    name: "razor",
    grammar: Some(|| tree_sitter_razor::LANGUAGE.into()),
    extensions: &["cshtml", "razor"],
    chunk_query: include_str!("queries/razor.chunks.scm"),
    call_query: Some(include_str!("queries/razor.calls.scm")),
    doc_nodes: &["comment", "razor_comment"],
    method_containers: &[
        "class_declaration",
        "struct_declaration",
        "record_declaration",
        "interface_declaration",
        "declaration_list",
        "razor_block",
    ],
    stopwords: &[
        // C# keywords
        "public",
        "private",
        "protected",
        "internal",
        "static",
        "readonly",
        "sealed",
        "abstract",
        "virtual",
        "override",
        "async",
        "await",
        "class",
        "struct",
        "interface",
        "enum",
        "namespace",
        "using",
        "return",
        "if",
        "else",
        "for",
        "foreach",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "new",
        "this",
        "base",
        "try",
        "catch",
        "finally",
        "throw",
        "var",
        "void",
        "int",
        "string",
        "bool",
        "true",
        "false",
        "null",
        "get",
        "set",
        "value",
        "where",
        "partial",
        "event",
        "delegate",
        "record",
        "yield",
        "in",
        "out",
        "ref",
        // Razor directives (without @ — tokenizer strips it)
        "page",
        "model",
        "inject",
        "code",
        "functions",
        "rendermode",
        "attribute",
        "layout",
        "inherits",
        "implements",
        "preservewhitespace",
        "typeparam",
        "section",
    ],
    extract_return_nl: extract_return_razor,
    type_query: Some(include_str!("queries/razor.types.scm")),
    common_types: &[
        "string",
        "int",
        "bool",
        "object",
        "void",
        "double",
        "float",
        "long",
        "byte",
        "char",
        "decimal",
        "short",
        "uint",
        "ulong",
        "Task",
        "ValueTask",
        "List",
        "Dictionary",
        "HashSet",
        "Queue",
        "Stack",
        "IEnumerable",
        "IList",
        "IDictionary",
        "ICollection",
        "IQueryable",
        "Action",
        "Func",
        "Predicate",
        "EventHandler",
        "EventArgs",
        "IDisposable",
        "CancellationToken",
        "ILogger",
        "StringBuilder",
        "Exception",
        "Nullable",
        "Span",
        "Memory",
        "ReadOnlySpan",
        "IServiceProvider",
        "HttpContext",
        "IConfiguration",
    ],
    container_body_kinds: &["declaration_list"],
    post_process_chunk: Some(post_process_razor_razor),
    test_markers: &["[Test]", "[Fact]", "[Theory]", "[TestMethod]"],
    entry_point_names: &["Main", "OnInitializedAsync", "OnParametersSetAsync"],
    trait_method_names: &[
        "Equals",
        "GetHashCode",
        "ToString",
        "Dispose",
        "OnInitialized",
        "OnParametersSet",
        "OnAfterRender",
        "SetParametersAsync",
    ],
    injections: &[
        // <script> and <style> elements → JS/CSS via _inner content mode
        InjectionRule {
            container_kind: "element",
            content_kind: "_inner",
            target_language: "javascript",
            detect_language: Some(detect_razor_element_language_razor),
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_razor() -> &'static LanguageDef {
    &LANG_RAZOR
}

// ============================================================================
// Ruby (ruby)
// ============================================================================

static LANG_RUBY: LanguageDef = LanguageDef {
    name: "ruby",
    grammar: Some(|| tree_sitter_ruby::LANGUAGE.into()),
    extensions: &["rb", "rake", "gemspec"],
    chunk_query: include_str!("queries/ruby.chunks.scm"),
    call_query: Some(include_str!("queries/ruby.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    method_node_kinds: &["singleton_method"],
    method_containers: &["class", "module"],
    stopwords: &[
        "def",
        "class",
        "module",
        "end",
        "if",
        "elsif",
        "else",
        "unless",
        "case",
        "when",
        "for",
        "while",
        "until",
        "do",
        "begin",
        "rescue",
        "ensure",
        "raise",
        "return",
        "yield",
        "self",
        "super",
        "true",
        "false",
        "nil",
        "and",
        "or",
        "not",
        "in",
        "include",
        "extend",
        "prepend",
        "require",
        "private",
        "protected",
        "public",
        "attr_accessor",
        "attr_reader",
        "attr_writer",
    ],
    test_file_suggestion: Some(|stem, parent| format!("{parent}/spec/{stem}_spec.rb")),
    test_markers: &["describe ", "it ", "context "],
    test_path_patterns: &["%/spec/%", "%/test/%", "%\\_spec.rb", "%\\_test.rb"],
    trait_method_names: &[
        "to_s",
        "to_i",
        "to_f",
        "to_a",
        "to_h",
        "inspect",
        "hash",
        "eql?",
        "==",
        "<=>",
        "each",
        "initialize",
    ],
    doc_format: "hash_comment",
    doc_convention: "Use YARD format: @param, @return, @raise tags.",
    field_style: FieldStyle::NameFirst {
        separators: "=",
        strip_prefixes: "attr_accessor attr_reader attr_writer",
    },
    skip_line_prefixes: &["class ", "module "],
    ..DEFAULTS
};

pub fn definition_ruby() -> &'static LanguageDef {
    &LANG_RUBY
}

// ============================================================================
// Rust (rust)
// ============================================================================

/// Extracts the return type from a function signature and formats it as a documentation string.
/// Parses a function signature to find the return type annotation (after `->`) and returns a formatted string describing the return type. If no return type is specified or the annotation is empty, returns `None`.
/// # Arguments
/// `signature` - A function signature string that may contain a return type annotation.
/// # Returns
/// `Some(String)` containing the formatted return type description if a non-empty return type annotation is found; `None` if no return type annotation exists or if the annotation is empty.
fn extract_return_rust(signature: &str) -> Option<String> {
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
fn extract_container_name_rust_rust(container: tree_sitter::Node, source: &str) -> Option<String> {
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

/// Post-process Rust chunks: reclassify `new` methods as Constructor (convention).
fn post_process_rust_rust(
    name: &mut String,
    chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Rust convention: fn new_rust(...) inside an impl block is a constructor
    if *chunk_type == ChunkType::Method && name == "new" {
        *chunk_type = ChunkType::Constructor;
    }
    true
}

static LANG_RUST: LanguageDef = LanguageDef {
    name: "rust",
    grammar: Some(|| tree_sitter_rust::LANGUAGE.into()),
    extensions: &["rs"],
    chunk_query: include_str!("queries/rust.chunks.scm"),
    call_query: Some(include_str!("queries/rust.calls.scm")),
    doc_nodes: &["line_comment", "block_comment"],
    method_containers: &["impl_item", "trait_item"],
    stopwords: &[
        "fn", "let", "mut", "pub", "use", "impl", "mod", "struct", "enum", "trait", "type",
        "where", "const", "static", "unsafe", "async", "await", "move", "ref", "self", "super",
        "crate", "return", "if", "else", "for", "while", "loop", "match", "break", "continue",
        "as", "in", "true", "false", "some", "none", "ok", "err",
    ],
    extract_return_nl: extract_return_rust,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/{stem}_test.rs")),
    type_query: Some(include_str!("queries/rust.types.scm")),
    common_types: &[
        "String",
        "Vec",
        "Result",
        "Option",
        "Box",
        "Arc",
        "Rc",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "Path",
        "PathBuf",
        "Value",
        "Error",
        "Self",
        "None",
        "Some",
        "Ok",
        "Err",
        "Mutex",
        "RwLock",
        "Cow",
        "Pin",
        "Future",
        "Iterator",
        "Display",
        "Debug",
        "Clone",
        "Default",
        "Send",
        "Sync",
        "Sized",
        "Copy",
        "From",
        "Into",
        "AsRef",
        "AsMut",
        "Deref",
        "DerefMut",
        "Read",
        "Write",
        "Seek",
        "BufRead",
        "ToString",
        "Serialize",
        "Deserialize",
    ],
    extract_container_name: Some(extract_container_name_rust_rust),
    post_process_chunk: Some(post_process_rust_rust as PostProcessChunkFn),
    test_markers: &["#[test]", "#[cfg(test)]"],
    test_path_patterns: &["%/tests/%", "%\\_test.rs"],
    entry_point_names: &["main"],
    trait_method_names: &[
        // std::fmt
        "fmt",
        // std::convert
        "from",
        "into",
        "try_from",
        "try_into",
        // std::ops
        "deref",
        "deref_mut",
        "drop",
        "index",
        "index_mut",
        "add",
        "sub",
        "mul",
        "div",
        "rem",
        "neg",
        "not",
        "bitor",
        "bitand",
        "bitxor",
        "shl",
        "shr",
        // std::cmp
        "eq",
        "ne",
        "partial_cmp",
        "cmp",
        // std::hash
        "hash",
        // std::clone
        "clone",
        "clone_from",
        // std::default
        "default",
        // std::iter
        "next",
        "into_iter",
        // std::io
        "read",
        "write",
        "flush",
        // std::str
        "from_str",
        // std::convert / std::borrow
        "as_ref",
        "as_mut",
        "borrow",
        "borrow_mut",
        // serde
        "serialize",
        "deserialize",
        // std::error
        "source",
        // std::future
        "poll",
    ],
    doc_format: "triple_slash",
    doc_convention:
        "Use `# Arguments`, `# Returns`, `# Errors`, `# Panics` sections as appropriate.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "pub pub(crate) pub(super)",
    },
    skip_line_prefixes: &[
        "pub struct",
        "pub enum",
        "pub union",
        "struct",
        "enum",
        "union",
    ],
    ..DEFAULTS
};

pub fn definition_rust() -> &'static LanguageDef {
    &LANG_RUST
}

// ============================================================================
// Scala (scala)
// ============================================================================

/// Extracts the return type from a Scala function signature and formats it as a documentation string.
/// Parses a Scala function signature to find the return type annotation (the type following `:` after the parameter list and before `=` or `{`), then formats it as a "Returns {type}" string suitable for documentation.
/// # Arguments
/// `signature` - A Scala function signature string, e.g., `def foo(x: Int): String = ...`
/// # Returns
/// `Some(String)` containing the formatted return type documentation (e.g., "Returns String"), or `None` if no return type annotation is found or the signature is malformed.
fn extract_return_scala(signature: &str) -> Option<String> {
    // Scala: def foo(x: Int): String = ...
    // Look for `: ReturnType` after last `)` and before `=` or `{`
    let paren_pos = signature.rfind(')')?;
    let after_paren = &signature[paren_pos + 1..];

    // Find the terminator (= or {)
    let end_pos = after_paren
        .find('=')
        .or_else(|| after_paren.find('{'))
        .unwrap_or(after_paren.len());
    let between = &after_paren[..end_pos];

    // Look for colon
    let colon_pos = between.find(':')?;
    let ret_type = between[colon_pos + 1..].trim();
    if ret_type.is_empty() {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

static LANG_SCALA: LanguageDef = LanguageDef {
    name: "scala",
    grammar: Some(|| tree_sitter_scala::LANGUAGE.into()),
    extensions: &["scala", "sc"],
    chunk_query: include_str!("queries/scala.chunks.scm"),
    call_query: Some(include_str!("queries/scala.calls.scm")),
    doc_nodes: &["comment", "block_comment"],
    method_containers: &["class_definition", "trait_definition", "object_definition"],
    stopwords: &[
        "def", "val", "var", "class", "object", "trait", "sealed", "case", "abstract", "override",
        "implicit", "lazy", "extends", "with", "import", "package", "match", "if", "else", "for",
        "while", "yield", "return", "throw", "try", "catch", "finally", "new", "this", "super",
        "true", "false", "null",
    ],
    extract_return_nl: extract_return_scala,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/src/test/scala/{stem}Spec.scala")),
    type_query: Some(include_str!("queries/scala.types.scm")),
    common_types: &[
        "String", "Int", "Long", "Double", "Float", "Boolean", "Char", "Byte", "Short", "Unit",
        "Any", "AnyRef", "AnyVal", "Nothing", "Null", "Option", "Some", "None", "List", "Map",
        "Set", "Seq", "Vector", "Array", "Future", "Either", "Left", "Right", "Try", "Success",
        "Failure", "Iterator", "Iterable", "Ordering",
    ],
    container_body_kinds: &["template_body"],
    test_markers: &["@Test", "\"should", "it should"],
    test_path_patterns: &["%/test/%", "%/tests/%", "%Spec.scala", "%Test.scala"],
    entry_point_names: &["main"],
    trait_method_names: &[
        "equals", "hashCode", "toString", "compare", "apply", "unapply",
    ],
    doc_format: "javadoc",
    doc_convention: "Use Scaladoc format: @param, @return, @throws tags.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "val var private protected override lazy",
    },
    skip_line_prefixes: &["class ", "case class", "sealed class", "trait ", "object "],
    ..DEFAULTS
};

pub fn definition_scala() -> &'static LanguageDef {
    &LANG_SCALA
}

// ============================================================================
// Solidity (solidity)
// ============================================================================

/// Extracts the return type information from a Solidity function signature.
/// Parses a Solidity function signature to find the `returns` clause and extracts the return type specification. Tokenizes the return type declaration and formats it as a human-readable string.
/// # Arguments
/// * `signature` - A Solidity function signature string (e.g., "function add(uint a, uint b) public pure returns (uint)")
/// # Returns
/// `Some(String)` containing the formatted return type as "Returns <type>" if a `returns` clause exists and contains a non-empty type specification, or `None` if no `returns` clause is found or it is empty.
fn extract_return_solidity(signature: &str) -> Option<String> {
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

static LANG_SOLIDITY: LanguageDef = LanguageDef {
    name: "solidity",
    grammar: Some(|| tree_sitter_solidity::LANGUAGE.into()),
    extensions: &["sol"],
    chunk_query: include_str!("queries/solidity.chunks.scm"),
    call_query: Some(include_str!("queries/solidity.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["contract_body"],
    stopwords: &[
        "if",
        "else",
        "for",
        "while",
        "do",
        "return",
        "break",
        "continue",
        "contract",
        "interface",
        "library",
        "struct",
        "enum",
        "function",
        "modifier",
        "event",
        "error",
        "mapping",
        "address",
        "bool",
        "string",
        "bytes",
        "uint",
        "int",
        "uint256",
        "int256",
        "uint8",
        "bytes32",
        "public",
        "private",
        "internal",
        "external",
        "view",
        "pure",
        "payable",
        "memory",
        "storage",
        "calldata",
        "indexed",
        "virtual",
        "override",
        "abstract",
        "immutable",
        "constant",
        "emit",
        "require",
        "assert",
        "revert",
        "this",
        "super",
        "true",
        "false",
        "msg",
        "block",
        "tx",
    ],
    extract_return_nl: extract_return_solidity,
    common_types: &[
        "address", "bool", "string", "bytes", "uint256", "int256", "uint8", "uint16", "uint32",
        "uint64", "uint128", "int8", "int16", "int32", "int64", "int128", "bytes32", "bytes4",
        "bytes20",
    ],
    container_body_kinds: &["contract_body"],
    test_path_patterns: &["%/test/%", "%.t.sol"],
    entry_point_names: &["constructor", "receive", "fallback"],
    doc_format: "javadoc",
    doc_convention: "Use NatSpec format: @param, @return, @dev tags.",
    field_style: FieldStyle::NameFirst {
        separators: ";",
        strip_prefixes: "public private internal constant immutable",
    },
    skip_line_prefixes: &["contract ", "struct ", "enum ", "interface "],
    ..DEFAULTS
};

pub fn definition_solidity() -> &'static LanguageDef {
    &LANG_SOLIDITY
}

// ============================================================================
// Sql (sql)
// ============================================================================

/// Extracts the return type from a SQL function signature.
/// Searches for the "RETURNS" keyword in a SQL function signature and extracts the return type that follows it. The return type is the first word after "RETURNS", with any precision suffixes (e.g., "(10,2)") removed, and converted to lowercase.
/// # Arguments
/// * `signature` - A SQL function signature string to parse
/// # Returns
/// `Some(String)` containing a formatted return type description (e.g., "Returns int"), or `None` if no "RETURNS" keyword is found in the signature.
fn extract_return_sql(signature: &str) -> Option<String> {
    // SQL functions: look for RETURNS type between name and AS
    let upper = signature.to_uppercase();
    if let Some(ret_pos) = upper.find("RETURNS") {
        let after = &signature[ret_pos + 7..].trim();
        // Take the first word as the return type, lowercase it
        // SQL types are all-caps (DECIMAL, INT, VARCHAR) — just lowercase, don't tokenize
        let type_str = after.split_whitespace().next()?;
        // Strip precision suffix like (10,2)
        let base_type = type_str.split('(').next().unwrap_or(type_str);
        return Some(format!("Returns {}", base_type.to_lowercase()));
    }
    None
}

static LANG_SQL: LanguageDef = LanguageDef {
    name: "sql",
    grammar: Some(|| tree_sitter_sql::LANGUAGE.into()),
    extensions: &["sql"],
    chunk_query: include_str!("queries/sql.chunks.scm"),
    call_query: Some(include_str!("queries/sql.calls.scm")),
    signature_style: SignatureStyle::UntilAs,
    doc_nodes: &["comment", "marginalia"],
    stopwords: &[
        "create",
        "alter",
        "procedure",
        "function",
        "view",
        "trigger",
        "begin",
        "end",
        "declare",
        "set",
        "select",
        "from",
        "where",
        "insert",
        "into",
        "update",
        "delete",
        "exec",
        "execute",
        "as",
        "returns",
        "return",
        "if",
        "else",
        "while",
        "and",
        "or",
        "not",
        "null",
        "int",
        "varchar",
        "nvarchar",
        "decimal",
        "table",
        "on",
        "after",
        "before",
        "instead",
        "of",
        "for",
        "each",
        "row",
        "order",
        "by",
        "group",
        "having",
        "join",
        "inner",
        "left",
        "right",
        "outer",
        "go",
        "with",
        "nocount",
        "language",
        "replace",
    ],
    extract_return_nl: extract_return_sql,
    ..DEFAULTS
};

pub fn definition_sql() -> &'static LanguageDef {
    &LANG_SQL
}

// ============================================================================
// Structured Text (structured_text)
// ============================================================================

static LANG_STRUCTURED_TEXT: LanguageDef = LanguageDef {
    name: "structured_text",
    grammar: Some(|| tree_sitter_structured_text::LANGUAGE.into()),
    extensions: &["st", "stl"],
    chunk_query: include_str!("queries/structured_text.chunks.scm"),
    call_query: Some(include_str!("queries/structured_text.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["block_comment", "inline_comment"],
    method_node_kinds: &["method_definition"],
    method_containers: &["function_block_definition"],
    stopwords: &[
        // Control flow
        "IF",
        "THEN",
        "ELSIF",
        "ELSE",
        "END_IF",
        "CASE",
        "OF",
        "END_CASE",
        "FOR",
        "TO",
        "BY",
        "DO",
        "END_FOR",
        "WHILE",
        "END_WHILE",
        "REPEAT",
        "UNTIL",
        "END_REPEAT",
        "RETURN",
        "EXIT",
        // Declarations
        "PROGRAM",
        "END_PROGRAM",
        "FUNCTION",
        "END_FUNCTION",
        "FUNCTION_BLOCK",
        "END_FUNCTION_BLOCK",
        "METHOD",
        "END_METHOD",
        "ACTION",
        "END_ACTION",
        "TYPE",
        "END_TYPE",
        "STRUCT",
        "END_STRUCT",
        "VAR",
        "VAR_INPUT",
        "VAR_OUTPUT",
        "VAR_IN_OUT",
        "VAR_TEMP",
        "VAR_GLOBAL",
        "END_VAR",
        "CONSTANT",
        "RETAIN",
        "PERSISTENT",
        // Data types
        "BOOL",
        "BYTE",
        "WORD",
        "DWORD",
        "LWORD",
        "SINT",
        "INT",
        "DINT",
        "LINT",
        "USINT",
        "UINT",
        "UDINT",
        "ULINT",
        "REAL",
        "LREAL",
        "STRING",
        "WSTRING",
        "TIME",
        "DATE",
        "DATE_AND_TIME",
        "TIME_OF_DAY",
        "ARRAY",
        // Operators
        "AND",
        "OR",
        "XOR",
        "NOT",
        "MOD",
        // Literals
        "TRUE",
        "FALSE",
        // Access
        "PUBLIC",
        "PRIVATE",
        "PROTECTED",
        "INTERNAL",
        "FINAL",
        "ABSTRACT",
        "EXTENDS",
    ],
    type_query: Some(include_str!("queries/structured_text.types.scm")),
    common_types: &[
        "BOOL", "BYTE", "WORD", "DWORD", "LWORD", "SINT", "INT", "DINT", "LINT", "USINT", "UINT",
        "UDINT", "ULINT", "REAL", "LREAL", "STRING", "WSTRING", "TIME", "DATE", "TON", "TOF", "TP",
        "CTU", "CTD", "CTUD", "R_TRIG", "F_TRIG",
    ],
    entry_point_names: &["Main", "MAIN"],
    doc_format: "block_comment",
    doc_convention: "Use (* ... *) block comments before declarations.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &[
        "VAR",
        "END_VAR",
        "FUNCTION",
        "END_FUNCTION",
        "PROGRAM",
        "END_PROGRAM",
    ],
    ..DEFAULTS
};

pub fn definition_structured_text() -> &'static LanguageDef {
    &LANG_STRUCTURED_TEXT
}

// ============================================================================
// Svelte (svelte)
// ============================================================================

// No call query — JS/CSS calls are extracted via injection
// No type query — Svelte templates don't have typed references

/// HTML heading tags
const HEADING_TAGS_SVELTE: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// HTML landmark tags — always kept in output
const LANDMARK_TAGS_SVELTE: &[&str] = &[
    "nav", "main", "header", "footer", "aside", "section", "article", "form",
];

/// Tags that are noise unless they have an `id` attribute
const NOISE_TAGS_SVELTE: &[&str] = &[
    "div",
    "span",
    "p",
    "ul",
    "ol",
    "li",
    "table",
    "tr",
    "td",
    "th",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "details",
    "summary",
    "blockquote",
    "pre",
    "code",
    "a",
    "img",
    "button",
    "input",
    "label",
    "select",
    "textarea",
    "option",
];

/// Post-process Svelte element chunks.
/// Same logic as HTML: headings→Section, script/style→Module,
/// landmarks→Section, noise→filter unless id, else Property.
fn post_process_svelte_svelte(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let tag = name.to_lowercase();

    // Headings → Section with text content
    if HEADING_TAGS_SVELTE.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        // Extract text content from heading
        let content = &source[node.byte_range()];
        // Strip tags for the name
        let text = content
            .split('>')
            .nth(1)
            .and_then(|s| s.split('<').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !text.is_empty() {
            *name = text;
        }
        return true;
    }

    // Script/style → Module
    if tag == "script" || tag == "style" {
        *chunk_type = ChunkType::Module;
        // Try to name from src or type attribute
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(st) = start_tag {
            if let Some(src_val) = find_attribute_value_html(st, "src", source) {
                *name = format!("script:{src_val}");
                return true;
            }
            if let Some(lang_val) = find_attribute_value_html(st, "lang", source) {
                *name = format!("{tag}:{lang_val}");
                return true;
            }
        }
        return true;
    }

    // Landmarks → Section with id/aria-label
    if LANDMARK_TAGS_SVELTE.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(st) = start_tag {
            if let Some(id) = find_attribute_value_html(st, "id", source) {
                *name = format!("{tag}#{id}");
                return true;
            }
            if let Some(label) = find_attribute_value_html(st, "aria-label", source) {
                *name = format!("{tag}:{label}");
                return true;
            }
        }
        return true;
    }

    // Noise tags → filter unless they have an id
    if NOISE_TAGS_SVELTE.contains(&tag.as_str()) {
        let start_tag = find_child_by_kind_html(node, "start_tag")
            .or_else(|| find_child_by_kind_html(node, "self_closing_tag"));
        if let Some(st) = start_tag {
            if let Some(id) = find_attribute_value_html(st, "id", source) {
                *name = format!("{tag}#{id}");
                *chunk_type = ChunkType::Property;
                return true;
            }
        }
        return false; // Filter out
    }

    true
}

static LANG_SVELTE: LanguageDef = LanguageDef {
    name: "svelte",
    grammar: Some(|| tree_sitter_svelte::LANGUAGE.into()),
    extensions: &["svelte"],
    chunk_query: include_str!("queries/svelte.chunks.scm"),
    signature_style: SignatureStyle::Breadcrumb,
    doc_nodes: &["comment"],
    stopwords: &[
        "div",
        "span",
        "p",
        "a",
        "img",
        "ul",
        "ol",
        "li",
        "table",
        "tr",
        "td",
        "th",
        "form",
        "input",
        "button",
        "label",
        "select",
        "option",
        "textarea",
        "br",
        "hr",
        "head",
        "body",
        "html",
        "meta",
        "link",
        "title",
        "script",
        "style",
        "class",
        "id",
        "href",
        "src",
        "alt",
        "type",
        "value",
        "name",
        "slot",
        "each",
        "if",
        "else",
        "await",
        "then",
        "catch",
        "key",
        "let",
        "const",
        "export",
        "import",
        "bind",
        "on",
        "use",
        "transition",
        "animate",
        "in",
        "out",
    ],
    post_process_chunk: Some(post_process_svelte_svelte),
    injections: &[
        // <script> blocks → JavaScript (or TypeScript via detect_script_language)
        InjectionRule {
            container_kind: "script_element",
            content_kind: "raw_text",
            target_language: "javascript",
            detect_language: Some(detect_script_language_html),
            content_scoped_lines: false,
        },
        // <style> blocks → CSS
        InjectionRule {
            container_kind: "style_element",
            content_kind: "raw_text",
            target_language: "css",
            detect_language: None,
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_svelte() -> &'static LanguageDef {
    &LANG_SVELTE
}

// ============================================================================
// Swift (swift)
// ============================================================================

/// Extracts the return type from a Swift function signature and formats it as documentation text.
/// Parses a Swift function signature to find the return type annotation (the part after `->` and before `{`), then formats it as a documentation string. Void and empty return types are treated as no return value.
/// # Arguments
/// * `signature` - A Swift function signature string containing a `->` return type annotation
/// # Returns
/// Returns `Some(String)` containing formatted return documentation if a non-void return type is found, or `None` if the signature has no `->` marker, an empty return type, or a `Void` return type.
fn extract_return_swift(signature: &str) -> Option<String> {
    // Swift: func name(params) -> ReturnType {
    // Find "->" and extract the type between it and "{"
    let arrow_pos = signature.find("->")?;
    let after_arrow = &signature[arrow_pos + 2..];

    let end_pos = after_arrow.find('{').unwrap_or(after_arrow.len());
    let ret_type = after_arrow[..end_pos].trim();

    if ret_type.is_empty() || ret_type == "Void" {
        return None;
    }

    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    Some(format!("Returns {}", ret_words))
}

/// Post-process Swift chunks to reclassify `class_declaration` into the correct type.
/// Swift's tree-sitter grammar uses `class_declaration` for all structural types:
/// classes, structs, enums, actors, and extensions. We distinguish them by:
/// - `enum_class_body` child → Enum
/// - Anonymous "struct" keyword → Struct
/// - Anonymous "actor" keyword → Class (actor treated as class)
/// - Anonymous "extension" keyword → Extension
/// - Anonymous "class" keyword or default → Class
/// Also reclassifies `init` methods as Constructor.
fn post_process_swift_swift(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    // init_declaration nodes and init methods are constructors
    if node.kind() == "init_declaration" {
        *chunk_type = ChunkType::Constructor;
        if name == "<anonymous>" {
            *name = "init".to_string();
        }
        return true;
    }
    if matches!(*chunk_type, ChunkType::Function | ChunkType::Method) && name == "init" {
        *chunk_type = ChunkType::Constructor;
        return true;
    }

    if node.kind() != "class_declaration" {
        return true;
    }

    let _span = tracing::debug_span!("post_process_swift", kind = node.kind()).entered();

    let mut cursor = node.walk();
    let mut has_enum_body = false;
    let mut keyword = "";

    for child in node.children(&mut cursor) {
        match child.kind() {
            "enum_class_body" => has_enum_body = true,
            _ if !child.is_named() => {
                let text = &source[child.byte_range()];
                match text {
                    "struct" => keyword = "struct",
                    "class" => keyword = "class",
                    "actor" => keyword = "actor",
                    "extension" => keyword = "extension",
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if has_enum_body {
        *chunk_type = ChunkType::Enum;
        tracing::debug!("Reclassified class_declaration as Enum (has enum_class_body)");
    } else {
        match keyword {
            "struct" => {
                *chunk_type = ChunkType::Struct;
                tracing::debug!("Reclassified class_declaration as Struct");
            }
            "actor" => {
                // Actor → Class (closest semantic match)
                tracing::debug!("Reclassified class_declaration as Class (actor)");
            }
            "extension" => {
                *chunk_type = ChunkType::Extension;
                tracing::debug!("Reclassified class_declaration as Extension");
            }
            _ => {
                // "class" or unknown — default @class stays
            }
        }
    }

    true
}

static LANG_SWIFT: LanguageDef = LanguageDef {
    name: "swift",
    grammar: Some(|| tree_sitter_swift::LANGUAGE.into()),
    extensions: &["swift"],
    chunk_query: include_str!("queries/swift.chunks.scm"),
    call_query: Some(include_str!("queries/swift.calls.scm")),
    doc_nodes: &["comment", "multiline_comment"],
    method_containers: &["class_body"],
    stopwords: &[
        "func",
        "var",
        "let",
        "class",
        "struct",
        "enum",
        "protocol",
        "extension",
        "actor",
        "import",
        "return",
        "if",
        "else",
        "guard",
        "switch",
        "case",
        "for",
        "while",
        "repeat",
        "break",
        "continue",
        "self",
        "super",
        "nil",
        "true",
        "false",
        "is",
        "as",
        "in",
        "try",
        "catch",
        "throw",
        "throws",
        "async",
        "await",
        "public",
        "private",
        "internal",
        "open",
        "fileprivate",
        "static",
        "final",
        "override",
        "mutating",
        "typealias",
        "where",
        "some",
        "any",
    ],
    extract_return_nl: extract_return_swift,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.swift")),
    test_name_suggestion: Some(|name| super::pascal_test_name("test", name)),
    type_query: Some(include_str!("queries/swift.types.scm")),
    common_types: &[
        "String",
        "Int",
        "Double",
        "Float",
        "Bool",
        "Character",
        "UInt",
        "Int8",
        "Int16",
        "Int32",
        "Int64",
        "UInt8",
        "UInt16",
        "UInt32",
        "UInt64",
        "Optional",
        "Array",
        "Dictionary",
        "Set",
        "Any",
        "AnyObject",
        "Void",
        "Never",
        "Error",
        "Codable",
        "Equatable",
        "Hashable",
        "Comparable",
        "Identifiable",
        "CustomStringConvertible",
    ],
    container_body_kinds: &["class_body", "protocol_body"],
    post_process_chunk: Some(post_process_swift_swift),
    test_markers: &["func test"],
    test_path_patterns: &["%/Tests/%", "%Tests.swift"],
    entry_point_names: &["main"],
    trait_method_names: &["hash", "encode", "init", "deinit", "description"],
    doc_format: "javadoc",
    doc_convention: "Use Swift doc comments: - Parameters:, - Returns:, - Throws: sections.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "let var private public internal fileprivate open static weak lazy",
    },
    skip_line_prefixes: &["class ", "struct ", "enum ", "protocol "],
    ..DEFAULTS
};

pub fn definition_swift() -> &'static LanguageDef {
    &LANG_SWIFT
}

// ============================================================================
// Toml (toml_lang)
// ============================================================================

/// Strip quotes from TOML quoted keys.
fn post_process_toml_toml(
    name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Only keep top-level pairs (skip pairs nested inside tables)
    if node.kind() == "pair" {
        if let Some(parent) = node.parent() {
            // A pair inside a table or table_array_element is nested
            if parent.kind() == "table" || parent.kind() == "table_array_element" {
                return false;
            }
        }
    }
    // Strip surrounding quotes from quoted keys
    if name.starts_with('"') && name.ends_with('"') && name.len() >= 2 {
        *name = name[1..name.len() - 1].to_string();
    }
    true
}

/// Extracts the return type from a function signature.
/// This function is a no-op for TOML content, as TOML has no function or return type syntax.
/// # Arguments
/// * `_signature` - A function signature string (unused for TOML)
/// # Returns
/// Always returns `None`, as TOML does not support function definitions or return type annotations.
fn extract_return_toml(_signature: &str) -> Option<String> {
    // TOML has no functions or return types
    None
}

static LANG_TOML: LanguageDef = LanguageDef {
    name: "toml",
    grammar: Some(|| tree_sitter_toml::LANGUAGE.into()),
    extensions: &["toml"],
    chunk_query: include_str!("queries/toml_lang.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &["true", "false"],
    extract_return_nl: extract_return_toml,
    post_process_chunk: Some(post_process_toml_toml),
    ..DEFAULTS
};

pub fn definition_toml() -> &'static LanguageDef {
    &LANG_TOML
}

// ============================================================================
// Typescript (typescript)
// ============================================================================

/// Returns true if the node is nested inside a function/method/arrow body.
fn is_inside_function_typescript(node: tree_sitter::Node) -> bool {
    let mut cursor = node.parent();
    while let Some(parent) = cursor {
        match parent.kind() {
            "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function" => return true,
            _ => {}
        }
        cursor = parent.parent();
    }
    false
}

/// Post-process TypeScript chunks: skip `@const` captures whose value is an arrow_function
/// or function_expression (already captured as Function), and skip const inside function bodies.
fn post_process_typescript_typescript(
    _name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if *chunk_type == ChunkType::Constant {
        // Skip const declarations inside function bodies — only capture module-level
        if is_inside_function_typescript(node) {
            return false;
        }
        // node is the variable_declarator; check if the value child is a function
        if let Some(value) = node.child_by_field_name("value") {
            let kind = value.kind();
            if kind == "arrow_function" || kind == "function_expression" || kind == "function" {
                return false;
            }
        }
    }
    true
}

/// Extracts the return type from a TypeScript function signature and formats it as a description.
/// # Arguments
/// * `signature` - A TypeScript function signature string to parse
/// # Returns
/// `Some(String)` containing a formatted return type description (e.g., "Returns string") if a return type annotation is found after `):`, or `None` if no return type is present or the signature is malformed.
fn extract_return_typescript(signature: &str) -> Option<String> {
    // TypeScript: return type after `):` e.g. `function foo(): string`
    if let Some(colon) = signature.rfind("):") {
        let ret = signature[colon + 2..].trim();
        if ret.is_empty() {
            return None;
        }
        let ret_words = crate::nl::tokenize_identifier(ret).join(" ");
        return Some(format!("Returns {}", ret_words));
    }
    None
}

static LANG_TYPESCRIPT: LanguageDef = LanguageDef {
    name: "typescript",
    grammar: Some(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
    extensions: &["ts", "tsx"],
    chunk_query: include_str!("queries/typescript.chunks.scm"),
    call_query: Some(include_str!("queries/typescript.calls.scm")),
    doc_nodes: &["comment"],
    method_containers: &["class_body", "class_declaration"],
    stopwords: &[
        "function",
        "const",
        "let",
        "var",
        "return",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "new",
        "this",
        "class",
        "extends",
        "import",
        "export",
        "from",
        "default",
        "try",
        "catch",
        "finally",
        "throw",
        "async",
        "await",
        "true",
        "false",
        "null",
        "undefined",
        "typeof",
        "instanceof",
        "void",
    ],
    extract_return_nl: extract_return_typescript,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}.test.ts")),
    test_name_suggestion: Some(|name| format!("test('{}', ...)", name)),
    type_query: Some(include_str!("queries/typescript.types.scm")),
    common_types: &[
        "string",
        "number",
        "boolean",
        "void",
        "null",
        "undefined",
        "any",
        "never",
        "unknown",
        "Array",
        "Map",
        "Set",
        "Promise",
        "Record",
        "Partial",
        "Required",
        "Readonly",
        "Pick",
        "Omit",
        "Exclude",
        "Extract",
        "NonNullable",
        "ReturnType",
        "Date",
        "Error",
        "RegExp",
        "Function",
        "Object",
        "Symbol",
    ],
    container_body_kinds: &["class_body"],
    post_process_chunk: Some(post_process_typescript_typescript as PostProcessChunkFn),
    test_markers: &["describe(", "it(", "test("],
    test_path_patterns: &["%.test.%", "%.spec.%", "%/tests/%"],
    entry_point_names: &[
        "handler",
        "middleware",
        "beforeEach",
        "afterEach",
        "beforeAll",
        "afterAll",
    ],
    trait_method_names: &["toString", "valueOf", "toJSON"],
    doc_format: "javadoc",
    doc_convention: "Use JSDoc format: @param {type} name, @returns {type}, @throws {type}.",
    field_style: FieldStyle::NameFirst {
        separators: ":=;",
        strip_prefixes: "public private protected readonly static",
    },
    skip_line_prefixes: &["class ", "interface ", "type ", "export "],
    ..DEFAULTS
};

pub fn definition_typescript() -> &'static LanguageDef {
    &LANG_TYPESCRIPT
}

// ============================================================================
// Vbnet (vbnet)
// ============================================================================

/// Post-process: assign "New" name to constructor chunks and reclassify as Constructor.
fn post_process_vbnet_vbnet(
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
/// Parses a VB.NET function signature to find the return type specified after the closing parenthesis with the "As" keyword. If found, tokenizes and formats the return type as a "Returns" statement.
/// # Arguments
/// * `signature` - A VB.NET function signature string to parse
/// # Returns
/// `Some(String)` containing the formatted return type as "Returns {type}" if a return type is found after "As" keyword, or `None` if no return type is present or the signature format is invalid.
fn extract_return_vbnet(signature: &str) -> Option<String> {
    // VB.NET: Function Name(...) As ReturnType
    // Look for "As" after the closing paren
    if let Some(paren_close) = signature.rfind(')') {
        let after = signature[paren_close + 1..].trim();
        if let Some(rest) = after
            .strip_prefix("As")
            .or_else(|| after.strip_prefix("as"))
        {
            let ret_type = rest.split_whitespace().next()?;
            if !ret_type.is_empty() {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static LANG_VBNET: LanguageDef = LanguageDef {
    name: "vbnet",
    grammar: Some(|| tree_sitter_vb_dotnet::LANGUAGE.into()),
    extensions: &["vb"],
    chunk_query: include_str!("queries/vbnet.chunks.scm"),
    call_query: Some(include_str!("queries/vbnet.calls.scm")),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    method_containers: &[
        "class_block",
        "module_block",
        "structure_block",
        "interface_block",
    ],
    stopwords: &[
        // VB.NET keywords
        "public",
        "private",
        "protected",
        "friend",
        "shared",
        "readonly",
        "mustinherit",
        "notinheritable",
        "mustoverride",
        "overridable",
        "overrides",
        "overloads",
        "shadows",
        "class",
        "module",
        "structure",
        "interface",
        "enum",
        "namespace",
        "imports",
        "return",
        "if",
        "then",
        "else",
        "elseif",
        "end",
        "for",
        "each",
        "next",
        "while",
        "do",
        "loop",
        "select",
        "case",
        "exit",
        "continue",
        "new",
        "me",
        "mybase",
        "myclass",
        "try",
        "catch",
        "finally",
        "throw",
        "dim",
        "as",
        "sub",
        "function",
        "property",
        "event",
        "delegate",
        "integer",
        "string",
        "boolean",
        "double",
        "single",
        "long",
        "byte",
        "char",
        "decimal",
        "short",
        "object",
        "true",
        "false",
        "nothing",
        "void",
        "get",
        "set",
        "value",
        "where",
        "partial",
        "of",
        "in",
        "out",
        "byval",
        "byref",
        "optional",
        "paramarray",
        "handles",
        "withevents",
        "addhandler",
        "removehandler",
        "raiseevent",
        "not",
        "and",
        "or",
        "andalso",
        "orelse",
        "xor",
        "mod",
        "like",
        "is",
        "isnot",
        "with",
        "using",
        "synclock",
        "redim",
        "preserve",
        "goto",
    ],
    extract_return_nl: extract_return_vbnet,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.vb")),
    type_query: Some(include_str!("queries/vbnet.types.scm")),
    common_types: &[
        "String",
        "Integer",
        "Boolean",
        "Object",
        "Double",
        "Single",
        "Long",
        "Byte",
        "Char",
        "Decimal",
        "Short",
        "UInteger",
        "ULong",
        "Task",
        "ValueTask",
        "List",
        "Dictionary",
        "HashSet",
        "Queue",
        "Stack",
        "IEnumerable",
        "IList",
        "IDictionary",
        "ICollection",
        "IQueryable",
        "Action",
        "Func",
        "Predicate",
        "EventHandler",
        "EventArgs",
        "IDisposable",
        "CancellationToken",
        "ILogger",
        "StringBuilder",
        "Exception",
        "Nullable",
    ],
    post_process_chunk: Some(post_process_vbnet_vbnet),
    test_markers: &["<Test>", "<Fact>", "<Theory>", "<TestMethod>"],
    test_path_patterns: &["%/Tests/%", "%/tests/%", "%Tests.vb"],
    entry_point_names: &["Main"],
    trait_method_names: &[
        "Equals",
        "GetHashCode",
        "ToString",
        "CompareTo",
        "Dispose",
        "GetEnumerator",
        "MoveNext",
    ],
    doc_convention: "Use XML doc comments: <summary>, <param>, <returns> tags.",
    skip_line_prefixes: &["Class ", "Structure ", "Interface ", "Enum "],
    ..DEFAULTS
};

pub fn definition_vbnet() -> &'static LanguageDef {
    &LANG_VBNET
}

// ============================================================================
// Vue (vue)
// ============================================================================

// No call query — JS/CSS calls are extracted via injection
// No type query — Vue templates don't have typed references

/// HTML heading tags
const HEADING_TAGS_VUE: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// HTML landmark tags — always kept in output
const LANDMARK_TAGS_VUE: &[&str] = &[
    "nav", "main", "header", "footer", "aside", "section", "article", "form",
];

/// Tags that are noise unless they have an `id` attribute
const NOISE_TAGS_VUE: &[&str] = &[
    "div",
    "span",
    "p",
    "ul",
    "ol",
    "li",
    "table",
    "tr",
    "td",
    "th",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "details",
    "summary",
    "blockquote",
    "pre",
    "code",
    "a",
    "img",
    "button",
    "input",
    "label",
    "select",
    "textarea",
    "option",
];

/// Post-process Vue element chunks.
/// Same logic as HTML/Svelte: headings→Section, script/style/template→Module,
/// landmarks→Section, noise→filter unless id, else Property.
fn post_process_vue_vue(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let tag = name.to_lowercase();

    // Headings → Section with text content
    if HEADING_TAGS_VUE.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        let content = &source[node.byte_range()];
        let text = content
            .split('>')
            .nth(1)
            .and_then(|s| s.split('<').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !text.is_empty() {
            *name = text;
        }
        return true;
    }

    // Script/style/template → Module
    if tag == "script" || tag == "style" || tag == "template" {
        *chunk_type = ChunkType::Module;
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(st) = start_tag {
            if let Some(src_val) = find_attribute_value_html(st, "src", source) {
                *name = format!("script:{src_val}");
                return true;
            }
            if let Some(lang_val) = find_attribute_value_html(st, "lang", source) {
                *name = format!("{tag}:{lang_val}");
                return true;
            }
            // Vue setup attribute (boolean — no value)
            if has_attribute_html(st, "setup", source) {
                *name = "script:setup".to_string();
                return true;
            }
        }
        return true;
    }

    // Landmarks → Section with id/aria-label
    if LANDMARK_TAGS_VUE.contains(&tag.as_str()) {
        *chunk_type = ChunkType::Section;
        let start_tag = find_child_by_kind_html(node, "start_tag");
        if let Some(st) = start_tag {
            if let Some(id) = find_attribute_value_html(st, "id", source) {
                *name = format!("{tag}#{id}");
                return true;
            }
            if let Some(label) = find_attribute_value_html(st, "aria-label", source) {
                *name = format!("{tag}:{label}");
                return true;
            }
        }
        return true;
    }

    // Noise tags → filter unless they have an id
    if NOISE_TAGS_VUE.contains(&tag.as_str()) {
        let start_tag = find_child_by_kind_html(node, "start_tag")
            .or_else(|| find_child_by_kind_html(node, "self_closing_tag"));
        if let Some(st) = start_tag {
            if let Some(id) = find_attribute_value_html(st, "id", source) {
                *name = format!("{tag}#{id}");
                *chunk_type = ChunkType::Property;
                return true;
            }
        }
        return false; // Filter out
    }

    true
}

#[cfg(feature = "lang-vue")]
static LANG_VUE: LanguageDef = LanguageDef {
    name: "vue",
    grammar: Some(|| tree_sitter_vue::LANGUAGE.into()),
    extensions: &["vue"],
    chunk_query: include_str!("queries/vue.chunks.scm"),
    signature_style: SignatureStyle::Breadcrumb,
    doc_nodes: &["comment"],
    stopwords: &[
        "div",
        "span",
        "p",
        "a",
        "img",
        "ul",
        "ol",
        "li",
        "table",
        "tr",
        "td",
        "th",
        "form",
        "input",
        "button",
        "label",
        "select",
        "option",
        "textarea",
        "br",
        "hr",
        "head",
        "body",
        "html",
        "meta",
        "link",
        "title",
        "script",
        "style",
        "class",
        "id",
        "href",
        "src",
        "alt",
        "type",
        "value",
        "name",
        "slot",
        "template",
        "component",
        "transition",
        "keep",
        "alive",
        "teleport",
        "suspense",
        "v-if",
        "v-else",
        "v-for",
        "v-show",
        "v-bind",
        "v-on",
        "v-model",
        "v-slot",
        "v-html",
        "const",
        "let",
        "var",
        "export",
        "import",
        "default",
        "ref",
        "reactive",
        "computed",
        "watch",
        "defineProps",
        "defineEmits",
        "defineExpose",
        "withDefaults",
    ],
    post_process_chunk: Some(post_process_vue_vue),
    injections: &[
        // <script> blocks → JavaScript (or TypeScript via detect_script_language)
        InjectionRule {
            container_kind: "script_element",
            content_kind: "raw_text",
            target_language: "javascript",
            detect_language: Some(detect_script_language_html),
            content_scoped_lines: false,
        },
        // <style> blocks → CSS
        InjectionRule {
            container_kind: "style_element",
            content_kind: "raw_text",
            target_language: "css",
            detect_language: None,
            content_scoped_lines: false,
        },
    ],
    ..DEFAULTS
};

pub fn definition_vue() -> &'static LanguageDef {
    &LANG_VUE
}

// ============================================================================
// Xml (xml)
// ============================================================================

/// Extracts the return type from a function signature.
/// # Arguments
/// * `_signature` - A string slice containing a function signature (unused for XML)
/// # Returns
/// Returns `None` as XML has no concept of functions or return types.
fn extract_return_xml(_signature: &str) -> Option<String> {
    // XML has no functions or return types
    None
}

/// Post-process XML chunks: only keep top-level elements (direct children of root).
fn post_process_xml_xml(
    _name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Processing instructions are always kept
    if node.kind() == "PI" {
        return true;
    }
    // element > document (depth 1) or element > content > element > document (depth 2)
    if let Some(parent) = node.parent() {
        let pk = parent.kind();
        if pk == "document" {
            return true;
        }
        // Depth 2: element inside root element's content
        if pk == "content" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "element" {
                    if let Some(ggp) = grandparent.parent() {
                        return ggp.kind() == "document";
                    }
                }
            }
        }
    }
    false
}

static LANG_XML: LanguageDef = LanguageDef {
    name: "xml",
    grammar: Some(|| tree_sitter_xml::LANGUAGE_XML.into()),
    extensions: &[
        "xml", "xsl", "xslt", "xsd", "svg", "wsdl", "rss", "plist", "l5x", "l5k",
    ],
    chunk_query: include_str!("queries/xml.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["Comment"],
    stopwords: &[
        "xml",
        "xmlns",
        "version",
        "encoding",
        "standalone",
        "xsi",
        "xsd",
        "type",
        "name",
        "value",
    ],
    extract_return_nl: extract_return_xml,
    post_process_chunk: Some(post_process_xml_xml),
    ..DEFAULTS
};

pub fn definition_xml() -> &'static LanguageDef {
    &LANG_XML
}

// ============================================================================
// Yaml (yaml)
// ============================================================================

/// Extracts the return type from a function signature.
/// # Arguments
/// * `_signature` - A function signature string to parse (unused for YAML as it has no function types)
/// # Returns
/// Returns `None` as YAML does not support function signatures or return type annotations.
fn extract_return_yaml(_signature: &str) -> Option<String> {
    // YAML has no functions or return types
    None
}

/// Post-process YAML chunks: only keep top-level keys (depth 1).
/// Nested keys within mappings are too granular.
fn post_process_yaml_yaml(
    _name: &mut String,
    _chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Only keep top-level mapping pairs (parent is block_mapping, grandparent is stream/document)
    if let Some(parent) = node.parent() {
        if let Some(grandparent) = parent.parent() {
            let gp_kind = grandparent.kind();
            // Top-level: stream > document > block_node > block_mapping > block_mapping_pair
            // or: stream > block_mapping > block_mapping_pair
            return gp_kind == "stream"
                || gp_kind == "document"
                || grandparent
                    .parent()
                    .is_some_and(|ggp| ggp.kind() == "stream" || ggp.kind() == "document");
        }
    }
    true
}

static LANG_YAML: LanguageDef = LanguageDef {
    name: "yaml",
    grammar: Some(|| tree_sitter_yaml::LANGUAGE.into()),
    extensions: &["yaml", "yml"],
    chunk_query: include_str!("queries/yaml.chunks.scm"),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: &["comment"],
    stopwords: &["true", "false", "null", "yes", "no", "on", "off"],
    extract_return_nl: extract_return_yaml,
    post_process_chunk: Some(post_process_yaml_yaml),
    ..DEFAULTS
};

pub fn definition_yaml() -> &'static LanguageDef {
    &LANG_YAML
}

// ============================================================================
// Zig (zig)
// ============================================================================

/// Post-process Zig chunks: reclassify variable_declaration to correct type,
/// discard non-container variable declarations, and clean test names.
fn post_process_zig_zig(
    name: &mut String,
    chunk_type: &mut ChunkType,
    node: tree_sitter::Node,
    source: &str,
) -> bool {
    let kind = node.kind();

    if kind == "test_declaration" {
        // Extract test name from string child or identifier child
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                if child.kind() == "string" || child.kind() == "identifier" {
                    let text = &source[child.start_byte()..child.end_byte()];
                    // Strip quotes from string literals
                    let clean = text.trim_matches('"');
                    *name = clean.to_string();
                    return true;
                }
            }
        }
        *name = "anonymous_test".to_string();
        return true;
    }

    if kind == "variable_declaration" {
        let text = &source[node.start_byte()..node.end_byte()];
        if text.contains("struct") {
            *chunk_type = ChunkType::Struct;
        } else if text.contains("enum") {
            *chunk_type = ChunkType::Enum;
        } else if text.contains("union") {
            *chunk_type = ChunkType::TypeAlias;
        } else if text.contains("error{") || text.contains("error {") {
            *chunk_type = ChunkType::Enum;
        } else {
            // Regular variable — not a significant definition
            return false;
        }
    }

    true
}

/// Extracts and formats the return type from a Zig function signature.
/// Parses a Zig function signature to locate the return type between the closing parenthesis and opening brace. Strips error union syntax (leading `!`) and filters out void and noreturn types. Returns a formatted string describing the return type.
/// # Arguments
/// * `signature` - A Zig function signature string (e.g., `fn name_zig(params) ReturnType { ... }`)
/// # Returns
/// `Some(String)` containing a formatted description like "Returns TypeName" if a valid return type is found, or `None` if the signature has no return type, returns void/noreturn, or contains only whitespace.
fn extract_return_zig(signature: &str) -> Option<String> {
    // Zig: fn name_zig(params) ReturnType { ... }
    // Look for ) followed by a type before {
    let paren_pos = signature.rfind(')')?;
    let after_paren = &signature[paren_pos + 1..];
    let brace_pos = after_paren.find('{').unwrap_or(after_paren.len());
    let ret_part = after_paren[..brace_pos].trim();
    if ret_part.is_empty() || ret_part == "void" || ret_part == "noreturn" || ret_part == "anytype"
    {
        return None;
    }
    // Strip error union: !Type → Type
    let ret_type = ret_part.strip_prefix('!').unwrap_or(ret_part).trim();
    if ret_type.is_empty() || ret_type == "void" {
        return None;
    }
    let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
    if ret_words.is_empty() {
        return None;
    }
    Some(format!("Returns {}", ret_words))
}

static LANG_ZIG: LanguageDef = LanguageDef {
    name: "zig",
    grammar: Some(|| tree_sitter_zig::LANGUAGE.into()),
    extensions: &["zig"],
    chunk_query: include_str!("queries/zig.chunks.scm"),
    call_query: Some(include_str!("queries/zig.calls.scm")),
    doc_nodes: &["doc_comment", "line_comment"],
    method_containers: &[
        "struct_declaration",
        "enum_declaration",
        "union_declaration",
    ],
    stopwords: &[
        "fn",
        "pub",
        "const",
        "var",
        "return",
        "if",
        "else",
        "for",
        "while",
        "break",
        "continue",
        "switch",
        "unreachable",
        "undefined",
        "null",
        "true",
        "false",
        "and",
        "or",
        "try",
        "catch",
        "comptime",
        "inline",
        "extern",
        "export",
        "struct",
        "enum",
        "union",
        "error",
        "test",
        "defer",
        "errdefer",
        "async",
        "await",
        "suspend",
        "resume",
        "nosuspend",
        "orelse",
        "anytype",
        "anyframe",
        "void",
        "noreturn",
        "type",
        "usize",
        "isize",
        "bool",
    ],
    extract_return_nl: extract_return_zig,
    type_query: Some(include_str!("queries/zig.types.scm")),
    common_types: &[
        "void",
        "noreturn",
        "bool",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f16",
        "f32",
        "f64",
        "f128",
        "anytype",
        "anyframe",
        "type",
        "anyerror",
        "anyopaque",
    ],
    container_body_kinds: &[
        "struct_declaration",
        "enum_declaration",
        "union_declaration",
    ],
    post_process_chunk: Some(post_process_zig_zig),
    test_markers: &["test "],
    test_path_patterns: &["%/tests/%", "%_test.zig"],
    entry_point_names: &["main"],
    doc_convention: "Use /// doc comments describing parameters and return values.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "pub",
    },
    skip_line_prefixes: &["const ", "pub const"],
    ..DEFAULTS
};

pub fn definition_zig() -> &'static LanguageDef {
    &LANG_ZIG
}

// ============================================================================
// Aspx (aspx)
// ============================================================================

static LANG_ASPX: LanguageDef = LanguageDef {
    name: "aspx",
    grammar: None, // Custom parser — delegates to C#/VB.NET grammars
    extensions: &["aspx", "ascx", "asmx", "master"],
    signature_style: SignatureStyle::FirstLine,
    stopwords: &[
        "page",
        "control",
        "master",
        "runat",
        "server",
        "autopostback",
        "viewstate",
        "postback",
        "handler",
        "event",
        "sender",
        "eventargs",
        "codebehind",
        "inherits",
        "aspx",
        "ascx",
        "asmx",
    ],
    entry_point_names: &["Page_Load", "Page_Init", "Page_PreRender"],
    ..DEFAULTS
};

pub fn definition_aspx() -> &'static LanguageDef {
    &LANG_ASPX
}
