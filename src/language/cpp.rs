//! C++ language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting C++ code chunks
const CHUNK_QUERY: &str = r#"
;; Free functions
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

;; Inline methods (field_identifier inside class body)
(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @name)) @function

;; Out-of-class methods (Class::method)
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @name))) @function

;; Destructors (inline)
(function_definition
  declarator: (function_declarator
    declarator: (destructor_name) @name)) @function

;; Destructors (out-of-class, Class::~Class)
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (destructor_name) @name))) @function

;; Forward declarations with function body (rare)
(declaration
  declarator: (init_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @function

;; Classes
(class_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @class

;; Structs
(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Enums (including enum class)
(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @enum

;; Namespaces
(namespace_definition
  name: (namespace_identifier) @name) @module

;; Concepts (C++20)
(concept_definition
  name: (identifier) @name) @trait

;; Type aliases — using X = Y (C++11)
(alias_declaration
  name: (type_identifier) @name) @typealias

;; Typedefs (C-style)
(type_definition
  declarator: (type_identifier) @name) @typealias

;; Unions
(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Preprocessor constants
(preproc_def
  name: (identifier) @name) @const

;; Preprocessor function macros
(preproc_function_def
  name: (identifier) @name) @macro
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
;; Direct function call
(call_expression
  function: (identifier) @callee)

;; Qualified call (Class::method or ns::func)
(call_expression
  function: (qualified_identifier
    name: (identifier) @callee))

;; Member call (obj.method or ptr->method)
(call_expression
  function: (field_expression
    field: (field_identifier) @callee))

;; Template function call (make_shared<T>())
(call_expression
  function: (template_function
    name: (identifier) @callee))

;; Qualified template call (std::make_shared<T>())
(call_expression
  function: (qualified_identifier
    name: (template_function
      name: (identifier) @callee)))

;; new expression
(new_expression
  type: (type_identifier) @callee)
"#;

/// Tree-sitter query for extracting type references
const TYPE_QUERY: &str = r#"
;; Parameter types
(parameter_declaration type: (type_identifier) @param_type)
(parameter_declaration type: (qualified_identifier name: (type_identifier) @param_type))

;; Return types
(function_definition type: (type_identifier) @return_type)
(function_definition type: (qualified_identifier name: (type_identifier) @return_type))

;; Field types
(field_declaration type: (type_identifier) @field_type)
(field_declaration type: (qualified_identifier name: (type_identifier) @field_type))

;; Base class / inheritance
(base_class_clause (type_identifier) @impl_type)
(base_class_clause (qualified_identifier name: (type_identifier) @impl_type))
(base_class_clause (template_type name: (type_identifier) @impl_type))

;; Template arguments
(template_argument_list (type_identifier) @type_ref)

;; Using alias source type (alias_declaration wraps in type_descriptor)
(alias_declaration type: (type_descriptor type: (type_identifier) @alias_type))

;; Catch-all
(type_identifier) @type_ref
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "case", "break", "continue", "return",
    "class", "struct", "enum", "namespace", "template", "typename", "using", "typedef",
    "virtual", "override", "final", "const", "static", "inline", "explicit", "extern", "friend",
    "public", "private", "protected", "void", "int", "char", "float", "double", "long", "short",
    "unsigned", "signed", "auto", "new", "delete", "this", "true", "false", "nullptr", "sizeof",
    "dynamic_cast", "static_cast", "reinterpret_cast", "const_cast", "throw", "try", "catch",
    "noexcept", "operator", "concept", "requires", "constexpr", "consteval", "constinit",
    "mutable", "volatile", "co_await", "co_yield", "co_return", "decltype",
];

const COMMON_TYPES: &[&str] = &[
    "string", "wstring", "string_view", "vector", "map", "unordered_map", "set", "unordered_set",
    "multimap", "multiset", "list", "deque", "array", "forward_list", "pair", "tuple", "optional",
    "variant", "any", "expected", "shared_ptr", "unique_ptr", "weak_ptr", "function", "size_t",
    "ptrdiff_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t",
    "uint64_t", "nullptr_t", "span", "basic_string", "iterator", "const_iterator",
    "reverse_iterator", "ostream", "istream", "iostream", "fstream", "ifstream", "ofstream",
    "stringstream", "istringstream", "ostringstream", "thread", "mutex", "recursive_mutex",
    "condition_variable", "atomic", "future", "promise", "exception", "runtime_error",
    "logic_error", "invalid_argument", "out_of_range", "overflow_error", "bad_alloc", "type_info",
    "initializer_list", "allocator", "hash", "equal_to", "less", "greater", "reference_wrapper",
    "bitset", "complex", "regex", "chrono",
];

/// Extract parent type from a function's own declarator.
/// For out-of-class methods: `void MyClass::method()` → Some("MyClass").
fn extract_qualified_method(node: tree_sitter::Node, source: &str) -> Option<String> {
    // function_definition > declarator: function_declarator > declarator: qualified_identifier
    let func_decl = node.child_by_field_name("declarator")?;
    let inner_decl = func_decl.child_by_field_name("declarator")?;
    if inner_decl.kind() != "qualified_identifier" {
        return None;
    }
    let scope = inner_decl.child_by_field_name("scope")?;
    Some(source[scope.byte_range()].to_string())
}

fn extract_return(signature: &str) -> Option<String> {
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

static DEFINITION: LanguageDef = LanguageDef {
    name: "cpp",
    grammar: Some(|| tree_sitter_cpp::LANGUAGE.into()),
    extensions: &["cpp", "cxx", "cc", "hpp", "hxx", "hh", "ipp"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_specifier", "struct_specifier"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/tests/{stem}_test.cpp")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["field_declaration_list"],
    extract_container_name: None,
    extract_qualified_method: Some(extract_qualified_method),
    post_process_chunk: None,
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
    fn test_extract_return_cpp() {
        // Prefix return type
        assert_eq!(
            extract_return("int add(int a, int b)"),
            Some("Returns int".to_string())
        );
        // Trailing return type
        assert_eq!(
            extract_return("auto add(int a, int b) -> int"),
            Some("Returns int".to_string())
        );
        // auto deduction (no ->)
        assert_eq!(extract_return("auto foo()"), None);
        // void
        assert_eq!(extract_return("void doSomething()"), None);
        // With specifiers
        assert_eq!(
            extract_return("static inline int getValue()"),
            Some("Returns int".to_string())
        );
        // virtual with qualified type (tokenize_identifier preserves ::)
        assert_eq!(
            extract_return("virtual std::string getName()"),
            Some("Returns std::string".to_string())
        );
    }

    #[test]
    fn parse_cpp_free_function() {
        let content = "void foo() {\n  // body\n}\n";
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "foo").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
        assert!(func.parent_type_name.is_none());
    }

    #[test]
    fn parse_cpp_class() {
        let content = r#"
class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_cpp_struct() {
        let content = "struct Point {\n  double x;\n  double y;\n};\n";
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "Point").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_cpp_namespace() {
        let content = r#"
namespace utils {
    void helper() {}
}
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ns = chunks.iter().find(|c| c.name == "utils").unwrap();
        assert_eq!(ns.chunk_type, ChunkType::Module);
    }

    #[test]
    fn parse_cpp_concept() {
        let content = r#"
template<typename T>
concept Printable = requires(T t) {
    { t.print() } -> std::same_as<void>;
};
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let concept = chunks.iter().find(|c| c.name == "Printable").unwrap();
        assert_eq!(concept.chunk_type, ChunkType::Trait);
    }

    #[test]
    fn parse_cpp_using_alias() {
        let content = "using StringVec = std::vector<std::string>;\n";
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "StringVec").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_cpp_typedef() {
        let content = "typedef unsigned long size_type;\n";
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "size_type").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_cpp_method_in_class() {
        let content = r#"
class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| format!("{}:{:?}", c.name, c.chunk_type)).collect();
        let method = chunks.iter().find(|c| c.name == "add").unwrap_or_else(|| panic!("Expected 'add', found: {:?}", names));
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }

    #[test]
    fn parse_cpp_out_of_class_method() {
        let content = r#"
class Foo {
public:
    void bar();
};

void Foo::bar() {
    // implementation
}
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        // Find the out-of-class definition (the one with a body)
        let methods: Vec<_> = chunks.iter().filter(|c| c.name == "bar").collect();
        let impl_method = methods.iter().find(|c| c.chunk_type == ChunkType::Method);
        assert!(impl_method.is_some(), "Expected out-of-class method, got: {:?}", methods.iter().map(|c| (&c.name, &c.chunk_type)).collect::<Vec<_>>());
        assert_eq!(impl_method.unwrap().parent_type_name.as_deref(), Some("Foo"));
    }

    #[test]
    fn parse_cpp_destructor_inline() {
        let content = r#"
class Resource {
public:
    ~Resource() {
        cleanup();
    }
};
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let dtor = chunks.iter().find(|c| c.name.contains("Resource") && c.name.contains("~"));
        assert!(dtor.is_some(), "Expected destructor, got: {:?}", chunks.iter().map(|c| &c.name).collect::<Vec<_>>());
        // Destructor inside class body should be Method
        assert_eq!(dtor.unwrap().chunk_type, ChunkType::Method);
    }

    #[test]
    fn parse_cpp_destructor_out_of_class() {
        let content = r#"
class Foo {
public:
    ~Foo();
};

Foo::~Foo() {
    // cleanup
}
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let dtors: Vec<_> = chunks.iter().filter(|c| c.name.contains("~")).collect();
        assert!(!dtors.is_empty(), "Expected destructor, got: {:?}", chunks.iter().map(|c| &c.name).collect::<Vec<_>>());
    }

    #[test]
    fn parse_cpp_enum_class() {
        let content = "enum class Color { Red, Green, Blue };\n";
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks.iter().find(|c| c.name == "Color").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }

    #[test]
    fn parse_cpp_union() {
        let content = "union Data {\n  int i;\n  float f;\n};\n";
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let u = chunks.iter().find(|c| c.name == "Data").unwrap();
        assert_eq!(u.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_cpp_template_class() {
        let content = r#"
template<typename T>
class Container {
public:
    void add(T item) {}
};
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Container").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_cpp_calls() {
        let content = r#"
void process() {
    auto x = transform(input);
    obj.method();
    ptr->cleanup();
    auto p = std::make_shared<Foo>(42);
    auto w = new Widget();
}
"#;
        let file = write_temp_file(content, "cpp");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"transform"), "Expected transform, got: {:?}", names);
        assert!(names.contains(&"method"), "Expected method, got: {:?}", names);
        assert!(names.contains(&"cleanup"), "Expected cleanup, got: {:?}", names);
    }
}
