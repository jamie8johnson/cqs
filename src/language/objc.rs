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

/// Extracts the return type from a function signature.
/// 
/// Currently returns `None` for all inputs as Objective-C method signatures use `- (ReturnType)methodName` syntax that is not amenable to simple text-based extraction.
/// 
/// # Arguments
/// 
/// * `_signature` - A function signature string to parse
/// 
/// # Returns
/// 
/// `None` in all cases, as return type extraction is not yet implemented.
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
    test_markers: &["- (void)test"],
    test_path_patterns: &["%/Tests/%", "%Tests.m"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "init", "dealloc", "description", "hash", "isEqual",
        "copyWithZone", "encodeWithCoder", "initWithCoder",
    ],
    injections: &[],
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
    /// Parses an Objective-C class interface definition and verifies the resulting chunk metadata.
    /// 
    /// This test function creates a temporary Objective-C file containing a `Person` class interface with a property and method, parses it using the `Parser`, and asserts that the parser correctly identifies the class chunk with the expected type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser initialization fails
    /// - File parsing fails
    /// - No chunk named "Person" is found in the parsed results
    /// - The identified chunk's type is not `ChunkType::Class`

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
    /// Parses an Objective-C protocol definition and verifies it is correctly identified as an interface chunk.
    /// 
    /// # Arguments
    /// 
    /// This function takes no arguments.
    /// 
    /// # Returns
    /// 
    /// Returns nothing; this is a test function that asserts correct parsing behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the "Drawable" protocol is not found in parsed chunks, or the chunk type is not `ChunkType::Interface`.

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
    /// Parses an Objective-C file and verifies that both instance and class methods are correctly identified.
    /// 
    /// This test function writes a temporary Objective-C interface file containing instance and class method declarations, parses it using the Parser, and asserts that both methods are recognized with the correct chunk type.
    /// 
    /// # Arguments
    /// 
    /// None - this is a test function.
    /// 
    /// # Returns
    /// 
    /// None - performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize or parse the file, or if the expected methods are not found or have incorrect chunk types.

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
    /// Parses an Objective-C method definition and verifies correct extraction.
    /// 
    /// This test function creates a temporary Objective-C file containing a simple method implementation, parses it, and asserts that the method is correctly identified and classified as a Method chunk type.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded content.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on test failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to initialize, file parsing fails, the "greet" method is not found in parsed chunks, or the method's chunk_type is not ChunkType::Method.

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
    /// Parses an Objective-C free function and verifies it is correctly identified as a Function chunk type.
    /// 
    /// This test function creates a temporary Objective-C file containing a simple free function, parses it using the Parser, and asserts that the resulting chunk is properly recognized as a Function with the correct name.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses internal test utilities.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, parsing fails, the "freeFunc" chunk is not found in the parsed results, or the chunk type assertion fails.

    #[test]
    fn parse_objc_free_function() {
        let content = "void freeFunc(int x) { }\n";
        let file = write_temp_file(content, "m");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "freeFunc").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }
    /// Parses Objective-C property declarations and verifies they are correctly identified as Property chunk types.
    /// 
    /// This is a test function that validates the parser's ability to recognize and extract both pointer-based properties (with copy semantics) and value-based properties from an Objective-C interface definition.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None (unit type). The function asserts on parsing results and panics if assertions fail.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to create, the file fails to parse, or if expected properties named "name" or "count" are not found in the parsed chunks, or if they are not identified as Property chunk types.

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
    /// Parses Objective-C source code and extracts function and method calls, verifying that both message sends and C function calls are correctly identified.
    /// 
    /// This is a test function that validates the parser's ability to detect calls within Objective-C code, including instance method invocations (e.g., `[self greet]`) and standard C function calls (e.g., `free(ptr)`).
    /// 
    /// # Arguments
    /// 
    /// None. This function uses hardcoded Objective-C source content for testing purposes.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the expected method calls ("greet" and "free") are not found in the extracted calls, indicating a failure in the parser's call extraction logic.

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
