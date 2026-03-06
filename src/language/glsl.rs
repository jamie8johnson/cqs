//! GLSL language definition
//!
//! GLSL extends C grammar. Reuses C chunk and call queries.
//! Uses `LANGUAGE_GLSL` (non-standard export, like OCaml's `LANGUAGE_OCAML`).

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting GLSL code chunks (reuses C patterns)
const CHUNK_QUERY: &str = r#"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

(enum_specifier
  name: (type_identifier) @name
  body: (enumerator_list)) @enum

(type_definition
  declarator: (type_identifier) @name) @typealias

(declaration
  declarator: (init_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @function

;; Union definitions
(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list)) @struct

;; Preprocessor constants (#define FOO 42)
(preproc_def
  name: (identifier) @name) @const

;; Preprocessor function macros (#define FOO(x) ...)
(preproc_function_def
  name: (identifier) @name) @macro
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (field_identifier) @callee))
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    // C stopwords
    "if", "else", "for", "while", "do", "switch", "case", "break", "continue", "return",
    "typedef", "struct", "enum", "union", "void", "int", "char", "float", "double",
    "const", "static", "sizeof", "true", "false",
    // GLSL-specific qualifiers and types
    "uniform", "varying", "attribute", "in", "out", "inout", "flat", "smooth",
    "noperspective", "centroid", "sample", "patch",
    "layout", "location", "binding", "set", "push_constant",
    "precision", "lowp", "mediump", "highp",
    "vec2", "vec3", "vec4", "ivec2", "ivec3", "ivec4",
    "uvec2", "uvec3", "uvec4", "bvec2", "bvec3", "bvec4",
    "mat2", "mat3", "mat4", "mat2x3", "mat3x4",
    "sampler2D", "sampler3D", "samplerCube", "sampler2DShadow",
    "texture", "discard", "gl_Position", "gl_FragColor",
];

fn extract_return(signature: &str) -> Option<String> {
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
                        "static" | "inline" | "const" | "volatile"
                            | "highp" | "mediump" | "lowp"
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
    name: "glsl",
    grammar: Some(|| tree_sitter_glsl::LANGUAGE_GLSL.into()),
    extensions: &["glsl", "vert", "frag", "geom", "comp", "tesc", "tese"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: None,
    type_query: None,
    common_types: &[
        "int", "float", "double", "void", "bool",
        "vec2", "vec3", "vec4", "ivec2", "ivec3", "ivec4",
        "uvec2", "uvec3", "uvec4", "bvec2", "bvec3", "bvec4",
        "mat2", "mat3", "mat4", "mat2x3", "mat2x4", "mat3x2", "mat3x4", "mat4x2", "mat4x3",
        "sampler2D", "sampler3D", "samplerCube", "sampler2DShadow",
    ],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &[],
    test_path_patterns: &[],
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
    fn parse_glsl_vertex_shader() {
        let content = r#"
#version 450

layout(location = 0) in vec3 aPosition;
layout(location = 1) in vec2 aTexCoord;

layout(location = 0) out vec2 vTexCoord;

uniform mat4 uModelViewProjection;

void main() {
    gl_Position = uModelViewProjection * vec4(aPosition, 1.0);
    vTexCoord = aTexCoord;
}
"#;
        let file = write_temp_file(content, "vert");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let main_fn = chunks.iter().find(|c| c.name == "main").unwrap();
        assert_eq!(main_fn.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_glsl_struct() {
        let content = r#"
struct Light {
    vec3 position;
    vec3 color;
    float intensity;
};
"#;
        let file = write_temp_file(content, "glsl");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let s = chunks.iter().find(|c| c.name == "Light").unwrap();
        assert_eq!(s.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn parse_glsl_calls() {
        let content = r#"
vec4 applyLighting(vec3 normal, vec3 lightDir) {
    float diff = max(dot(normal, lightDir), 0.0);
    vec3 color = mix(ambient, diffuse, diff);
    return vec4(normalize(color), 1.0);
}
"#;
        let file = write_temp_file(content, "frag");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "applyLighting").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"max"), "Expected max, got: {:?}", names);
        assert!(names.contains(&"dot"), "Expected dot, got: {:?}", names);
        assert!(names.contains(&"mix"), "Expected mix, got: {:?}", names);
        assert!(names.contains(&"normalize"), "Expected normalize, got: {:?}", names);
    }

    #[test]
    fn test_extract_return_glsl() {
        assert_eq!(
            extract_return("vec4 applyLighting(vec3 normal)"),
            Some("Returns vec4".to_string())
        );
        assert_eq!(extract_return("void main()"), None);
    }
}
