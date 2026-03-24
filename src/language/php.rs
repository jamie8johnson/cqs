//! PHP language definition

use super::{ChunkType, InjectionRule, LanguageDef, SignatureStyle};

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
/// 
/// Parses a PHP function signature to find the return type annotation (the type following `:` after the parameter list). Filters out void and mixed types, strips nullable prefixes, and returns a formatted description string.
/// 
/// # Arguments
/// 
/// * `signature` - A PHP function signature string, expected to contain parameter list and optional return type annotation
/// 
/// # Returns
/// 
/// Returns `Some(String)` containing a formatted return type description (e.g., "Returns string") if a valid, non-void return type is found. Returns `None` if no return type annotation exists, the type is void/mixed, the colon appears after the opening brace, or the signature is malformed.
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
    /// Parses a PHP class definition from a temporary file and verifies the parser correctly identifies it as a Class chunk type.
    /// 
    /// This test function creates a temporary PHP file containing a User class with a private property and public method, parses it using the Parser, and asserts that the resulting chunks contain a chunk named "User" with the ChunkType::Class variant.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded PHP content.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if parsing fails, if no chunk named "User" is found in the parsed results, or if the User chunk does not have ChunkType::Class.

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
    /// Parses a PHP interface definition and verifies the parser correctly identifies it as an Interface chunk type.
    /// 
    /// # Arguments
    /// 
    /// None
    /// 
    /// # Returns
    /// 
    /// None
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be created, the parser fails to parse the file, the "Printable" interface is not found in the parsed chunks, or the chunk type is not Interface.

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
    /// Tests that the parser correctly identifies and extracts a PHP trait definition from a source file.
    /// 
    /// # Arguments
    /// 
    /// This function takes no arguments. It creates its own test data internally.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. This is a test function that validates parser behavior through assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to parse the file, the "Timestampable" trait chunk is not found in the parsed results, or the chunk type is not correctly identified as `ChunkType::Trait`.

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
    /// Parses a PHP file containing a backed enum definition and verifies the parser correctly identifies it as an Enum chunk type.
    /// 
    /// This is a test function that creates a temporary PHP file with a string-backed enum, parses it using the Parser, and asserts that the resulting chunk has the correct type (Enum) and name (Status).
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and returns unit type.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, the file cannot be parsed, the "Status" enum is not found in the parsed chunks, or the chunk type assertion fails.

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
    /// Parses a PHP file containing a function definition and verifies the parser correctly identifies it.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded PHP source code.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, the parser fails to initialize, file parsing fails, the `formatDuration` function is not found in the parsed chunks, or the identified chunk is not of type `Function`.

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
    /// Verifies that the parser correctly identifies PHP methods within classes, extracting method metadata including its type and parent class name.
    /// 
    /// # Arguments
    /// 
    /// None - this is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// None - this function is a test that performs assertions.
    /// 
    /// # Panics
    /// 
    /// Panics if any assertions fail, including:
    /// - If the temporary file cannot be written
    /// - If the parser initialization fails
    /// - If file parsing fails
    /// - If the "add" method is not found in parsed chunks
    /// - If the method's chunk type is not `Method`
    /// - If the parent type name is not "Calculator"

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
    /// Tests parsing of PHP class constructors to verify that `__construct` methods are correctly identified as methods with their parent class properly associated.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// This function returns nothing (unit type).
    /// 
    /// # Panics
    /// 
    /// Panics if temporary file creation fails, parser initialization fails, file parsing fails, the constructor chunk is not found in parsed results, or any assertions fail.

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
        assert_eq!(ctor.chunk_type, ChunkType::Constructor);
        assert_eq!(ctor.parent_type_name.as_deref(), Some("User"));
    }
    /// Parses PHP function calls from a temporary PHP file and verifies that the parser correctly identifies function calls within a function definition.
    /// 
    /// This is a test function that creates a temporary PHP file containing a `process` function with calls to `trim` and `intval`, then uses the Parser to extract all function calls and asserts that both expected calls are detected. It demonstrates the correct usage of `parse_file_calls` for PHP parsing, which requires the `<?php` tag to be present in the file content.
    /// 
    /// # Arguments
    /// 
    /// None.
    /// 
    /// # Returns
    /// 
    /// None. This function is a test that asserts expected behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the temporary file cannot be written, if the parser fails to initialize or parse the file, if the `process` function is not found in the parsed calls, or if either the `trim` or `intval` function calls are not detected.

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
    /// Verifies that the parser correctly strips the dollar sign prefix from PHP property names during parsing.
    /// 
    /// # Arguments
    /// 
    /// None. This is a unit test function.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to initialize
    /// - The file fails to parse
    /// - No Property chunk is found in the parsed output
    /// - The property name is not "name" (i.e., the dollar sign was not properly stripped)

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

    // --- Multi-grammar injection tests ---
    /// Verifies that the parser correctly extracts HTML chunks from a PHP template file that contains both PHP code blocks and HTML content.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to extract HTML chunks from the mixed PHP/HTML content, or if temporary file creation fails.

    #[test]
    fn parse_php_with_html_extracts_html_chunks() {
        // PHP template with HTML content between <?php blocks
        let content = r#"<?php
$title = "My Page";
?>
<!DOCTYPE html>
<html>
<body>
<h1><?php echo $title; ?></h1>
<nav id="main-nav">
  <a href="/">Home</a>
</nav>
</body>
</html>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Should have HTML heading chunk
        let html_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Html)
            .collect();
        assert!(
            !html_chunks.is_empty(),
            "Expected HTML chunks from injection, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
    }
    /// Verifies that the parser correctly extracts code chunks from a PHP file containing nested HTML with embedded JavaScript, following a multi-level language injection chain (PHP → HTML → JS).
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that uses hardcoded test content.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts on the parser results and panics if expectations are not met.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be written
    /// - The parser fails to parse the file
    /// - A PHP function named `getdata` is not found in the parsed chunks
    /// - A JavaScript function named `handleclick` is not found in the parsed chunks (indicating failure to recursively extract from nested script tags)

    #[test]
    fn parse_php_with_html_script_extracts_js() {
        // PHP file with <script> in HTML region — 2-level chain: PHP→HTML→JS
        let content = r#"<?php
function getData(): array {
    return ['key' => 'value'];
}
?>
<html>
<body>
<script>
function handleClick(event) {
    const el = document.getElementById('target');
    el.classList.toggle('active');
}
</script>
</body>
</html>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Should have PHP function
        let php_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Php)
            .collect();
        assert!(
            php_chunks.iter().any(|c| c.name == "getData"),
            "Expected PHP function 'getData', got: {:?}",
            php_chunks.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        // Should have JS function (via recursive injection: PHP→HTML→JS)
        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_chunks.iter().any(|c| c.name == "handleClick"),
            "Expected JS function 'handleClick' from 2-level injection, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
    }
    /// Verifies that PHP classes and methods are correctly preserved as separate code chunks during parsing, surviving any injection processing.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts expected parsing behavior and panics if assertions fail.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to identify the PHP class "UserController" or the PHP method "index" as separate chunks with the correct language designation.

    #[test]
    fn parse_php_keeps_php_chunks() {
        // PHP functions/classes must survive injection processing
        let content = r#"<?php
class UserController {
    public function index(): string {
        return 'Hello';
    }
}
?>
<h1>Page Title</h1>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(
            chunks.iter().any(|c| c.name == "UserController" && c.language == crate::parser::Language::Php),
            "PHP class 'UserController' should survive injection"
        );
        assert!(
            chunks.iter().any(|c| c.name == "index" && c.language == crate::parser::Language::Php),
            "PHP method 'index' should survive injection"
        );
    }
    /// Verifies that a pure PHP file without any HTML or text nodes is parsed correctly without triggering any injection.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data internally.
    /// 
    /// # Returns
    /// 
    /// Nothing. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, fails to parse the temporary file, or if any assertion fails (e.g., if non-PHP chunks are found, or if expected function/class chunks are missing).

    #[test]
    fn parse_php_without_html_unchanged() {
        // Pure PHP file (no text nodes) — injection should not fire
        let content = r#"<?php
function purePhp(): int {
    return 42;
}

class Standalone {
    public function method(): void {}
}
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // All chunks should be PHP
        for chunk in &chunks {
            assert_eq!(
                chunk.language,
                crate::parser::Language::Php,
                "Pure PHP file should have only PHP chunks, found {:?} for '{}'",
                chunk.language,
                chunk.name
            );
        }
        assert!(chunks.iter().any(|c| c.name == "purePhp"));
        assert!(chunks.iter().any(|c| c.name == "Standalone"));
    }
    /// Tests the parser's ability to extract code chunks from a PHP file containing interleaved PHP, HTML, and embedded JavaScript. Verifies that JavaScript functions embedded within script tags are correctly identified and extracted with the JavaScript language designation, even when interspersed with PHP code blocks.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts test conditions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the JavaScript function 'jsFunc' is not found in the parsed chunks, indicating the parser failed to properly extract embedded JavaScript from interleaved PHP/HTML content.

    #[test]
    fn parse_php_interleaved() {
        // Interleaved PHP and HTML with embedded JS
        let content = r#"<?php echo "start"; ?>
<div>
<script>
function jsFunc() { return 1; }
</script>
</div>
<?php echo "end"; ?>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // JS function should be extracted
        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_chunks.iter().any(|c| c.name == "jsFunc"),
            "Expected JS function 'jsFunc' from interleaved PHP, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
    }
    /// Verifies that the parser correctly extracts JavaScript call graphs from PHP files containing embedded HTML and JavaScript code.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. Returns unit type `()`.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to extract the call graph, if the "caller" function is not found in the results, or if the "caller→helper" call relationship is not detected in the parsed output.

    #[test]
    fn parse_php_injection_call_graph() {
        // JS call graph should be extracted from PHP→HTML→JS
        let content = r#"<?php $x = 1; ?>
<script>
function caller() {
    helper();
}
function helper() {
    return 42;
}
</script>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let (calls, _types) = parser.parse_file_relationships(file.path()).unwrap();

        let caller = calls.iter().find(|c| c.name == "caller");
        assert!(
            caller.is_some(),
            "Expected call graph for 'caller' from PHP→HTML→JS, got: {:?}",
            calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        let callee_names: Vec<_> = caller.unwrap().calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            callee_names.contains(&"helper"),
            "Expected caller→helper, got: {:?}",
            callee_names
        );
    }
    /// Verifies that the parser correctly extracts PHP and HTML chunks from a file containing HTML content before the first PHP tag.
    /// 
    /// This test validates that when a PHP file contains HTML markup preceding PHP code, the parser properly identifies and separates both the PHP function definitions and the HTML content into distinct chunks. It confirms that leading HTML, PHP code blocks, and trailing HTML are all correctly parsed and categorized by language type.
    /// 
    /// # Arguments
    /// 
    /// None (this is a test function).
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to find a PHP function named "getTitle", or if no HTML chunks are extracted from the mixed HTML/PHP content.

    #[test]
    fn parse_php_html_first() {
        // HTML before first <?php tag — `text` is a direct child of `program`
        let content = r#"<h1>Welcome</h1>
<nav id="main-nav">
  <a href="/">Home</a>
</nav>
<?php
function getTitle(): string {
    return "My Page";
}
?>
<footer>End</footer>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // PHP function should exist
        assert!(
            chunks.iter().any(|c| c.name == "getTitle" && c.language == crate::parser::Language::Php),
            "Expected PHP function 'getTitle'"
        );

        // HTML chunks should be extracted from both leading and trailing regions
        let html_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Html)
            .collect();
        assert!(
            !html_chunks.is_empty(),
            "Expected HTML chunks from file with leading HTML, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
    }
    /// Verifies that the parser correctly handles the maximum injection depth limit without crashing or producing incorrect results when parsing PHP files containing nested language injections.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that operates on hardcoded test content.
    /// 
    /// # Returns
    /// 
    /// None. This is a test function that uses assertions to verify parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any of the following assertions fail:
    /// - The parser fails to parse the temporary PHP file
    /// - No PHP chunks are found in the parsed output
    /// - No JavaScript function named 'init' is found in the parsed chunks
    /// - Expected CSS chunks are missing from the parse results

    #[test]
    fn parse_php_injection_depth_limit() {
        // Verify that injection doesn't crash or produce garbage with normal PHP files.
        // The depth limit (MAX_INJECTION_DEPTH=3) should never be reached in practice
        // since PHP→HTML→JS is only depth 2. This test ensures the recursive machinery
        // handles the deepest real-world chain (PHP→HTML→JS/CSS) without issues.
        let content = r#"<?php
class App {
    public function render(): string {
        return '<html>';
    }
}
?>
<html>
<head>
<style>
body { color: red; }
.container { margin: 0 auto; }
</style>
</head>
<body>
<script>
function init() {
    document.querySelector('.container');
}
</script>
</body>
</html>
"#;
        let file = write_temp_file(content, "php");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Should have PHP, JS, and CSS chunks — full 3-level chain
        let php_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Php)
            .collect();
        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        let css_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Css)
            .collect();

        assert!(!php_chunks.is_empty(), "Expected PHP chunks");
        assert!(
            js_chunks.iter().any(|c| c.name == "init"),
            "Expected JS function 'init' from PHP→HTML→JS chain, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
        assert!(
            !css_chunks.is_empty(),
            "Expected CSS chunks from PHP→HTML→CSS chain, got: {:?}",
            chunks.iter().map(|c| (&c.name, &c.language)).collect::<Vec<_>>()
        );
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
