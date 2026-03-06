//! CUDA language definition
//!
//! CUDA extends C++ grammar with kernel launch syntax and device qualifiers.
//! Reuses C++ chunk and call queries — all C++ node types are present.

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting CUDA code chunks (reuses C++ patterns)
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

;; Forward declarations with function body
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

/// Tree-sitter query for extracting function calls (reuses C++ patterns)
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

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    // C++ stopwords
    "if", "else", "for", "while", "do", "switch", "case", "break", "continue", "return",
    "class", "struct", "enum", "namespace", "template", "typename", "using", "typedef",
    "virtual", "override", "final", "const", "static", "inline", "explicit", "extern", "friend",
    "public", "private", "protected", "void", "int", "char", "float", "double", "long", "short",
    "unsigned", "signed", "auto", "new", "delete", "this", "true", "false", "nullptr", "sizeof",
    // CUDA-specific qualifiers
    "__global__", "__device__", "__host__", "__shared__", "__constant__",
    "__managed__", "__restrict__", "__noinline__", "__forceinline__",
    "dim3", "blockIdx", "threadIdx", "blockDim", "gridDim", "warpSize",
    "cudaMalloc", "cudaFree", "cudaMemcpy",
];

fn extract_return(signature: &str) -> Option<String> {
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
fn extract_qualified_method(node: tree_sitter::Node, source: &str) -> Option<String> {
    let func_decl = node.child_by_field_name("declarator")?;
    let inner_decl = func_decl.child_by_field_name("declarator")?;
    if inner_decl.kind() != "qualified_identifier" {
        return None;
    }
    let scope = inner_decl.child_by_field_name("scope")?;
    Some(source[scope.byte_range()].to_string())
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "cuda",
    grammar: Some(|| tree_sitter_cuda::LANGUAGE.into()),
    extensions: &["cu", "cuh"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_specifier", "struct_specifier"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    type_query: None,
    common_types: &[
        "int", "char", "float", "double", "void", "long", "short", "unsigned", "size_t",
        "dim3", "cudaError_t", "cudaStream_t", "cudaEvent_t",
        "float2", "float3", "float4", "int2", "int3", "int4",
        "uint2", "uint3", "uint4", "half", "__half", "__half2",
    ],
    container_body_kinds: &["field_declaration_list"],
    extract_container_name: None,
    extract_qualified_method: Some(extract_qualified_method),
    post_process_chunk: None,
    test_markers: &["TEST(", "TEST_F(", "EXPECT_", "ASSERT_"],
    test_path_patterns: &["%/tests/%", "%\\_test.cu"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[],
    injections: &[],
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
    fn parse_cuda_kernel() {
        let content = r#"
__global__ void vectorAdd(float *a, float *b, float *c, int n) {
    int idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < n) {
        c[idx] = a[idx] + b[idx];
    }
}
"#;
        let file = write_temp_file(content, "cu");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let kernel = chunks.iter().find(|c| c.name == "vectorAdd").unwrap();
        assert_eq!(kernel.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_cuda_struct() {
        let content = r#"
struct DeviceConfig {
    int numBlocks;
    int threadsPerBlock;
    cudaStream_t stream;
};
"#;
        let file = write_temp_file(content, "cu");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "DeviceConfig").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_cuda_calls() {
        let content = r#"
void launch() {
    float *d_a;
    cudaMalloc(&d_a, size);
    vectorAdd<<<numBlocks, blockSize>>>(d_a, d_b, d_c, n);
    cudaDeviceSynchronize();
    cudaFree(d_a);
}
"#;
        let file = write_temp_file(content, "cu");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "launch").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"cudaMalloc"), "Expected cudaMalloc, got: {:?}", names);
        assert!(names.contains(&"cudaFree"), "Expected cudaFree, got: {:?}", names);
    }

    #[test]
    fn test_extract_return_cuda() {
        assert_eq!(
            extract_return("__global__ void vectorAdd(float *a)"),
            None
        );
        assert_eq!(
            extract_return("__device__ float computeForce(float mass)"),
            Some("Returns float".to_string())
        );
    }
}
