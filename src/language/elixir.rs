//! Elixir language definition

use super::{ChunkType, FieldStyle, LanguageDef, PostProcessChunkFn, SignatureStyle};

/// Tree-sitter query for extracting Elixir code chunks.
/// Elixir has no dedicated node types for `def`/`defmodule` etc. — everything
/// is a generic `call` node. The keyword (`def`, `defmodule`, etc.) is just an
/// `identifier` in the `target` field. We match the generic call structure and
/// reclassify in `post_process_elixir` based on the target identifier text.
/// Patterns matched:
///   - `def foo(args) do ... end` → Function
///   - `defp foo(args) do ... end` → Function (private)
///   - `defmodule Foo do ... end` → Module
///   - `defprotocol Foo do ... end` → Interface
///   - `defimpl Foo do ... end` → Object (protocol implementation)
///   - `defmacro foo(args) do ... end` → Macro
///   - `defstruct [...]` → Struct
///   - `defguard foo(args)` → Function
///   - `defdelegate foo(args)` → Function
const CHUNK_QUERY: &str = r#"
;; Function with arguments: def foo(args) do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (call
      target: (identifier) @name))
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp" "defdelegate")) @function

;; Function with guard: def foo(args) when guard do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (binary_operator
      left: (call
        target: (identifier) @name)))
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp")) @function

;; Zero-arity function: def foo do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (identifier) @name)
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp" "defdelegate")) @function

;; Module definition: defmodule MyApp.Foo do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (alias) @name)
  (#any-of? @_keyword "defmodule" "defprotocol")) @struct

;; defimpl: defimpl Protocol, for: Type do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (alias) @name)
  (#eq? @_keyword "defimpl")) @struct

;; defstruct: defstruct [:field1, :field2]
(call
  target: (identifier) @_keyword
  (#eq? @_keyword "defstruct")) @struct
"#;

/// Tree-sitter query for extracting Elixir function calls.
const CALL_QUERY: &str = r#"
;; Local function call: foo(args)
(call
  target: (identifier) @callee
  (#not-any-of? @callee "def" "defp" "defmodule" "defprotocol" "defimpl" "defmacro" "defmacrop" "defstruct" "defguard" "defguardp" "defdelegate" "defexception" "defoverridable" "use" "import" "require" "alias"))

;; Remote function call: Module.function(args)
(call
  target: (dot
    right: (identifier) @callee))

;; Pipe into function: data |> function
(binary_operator
  operator: "|>"
  right: (identifier) @callee)
"#;

/// Doc comment node types — Elixir uses `@doc` and `@moduledoc` (comments are generic)
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "def", "defp", "defmodule", "defprotocol", "defimpl", "defmacro", "defmacrop", "defstruct",
    "defguard", "defguardp", "defdelegate", "defexception", "defoverridable", "do", "end", "fn",
    "case", "cond", "if", "else", "unless", "when", "with", "for", "receive", "try", "catch",
    "rescue", "after", "raise", "throw", "import", "require", "use", "alias", "nil", "true",
    "false", "and", "or", "not", "in", "is", "self", "super", "send", "spawn", "apply",
    "Enum", "List", "Map", "String", "IO", "Kernel", "Agent", "Task", "GenServer",
];

/// Post-process Elixir chunks to set correct chunk types.
fn post_process_elixir(
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
                                            if let Ok(mod_name) =
                                                arg.utf8_text(source.as_bytes())
                                            {
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
fn extract_return(_signature: &str) -> Option<String> {
    // Elixir is dynamically typed — no return type annotations in signatures
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "elixir",
    grammar: Some(|| tree_sitter_elixir::LANGUAGE.into()),
    extensions: &["ex", "exs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, _parent| format!("test/{stem}_test.exs")),
    test_name_suggestion: Some(|name| format!("test \"{}\"", name)),
    type_query: None,
    common_types: &[],
    container_body_kinds: &["do_block"],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_elixir as PostProcessChunkFn),
    test_markers: &["test ", "describe "],
    test_path_patterns: &["%/test/%", "%_test.exs"],
    structural_matchers: None,
    entry_point_names: &["start", "init", "handle_call", "handle_cast", "handle_info"],
    trait_method_names: &[
        "init",
        "handle_call",
        "handle_cast",
        "handle_info",
        "terminate",
        "code_change",
    ],
    injections: &[],
    doc_format: "elixir_doc",
    doc_convention: "Use @doc with ## Examples section per Elixir conventions.",
    field_style: FieldStyle::NameFirst {
        separators: ":",
        strip_prefixes: "",
    },
    skip_line_prefixes: &["defmodule", "defstruct"],
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
    fn parse_elixir_function() {
        let content = r#"
defmodule MyApp do
  def greet(name) do
    "Hello, #{name}"
  end
end
"#;
        let file = write_temp_file(content, "ex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(func.chunk_type, ChunkType::Function);
    }

    #[test]
    fn parse_elixir_module() {
        let content = r#"
defmodule MyApp.Users do
  def list_users do
    []
  end
end
"#;
        let file = write_temp_file(content, "ex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let module = chunks
            .iter()
            .find(|c| c.name == "MyApp.Users" && c.chunk_type == ChunkType::Module);
        assert!(module.is_some(), "Should find 'MyApp.Users' module");
    }

    #[test]
    fn parse_elixir_protocol() {
        let content = r#"
defprotocol Printable do
  def to_string(data)
end
"#;
        let file = write_temp_file(content, "ex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let proto = chunks
            .iter()
            .find(|c| c.name == "Printable" && c.chunk_type == ChunkType::Interface);
        assert!(proto.is_some(), "Should find 'Printable' protocol/interface");
    }

    #[test]
    fn parse_elixir_macro() {
        let content = r#"
defmodule MyMacros do
  defmacro my_if(condition, do: block) do
    quote do
      case unquote(condition) do
        true -> unquote(block)
        _ -> nil
      end
    end
  end
end
"#;
        let file = write_temp_file(content, "ex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let mac = chunks
            .iter()
            .find(|c| c.name == "my_if" && c.chunk_type == ChunkType::Macro);
        assert!(mac.is_some(), "Should find 'my_if' macro");
    }

    #[test]
    fn parse_elixir_calls() {
        let content = r#"
defmodule Processor do
  def process(data) do
    data
    |> String.trim()
    |> transform()
    |> IO.puts()
  end

  defp transform(data), do: data
end
"#;
        let file = write_temp_file(content, "ex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let func = chunks.iter().find(|c| c.name == "process").unwrap();
        let calls = parser.extract_calls_from_chunk(func);
        let names: Vec<_> = calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            names.contains(&"transform"),
            "Expected transform, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_return_elixir() {
        assert_eq!(extract_return("def greet(name) do"), None);
        assert_eq!(extract_return(""), None);
    }
}
