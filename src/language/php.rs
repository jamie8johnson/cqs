//! PHP language definition

use super::{ChunkType, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting PHP code chunks.
///
/// Classes → Class, Interfaces → Interface, Traits → Trait, Enums → Enum,
/// Functions → Function, Methods → Function (reclassified via method_containers),
/// Constants → Constant, Properties → Property.
const CHUNK_QUERY: &str = r#"
;; Functions
(function_definition
  name: (name) @name) @function

;; Classes
(class_declaration
  name: (name) @name) @class

;; Interfaces
(interface_declaration
  name: (name) @name) @interface

;; Traits
(trait_declaration
  name: (name) @name) @trait

;; Enums (PHP 8.1+)
(enum_declaration
  name: (name) @name) @enum

;; Methods (reclassified to Method via method_containers when inside declaration_list)
(method_declaration
  name: (name) @name) @function

;; Constants
(const_declaration
  (const_element
    (name) @name)) @const

;; Properties
(property_declaration
  (property_element
    (variable_name) @name)) @property
"#;

/// Tree-sitter query for extracting PHP function calls.
const CALL_QUERY: &str = r#"
;; Regular function calls
(function_call_expression
  function: (name) @callee)

;; Method calls ($obj->method())
(member_call_expression
  name: (name) @callee)

;; Static calls (Class::method())
(scoped_call_expression
  name: (name) @callee)

;; Constructor calls (new ClassName)
(object_creation_expression
  (name) @callee)
"#;

/// Tree-sitter query for extracting PHP type references.
const TYPE_QUERY: &str = r#"
;; Parameter types (function foo(Type $param))
(simple_parameter
  type: (named_type (name) @param_type))

;; Return types (function foo(): Type)
(function_definition
  return_type: (named_type (name) @return_type))
(method_declaration
  return_type: (named_type (name) @return_type))

;; Property types (public Type $prop)
(property_declaration
  type: (named_type (name) @field_type))

;; Extends (class Foo extends Bar)
(base_clause
  (name) @impl_type)

;; Implements (class Foo implements Bar, Baz)
(class_interface_clause
  (name) @impl_type)

;; Catch-all for named types
(named_type (name) @type_ref)
"#;

/// Doc comment node types — PHPDoc uses `/** ... */` parsed as comment
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "function", "class", "interface", "trait", "enum", "namespace", "use", "extends", "implements",
    "abstract", "final", "static", "public", "protected", "private", "return", "if", "else",
    "elseif", "for", "foreach", "while", "do", "switch", "case", "break", "continue", "new",
    "try", "catch", "finally", "throw", "echo", "print", "var", "const", "true", "false", "null",
    "self", "parent", "this", "array", "string", "int", "float", "bool", "void", "mixed", "never",
    "callable", "iterable", "object", "isset", "unset", "empty",
];

const COMMON_TYPES: &[&str] = &[
    "string", "int", "float", "bool", "array", "object", "callable", "iterable", "void", "null",
    "mixed", "never", "self", "parent", "static", "false", "true", "Closure", "Iterator",
    "Generator", "Traversable", "Countable", "Throwable", "Exception", "RuntimeException",
    "InvalidArgumentException", "stdClass",
];

/// Strip `$` prefix from PHP property names.
///
/// PHP properties are declared as `$name`, but callers reference them without `$`.
/// This hook strips the prefix so property names match call sites.
fn post_process_php(
    name: &mut String,
    _chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    if let Some(stripped) = name.strip_prefix('$') {
        *name = stripped.to_string();
    }
    true
}

fn extract_return(signature: &str) -> Option<String> {
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

static DEFINITION: LanguageDef = LanguageDef {
    name: "php",
    grammar: Some(|| tree_sitter_php::LANGUAGE_PHP.into()),
    extensions: &["php"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["declaration_list"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Test.php")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["declaration_list"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_php),
    test_markers: &["@test", "function test"],
    test_path_patterns: &["%/tests/%", "%/Tests/%", "%Test.php"],
    structural_matchers: None,
    entry_point_names: &[],
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
    fn parse_php_class() {
        let content = r#"<?php
class User {
    private string $name;
    public function getName(): string {
        return $this->name;
    }
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "User").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_php_interface() {
        let content = r#"<?php
interface Printable {
    public function print(): void;
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let iface = chunks.iter().find(|c| c.name == "Printable").unwrap();
        assert_eq!(iface.chunk_type, ChunkType::Interface);
    }

    #[test]
    fn parse_php_trait() {
        let content = r#"<?php
trait Timestampable {
    public function getCreatedAt(): string {
        return date('Y-m-d');
    }
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let t = chunks.iter().find(|c| c.name == "Timestampable").unwrap();
        assert_eq!(t.chunk_type, ChunkType::Trait);
    }

    #[test]
    fn parse_php_enum() {
        let content = r#"<?php
enum Status: string {
    case Active = 'active';
    case Inactive = 'inactive';
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let e = chunks.iter().find(|c| c.name == "Status").unwrap();
        assert_eq!(e.chunk_type, ChunkType::Enum);
    }

    #[test]
    fn parse_php_function() {
        let content = r#"<?php
function formatDuration(int $seconds): string {
    $hours = intdiv($seconds, 3600);
    return "{$hours}h";
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "formatDuration").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_php_method_in_class() {
        let content = r#"<?php
class Calculator {
    public function add(int $a, int $b): int {
        return $a + $b;
    }
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }

    #[test]
    fn parse_php_constructor() {
        let content = r#"<?php
class User {
    public function __construct(private string $name) {}
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ctor = chunks.iter().find(|c| c.name == "__construct").unwrap();
        assert_eq!(ctor.chunk_type, ChunkType::Method);
        assert_eq!(ctor.parent_type_name.as_deref(), Some("User"));
    }

    #[test]
    fn parse_php_calls() {
        // NOTE: PHP grammar requires <?php tag, so extract_calls_from_chunk (which
        // re-parses chunk content without the tag) won't work. Use parse_file_calls
        // instead — this is the production path.
        let content = r#"<?php
function process(string $input): int {
    $trimmed = trim($input);
    $result = intval($trimmed);
    echo $result;
    return $result;
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let function_calls = parser.parse_file_calls(file.path()).unwrap();
        let func = function_calls
            .iter()
            .find(|fc| fc.name == "process")
            .unwrap();
        let names: Vec<_> = func.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"trim"),
            "Expected trim call, got: {:?}",
            names
        );
        assert!(
            names.contains(&"intval"),
            "Expected intval call, got: {:?}",
            names
        );
    }

    #[test]
    fn parse_php_property_strips_dollar() {
        let content = r#"<?php
class Config {
    public string $name = "default";
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let prop = chunks.iter().find(|c| c.chunk_type == ChunkType::Property).unwrap();
        assert_eq!(prop.name, "name", "Property name should have $ stripped");
    }

    #[test]
    fn test_extract_return_php() {
        assert_eq!(
            extract_return("function add(int $a, int $b): int {"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            extract_return("function getName(): string {"),
            Some("Returns string".to_string())
        );
        assert_eq!(extract_return("function doSomething(): void {"), None);
        assert_eq!(extract_return("function doSomething(): mixed {"), None);
        assert_eq!(
            extract_return("function getUser(): ?User {"),
            Some("Returns user".to_string())
        );
        assert_eq!(extract_return("function doSomething() {"), None);
    }
}
