//! Scala language definition

use super::{FieldStyle, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting Scala code chunks
const CHUNK_QUERY: &str = r#"
;; Functions
(function_definition
  name: (identifier) @name) @function

;; Classes
(class_definition
  name: (identifier) @name) @class

;; Objects (singletons)
(object_definition
  name: (identifier) @name) @object

;; Traits
(trait_definition
  name: (identifier) @name) @trait

;; Enums (Scala 3)
(enum_definition
  name: (identifier) @name) @enum

;; Val bindings
(val_definition
  pattern: (identifier) @name) @const

;; Var bindings
(var_definition
  pattern: (identifier) @name) @const

;; Type aliases — name is type_identifier, not identifier
(type_definition
  name: (type_identifier) @name) @typealias

;; Scala 3 extensions — name extracted from first parameter type
(extension_definition
  parameters: (parameters
    (parameter
      type: (type_identifier) @name))) @extension
"#;

/// Tree-sitter query for extracting Scala function calls
const CALL_QUERY: &str = r#"
(call_expression
  function: (identifier) @callee)

(call_expression
  function: (field_expression
    field: (identifier) @callee))
"#;

/// Tree-sitter query for extracting Scala type references
const TYPE_QUERY: &str = r#"
;; Parameter types
(parameter
  type: (type_identifier) @param_type)
(parameter
  type: (generic_type (type_identifier) @param_type))

;; Return types
(function_definition
  return_type: (type_identifier) @return_type)
(function_definition
  return_type: (generic_type (type_identifier) @return_type))

;; Field types — val/var type annotations
(val_definition
  type: (type_identifier) @field_type)
(val_definition
  type: (generic_type (type_identifier) @field_type))
(var_definition
  type: (type_identifier) @field_type)
(var_definition
  type: (generic_type (type_identifier) @field_type))

;; Extends clauses (class Foo extends Bar)
(extends_clause
  type: (type_identifier) @impl_type)
(extends_clause
  type: (generic_type (type_identifier) @impl_type))

;; Catch-all — generic type arguments
(type_arguments
  (type_identifier) @type_ref)
"#;

/// Doc comment node types
const DOC_NODES: &[&str] = &["comment", "block_comment"];

const STOPWORDS: &[&str] = &[
    "def", "val", "var", "class", "object", "trait", "sealed", "case", "abstract", "override",
    "implicit", "lazy", "extends", "with", "import", "package", "match", "if", "else", "for",
    "while", "yield", "return", "throw", "try", "catch", "finally", "new", "this", "super",
    "true", "false", "null",
];

const COMMON_TYPES: &[&str] = &[
    "String", "Int", "Long", "Double", "Float", "Boolean", "Char", "Byte", "Short", "Unit",
    "Any", "AnyRef", "AnyVal", "Nothing", "Null", "Option", "Some", "None", "List", "Map", "Set",
    "Seq", "Vector", "Array", "Future", "Either", "Left", "Right", "Try", "Success", "Failure",
    "Iterator", "Iterable", "Ordering",
];

/// Extracts the return type from a Scala function signature and formats it as a documentation string.
/// Parses a Scala function signature to find the return type annotation (the type following `:` after the parameter list and before `=` or `{`), then formats it as a "Returns {type}" string suitable for documentation.
/// # Arguments
/// `signature` - A Scala function signature string, e.g., `def foo(x: Int): String = ...`
/// # Returns
/// `Some(String)` containing the formatted return type documentation (e.g., "Returns String"), or `None` if no return type annotation is found or the signature is malformed.
fn extract_return(signature: &str) -> Option<String> {
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

static DEFINITION: LanguageDef = LanguageDef {
    name: "scala",
    grammar: Some(|| tree_sitter_scala::LANGUAGE.into()),
    extensions: &["scala", "sc"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &["class_definition", "trait_definition", "object_definition"],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/src/test/scala/{stem}Spec.scala")),
    test_name_suggestion: None,
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES,
    container_body_kinds: &["template_body"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: None,
    test_markers: &["@Test", "\"should", "it should"],
    test_path_patterns: &["%/test/%", "%/tests/%", "%Spec.scala", "%Test.scala"],
    structural_matchers: None,
    entry_point_names: &["main"],
    trait_method_names: &[
        "equals", "hashCode", "toString", "compare", "apply", "unapply",
    ],
    injections: &[],
    doc_format: "javadoc",
    doc_convention: "Use Scaladoc format: @param, @return, @throws tags.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "val var private protected override lazy",
    },
    skip_line_prefixes: &["class ", "case class", "sealed class", "trait ", "object "],
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
    fn test_extract_return_scala() {
        assert_eq!(
            extract_return("def foo(x: Int): String ="),
            Some("Returns string".to_string())
        );
        assert_eq!(extract_return("def bar() ="), None);
        assert_eq!(
            extract_return("def process(input: List[Int]): Boolean ="),
            Some("Returns boolean".to_string())
        );
        assert_eq!(extract_return("def noReturn() {"), None);
    }

    #[test]
    fn parse_scala_class() {
        let content = r#"
class Calculator {
  def add(a: Int, b: Int): Int = {
    a + b
  }
}
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);
    }

    #[test]
    fn parse_scala_object() {
        let content = r#"
object Main {
  def run(): Unit = {
    println("hello")
  }
}
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let obj = chunks.iter().find(|c| c.name == "Main").unwrap();
        assert_eq!(obj.chunk_type, ChunkType::Object);
    }

    #[test]
    fn parse_scala_trait() {
        let content = r#"
trait Printable {
  def print(): Unit
}
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let t = chunks.iter().find(|c| c.name == "Printable").unwrap();
        assert_eq!(t.chunk_type, ChunkType::Trait);
    }

    #[test]
    fn parse_scala_type_alias() {
        let content = "type StringMap = Map[String, String]\n";
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ta = chunks.iter().find(|c| c.name == "StringMap").unwrap();
        assert_eq!(ta.chunk_type, ChunkType::TypeAlias);
    }

    #[test]
    fn parse_scala_method_in_class() {
        let content = r#"
class Calculator {
  def add(a: Int, b: Int): Int = {
    a + b
  }
}
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let method = chunks.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
        assert_eq!(method.parent_type_name.as_deref(), Some("Calculator"));
    }

    #[test]
    fn parse_scala_val_const() {
        let content = r#"
object Config {
  val maxRetries: Int = 3
}
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let val_chunk = chunks.iter().find(|c| c.name == "maxRetries").unwrap();
        assert_eq!(val_chunk.chunk_type, ChunkType::Constant);
    }

    #[test]
    fn parse_scala_calls() {
        let content = r#"
object App {
  def process(input: String): Unit = {
    val result = transform(input)
    println(result.toString)
  }
}
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(names.contains(&"transform"), "Expected transform, got: {:?}", names);
        assert!(names.contains(&"println"), "Expected println, got: {:?}", names);
    }

    #[test]
    fn parse_scala3_extension() {
        let content = r#"
extension (x: Int)
  def isEven: Boolean = x % 2 == 0
  def double: Int = x * 2
"#;
        let file = write_temp_file(content, "scala");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let ext = chunks
            .iter()
            .find(|c| c.chunk_type == ChunkType::Extension);
        assert!(
            ext.is_some(),
            "Expected an extension chunk, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.chunk_type))
                .collect::<Vec<_>>()
        );
        let ext = ext.unwrap();
        assert_eq!(ext.name, "Int");
        assert_eq!(ext.chunk_type, ChunkType::Extension);
    }
}
