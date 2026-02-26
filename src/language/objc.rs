//! Objective-C language definition

use super::{LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Objective-C code chunks
const CHUNK_QUERY: &str = r#"
;; Class interfaces (@interface ... @end)
(class_interface
  (identifier) @name) @class

;; Protocols (@protocol ... @end)
(protocol_declaration
  (identifier) @name) @interface

;; Method declarations (in @interface or @protocol — no body)
(method_declaration
  (identifier) @name) @function

;; Method definitions (in @implementation — with body)
(method_definition
  (identifier) @name) @function

;; C-style free functions
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

;; Properties with pointer types (@property NSString *name)
(property_declaration
  (struct_declaration
    (struct_declarator
      (pointer_declarator
        (identifier) @name)))) @property

;; Properties with value types (@property NSInteger age)
(property_declaration
  (struct_declaration
    (struct_declarator
      (identifier) @name))) @property
"#;

/// Tree-sitter query for extracting function calls
const CALL_QUERY: &str = r#"
;; Objective-C message sends [receiver method]
(message_expression
  (identifier) @callee)

;; C function calls
(call_expression
  function: (identifier) @callee)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "self", "super", "nil", "NULL", "YES", "NO", "true", "false", "if", "else", "for", "while",
    "do", "switch", "case", "break", "continue", "return", "void", "int", "float", "double",
    "char", "long", "short", "unsigned", "signed", "static", "extern", "const", "typedef",
    "struct", "enum", "union", "id", "Class", "SEL", "IMP", "BOOL", "NSObject", "NSString",
    "NSInteger", "NSUInteger", "CGFloat", "nonatomic", "strong", "weak", "copy", "assign",
    "readonly", "readwrite", "atomic", "property", "synthesize", "dynamic", "interface",
    "implementation", "protocol", "end", "optional", "required", "import", "include",
];

fn extract_return(_signature: &str) -> Option<String> {
    // ObjC methods use `- (ReturnType)methodName` syntax which doesn't lend itself
    // to simple text-based extraction. Return None.
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "objc",
    grammar: Some(|| tree_sitter_objc::LANGUAGE.into()),
    extensions: &["m", "mm"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_interface", "implementation_definition", "protocol_declaration"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.m")),
    type_query: None,
    common_types: &[],
    container_body_kinds: &["implementation_definition"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
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

    #[test]
    fn parse_objc_class_interface() {
        let content = r#"
@interface Person : NSObject
@property (nonatomic) NSString *name;
- (void)greet;
@end
"#;
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Person").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_objc_protocol() {
        let content = r#"
@protocol Drawable
- (void)draw;
@end
"#;
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let proto = chunks.iter().find(|c| c.name == "Drawable").unwrap();
        assert_eq!(proto.chunk_type, ChunkType::Interface);
    }

    #[test]
    fn parse_objc_method_declaration() {
        let content = r#"
@interface Calculator : NSObject
- (int)add:(int)a to:(int)b;
+ (Calculator *)shared;
@end
"#;
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        let class_method = chunks.iter().find(|c| c.name == "shared").unwrap();
        assert_eq!(class_method.chunk_type, ChunkType::Method);
    }

    #[test]
    fn parse_objc_method_definition() {
        let content = r#"
@implementation Person

- (void)greet {
    NSLog(@"Hello, %@", self.name);
}

@end
"#;
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }

    #[test]
    fn parse_objc_free_function() {
        let content = "void freeFunc(int x) { }\n";
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "freeFunc").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_objc_property() {
        let content = r#"
@interface Config : NSObject
@property (nonatomic, copy) NSString *name;
@property (nonatomic) NSInteger count;
@end
"#;
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ptr_prop = chunks.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(ptr_prop.chunk_type, ChunkType::Property);
        let val_prop = chunks.iter().find(|c| c.name == "count").unwrap();
        assert_eq!(val_prop.chunk_type, ChunkType::Property);
    }

    #[test]
    fn parse_objc_calls() {
        let content = r#"
@implementation Runner

- (void)run {
    [self greet];
    NSLog(@"done");
    free(ptr);
}

@end
"#;
        let parser = Parser::new().unwrap();
        let lang = crate::parser::Language::ObjC;
        let calls = parser.extract_calls(content, lang, 0, content.len(), 0);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        // Message sends
        assert!(
            names.contains(&"greet"),
            "Expected greet call, got: {:?}",
            names
        );
        // C function calls
        assert!(
            names.contains(&"free"),
            "Expected free call, got: {:?}",
            names
        );
    }
}
