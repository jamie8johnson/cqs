//! Haskell language definition

use super::{ChunkType, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Haskell code chunks.
///
/// Haskell constructs:
///   - `function` → Function
///   - `signature` → skipped (type signatures are associated with functions)
///   - `data_type` → Struct or Enum (depending on constructor count)
///   - `newtype` → Struct
///   - `type_synomym` → TypeAlias (note: grammar has typo "synomym")
///   - `class` → Trait
///   - `instance` → Object (typeclass instance)
const CHUNK_QUERY: &str = r#"
;; Function definition: foo x y = ...
(function
  name: (variable) @name) @function

;; Data type definition: data Foo = Bar | Baz
(data_type
  name: (name) @name) @struct

;; Newtype definition: newtype Foo = Foo a
(newtype
  name: (name) @name) @struct

;; Type synonym: type Foo = Bar
(type_synomym
  name: (name) @name) @struct

;; Typeclass definition: class Foo a where ...
(class
  name: (name) @name) @trait

;; Instance declaration: instance Foo Bar where ...
(instance
  name: (name) @name) @struct
"#;

/// Tree-sitter query for extracting Haskell calls.
///
/// Haskell uses `apply` for function application. We capture:
///   - Direct application: `foo arg` → (apply function: (variable) ...)
///   - Qualified application: `Data.Map.lookup key m`
const CALL_QUERY: &str = r#"
;; Direct function application: foo arg
(apply
  function: (variable) @callee)

;; Qualified function call: Module.func arg
(apply
  function: (qualified
    id: (variable) @callee))
"#;

/// Doc comment node types — Haskell uses `-- |` and `{- | -}` doc comments
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "module", "where", "import", "qualified", "as", "hiding", "data", "type", "newtype", "class",
    "instance", "deriving", "do", "let", "in", "case", "of", "if", "then", "else", "forall",
    "infixl", "infixr", "infix", "default", "foreign", "True", "False", "Nothing", "Just",
    "Maybe", "Either", "Left", "Right", "IO", "Int", "Integer", "Float", "Double", "Char",
    "String", "Bool", "Show", "Read", "Eq", "Ord", "Num", "Monad", "Functor", "Applicative",
    "Foldable", "Traversable", "return", "pure", "putStrLn", "print", "map", "filter", "fmap",
];

/// Post-process Haskell chunks to set correct chunk types.
fn post_process_haskell(
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
        "instance" => *chunk_type = ChunkType::Object,
        _ => {}
    }
    true
}

/// Extract return type from Haskell type signatures.
///
/// Haskell signatures: `foo :: Int -> Bool -> String`
/// Return type is the last type after the final `->`.
fn extract_return(signature: &str) -> Option<String> {
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

static DEFINITION: LanguageDef = LanguageDef {
    name: "haskell",
    grammar: Some(|| tree_sitter_haskell::LANGUAGE.into()),
    extensions: &["hs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}Spec.hs")),
    type_query: None,
    common_types: &[
        "Int", "Integer", "Float", "Double", "Char", "String", "Bool", "IO", "Maybe", "Either",
        "Show", "Read", "Eq", "Ord", "Num",
    ],
    container_body_kinds: &["class_declarations", "instance_declarations"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_haskell as PostProcessChunkFn),
    test_markers: &["hspec", "describe", "it ", "prop "],
    test_path_patterns: &["%/test/%", "%Spec.hs", "%Test.hs"],
    structural_matchers: None,
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
    fn parse_haskell_function() {
        let content = r#"
greet :: String -> String
greet name = "Hello, " ++ name
"#;
        let file = write_temp_file(content, "hs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_haskell_data_type() {
        let content = r#"
data Color = Red | Green | Blue
"#;
        let file = write_temp_file(content, "hs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let dt = chunks
            .iter()
            .find(|c| c.name == "Color" && c.chunk_type == ChunkType::Enum);
        assert!(dt.is_some(), "Should find 'Color' data type as Enum");
    }

    #[test]
    fn parse_haskell_typeclass() {
        let content = r#"
class Printable a where
  display :: a -> String
"#;
        let file = write_temp_file(content, "hs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let tc = chunks
            .iter()
            .find(|c| c.name == "Printable" && c.chunk_type == ChunkType::Trait);
        assert!(tc.is_some(), "Should find 'Printable' typeclass as Trait");
    }

    #[test]
    fn parse_haskell_instance() {
        let content = r#"
data Color = Red | Green | Blue

instance Show Color where
  show Red = "Red"
  show Green = "Green"
  show Blue = "Blue"
"#;
        let file = write_temp_file(content, "hs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let inst = chunks
            .iter()
            .find(|c| c.name == "Show" && c.chunk_type == ChunkType::Object);
        assert!(inst.is_some(), "Should find 'Show' instance as Object");
    }

    #[test]
    fn parse_haskell_calls() {
        let content = r#"
process :: String -> IO ()
process text = do
  let trimmed = trim text
  putStrLn trimmed
  validate trimmed
"#;
        let file = write_temp_file(content, "hs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"putStrLn"),
            "Expected putStrLn, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_return_haskell() {
        assert_eq!(
            extract_return("greet :: String -> String"),
            Some("Returns string".to_string())
        );
        assert_eq!(
            extract_return("add :: Int -> Int -> Int"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            extract_return("main :: IO ()"),
            None
        );
        assert_eq!(extract_return(""), None);
    }
}
