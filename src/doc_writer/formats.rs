//! Per-language doc comment format definitions.
//!
//! Each language has a `DocFormat` describing its comment syntax (prefix, line prefix,
//! suffix) and insertion position (before the function or inside the body).
//!
//! `format_doc_comment` takes raw LLM text and wraps it in the correct format
//! with proper indentation for the target language.

use crate::language::Language;

/// Doc comment format for a language.
#[derive(Debug, Clone, PartialEq)]
pub struct DocFormat {
    /// Block-open delimiter (e.g., `"/**"` for Java, `"\"\"\""` for Python).
    /// Empty string means no block-open line.
    pub prefix: &'static str,
    /// Per-line prefix (e.g., `"/// "` for Rust, `" * "` for Java).
    /// Empty string means no per-line prefix (content lines are bare).
    pub line_prefix: &'static str,
    /// Block-close delimiter (e.g., `" */"` for Java, `"\"\"\""` for Python).
    /// Empty string means no block-close line.
    pub suffix: &'static str,
    /// Where the doc comment is inserted relative to the function.
    pub position: InsertionPosition,
}

/// Where a doc comment is placed relative to the function definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionPosition {
    /// Doc comment goes on lines immediately before the function signature.
    /// Used by most languages (Rust, Go, Java, C, etc.).
    BeforeFunction,
    /// Doc comment goes inside the function body (first statement).
    /// Used by Python (docstrings after `def`).
    InsideBody,
}

/// Returns the doc comment format for a given language.
///
/// Covers all major language families. Languages without a specific entry
/// fall through to the default `//`-style line comments.
pub fn doc_format_for(language: Language) -> DocFormat {
    match language {
        // Triple-slash: Rust, F#
        Language::Rust | Language::FSharp => DocFormat {
            prefix: "",
            line_prefix: "/// ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // Python docstrings (inside body)
        Language::Python => DocFormat {
            prefix: "\"\"\"",
            line_prefix: "",
            suffix: "\"\"\"",
            position: InsertionPosition::InsideBody,
        },

        // Go: plain // comments, but convention prepends FuncName
        Language::Go => DocFormat {
            prefix: "",
            line_prefix: "// ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // Javadoc-family: /** ... */
        Language::Java
        | Language::C
        | Language::Cpp
        | Language::Scala
        | Language::Kotlin
        | Language::Swift
        | Language::Php
        | Language::TypeScript
        | Language::JavaScript
        | Language::CSharp
        | Language::Solidity
        | Language::Cuda
        | Language::Glsl => DocFormat {
            prefix: "/**",
            line_prefix: " * ",
            suffix: " */",
            position: InsertionPosition::BeforeFunction,
        },

        // Ruby: # comments
        Language::Ruby => DocFormat {
            prefix: "",
            line_prefix: "# ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // Elixir: @doc """..."""
        Language::Elixir => DocFormat {
            prefix: "@doc \"\"\"",
            line_prefix: "",
            suffix: "\"\"\"",
            position: InsertionPosition::BeforeFunction,
        },

        // Lua: --- comments (LDoc)
        Language::Lua => DocFormat {
            prefix: "",
            line_prefix: "--- ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // Haskell: -- | Haddock
        Language::Haskell => DocFormat {
            prefix: "",
            line_prefix: "-- | ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // OCaml: (** ... *)
        Language::OCaml => DocFormat {
            prefix: "(** ",
            line_prefix: "",
            suffix: " *)",
            position: InsertionPosition::BeforeFunction,
        },

        // Perl: # comments (POD is block-level, but inline # is simpler for function docs)
        Language::Perl => DocFormat {
            prefix: "",
            line_prefix: "# ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // Erlang: %% comments (edoc convention)
        Language::Erlang => DocFormat {
            prefix: "",
            line_prefix: "%% ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // R: #' roxygen2
        Language::R => DocFormat {
            prefix: "",
            line_prefix: "#' ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },

        // Default: // line comments for everything else
        _ => DocFormat {
            prefix: "",
            line_prefix: "// ",
            suffix: "",
            position: InsertionPosition::BeforeFunction,
        },
    }
}

/// Format raw doc text into a language-specific doc comment with indentation.
///
/// Takes the raw LLM-generated doc text (plain prose, possibly multi-line) and
/// wraps it in the correct doc comment syntax for the target language.
///
/// # Arguments
/// * `text` - Raw doc text from LLM (no comment markers)
/// * `language` - Target language (determines format)
/// * `indent` - Indentation prefix for each line (spaces/tabs matching the function)
/// * `func_name` - Function name (used by Go convention: "// FuncName does X")
///
/// # Returns
/// Formatted doc comment string ready to insert into source, including trailing newline.
pub fn format_doc_comment(text: &str, language: Language, indent: &str, func_name: &str) -> String {
    let format = doc_format_for(language);
    let lines: Vec<&str> = text.lines().collect();

    // Handle empty text
    if lines.is_empty() {
        return String::new();
    }

    let mut result = String::new();

    // For Go: prepend function name to first line per convention
    let go_first_line: String;
    let effective_lines: Vec<&str> = if language == Language::Go {
        if let Some(&first) = lines.first() {
            // "FuncName does X" — capitalize first char of description if needed
            go_first_line = format!("{func_name} {first}");
            let mut v = vec![go_first_line.as_str()];
            v.extend_from_slice(&lines[1..]);
            v
        } else {
            lines.clone()
        }
    } else {
        lines.clone()
    };

    // Emit prefix line if non-empty
    if !format.prefix.is_empty() {
        result.push_str(indent);
        result.push_str(format.prefix);
        result.push('\n');
    }

    // Emit content lines
    for line in &effective_lines {
        result.push_str(indent);
        result.push_str(format.line_prefix);
        result.push_str(line);
        result.push('\n');
    }

    // Emit suffix line if non-empty
    if !format.suffix.is_empty() {
        result.push_str(indent);
        result.push_str(format.suffix);
        result.push('\n');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Format registry tests ──────────────────────────────────────────

    #[test]
    fn test_rust_format() {
        let fmt = doc_format_for(Language::Rust);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "/// ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_python_format() {
        let fmt = doc_format_for(Language::Python);
        assert_eq!(fmt.prefix, "\"\"\"");
        assert_eq!(fmt.line_prefix, "");
        assert_eq!(fmt.suffix, "\"\"\"");
        assert_eq!(fmt.position, InsertionPosition::InsideBody);
    }

    #[test]
    fn test_go_format() {
        let fmt = doc_format_for(Language::Go);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "// ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_java_format() {
        let fmt = doc_format_for(Language::Java);
        assert_eq!(fmt.prefix, "/**");
        assert_eq!(fmt.line_prefix, " * ");
        assert_eq!(fmt.suffix, " */");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_typescript_format() {
        let fmt = doc_format_for(Language::TypeScript);
        assert_eq!(fmt.prefix, "/**");
        assert_eq!(fmt.line_prefix, " * ");
        assert_eq!(fmt.suffix, " */");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_fsharp_format() {
        let fmt = doc_format_for(Language::FSharp);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "/// ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_ruby_format() {
        let fmt = doc_format_for(Language::Ruby);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "# ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_elixir_format() {
        let fmt = doc_format_for(Language::Elixir);
        assert_eq!(fmt.prefix, "@doc \"\"\"");
        assert_eq!(fmt.line_prefix, "");
        assert_eq!(fmt.suffix, "\"\"\"");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_lua_format() {
        let fmt = doc_format_for(Language::Lua);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "--- ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_haskell_format() {
        let fmt = doc_format_for(Language::Haskell);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "-- | ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_ocaml_format() {
        let fmt = doc_format_for(Language::OCaml);
        assert_eq!(fmt.prefix, "(** ");
        assert_eq!(fmt.line_prefix, "");
        assert_eq!(fmt.suffix, " *)");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    #[test]
    fn test_default_format_for_unknown_language() {
        // Bash has no specific doc format, should get default //
        let fmt = doc_format_for(Language::Bash);
        assert_eq!(fmt.prefix, "");
        assert_eq!(fmt.line_prefix, "// ");
        assert_eq!(fmt.suffix, "");
        assert_eq!(fmt.position, InsertionPosition::BeforeFunction);
    }

    // ── format_doc_comment tests ───────────────────────────────────────

    #[test]
    fn test_format_rust_single_line() {
        let result =
            format_doc_comment("Returns the sum of two numbers.", Language::Rust, "", "add");
        assert_eq!(result, "/// Returns the sum of two numbers.\n");
    }

    #[test]
    fn test_format_rust_multiline() {
        let text = "Returns the sum of two numbers.\n\nPanics if overflow occurs.";
        let result = format_doc_comment(text, Language::Rust, "", "add");
        assert_eq!(
            result,
            "/// Returns the sum of two numbers.\n/// \n/// Panics if overflow occurs.\n"
        );
    }

    #[test]
    fn test_format_rust_with_indent() {
        let result =
            format_doc_comment("Does something useful.", Language::Rust, "    ", "process");
        assert_eq!(result, "    /// Does something useful.\n");
    }

    #[test]
    fn test_format_python_single_line() {
        let result = format_doc_comment(
            "Returns the sum of two numbers.",
            Language::Python,
            "    ",
            "add",
        );
        assert_eq!(
            result,
            "    \"\"\"\n    Returns the sum of two numbers.\n    \"\"\"\n"
        );
    }

    #[test]
    fn test_format_python_multiline() {
        let text = "Calculate the sum.\n\nArgs:\n    a: First number.\n    b: Second number.";
        let result = format_doc_comment(text, Language::Python, "    ", "add");
        let expected = "    \"\"\"\n    Calculate the sum.\n    \n    Args:\n        a: First number.\n        b: Second number.\n    \"\"\"\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_go_prepends_func_name() {
        let result = format_doc_comment("returns the sum of two numbers.", Language::Go, "", "Add");
        assert_eq!(result, "// Add returns the sum of two numbers.\n");
    }

    #[test]
    fn test_format_go_multiline() {
        let text = "returns the sum of two numbers.\n\nIt panics on overflow.";
        let result = format_doc_comment(text, Language::Go, "", "Add");
        assert_eq!(
            result,
            "// Add returns the sum of two numbers.\n// \n// It panics on overflow.\n"
        );
    }

    #[test]
    fn test_format_java_single_line() {
        let result =
            format_doc_comment("Returns the sum of two numbers.", Language::Java, "", "add");
        assert_eq!(result, "/**\n * Returns the sum of two numbers.\n */\n");
    }

    #[test]
    fn test_format_java_multiline_with_indent() {
        let text = "Returns the sum.\n\n@param a first number\n@param b second number";
        let result = format_doc_comment(text, Language::Java, "    ", "add");
        let expected = "    /**\n     * Returns the sum.\n     * \n     * @param a first number\n     * @param b second number\n     */\n";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_format_typescript_single_line() {
        let result = format_doc_comment(
            "Fetches data from the API.",
            Language::TypeScript,
            "",
            "fetchData",
        );
        assert_eq!(result, "/**\n * Fetches data from the API.\n */\n");
    }

    #[test]
    fn test_format_empty_text() {
        let result = format_doc_comment("", Language::Rust, "", "foo");
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_default_language() {
        // Bash gets default // format
        let result = format_doc_comment("Runs the deploy script.", Language::Bash, "", "deploy");
        assert_eq!(result, "// Runs the deploy script.\n");
    }

    #[test]
    fn test_format_elixir() {
        let result =
            format_doc_comment("Adds two numbers together.", Language::Elixir, "  ", "add");
        assert_eq!(
            result,
            "  @doc \"\"\"\n  Adds two numbers together.\n  \"\"\"\n"
        );
    }

    #[test]
    fn test_format_ocaml() {
        let result =
            format_doc_comment("Computes the factorial.", Language::OCaml, "", "factorial");
        assert_eq!(result, "(** \nComputes the factorial.\n *)\n");
    }

    #[test]
    fn test_format_haskell() {
        let result = format_doc_comment(
            "Maps a function over a list.",
            Language::Haskell,
            "",
            "mapF",
        );
        assert_eq!(result, "-- | Maps a function over a list.\n");
    }

    #[test]
    fn test_format_ruby() {
        let result = format_doc_comment(
            "Initializes the connection pool.",
            Language::Ruby,
            "  ",
            "initialize",
        );
        assert_eq!(result, "  # Initializes the connection pool.\n");
    }

    #[test]
    fn test_format_preserves_internal_indentation() {
        // Python text with its own indentation (e.g., Args section)
        let text = "Short summary.\n\nArgs:\n    x: The input value.";
        let result = format_doc_comment(text, Language::Rust, "", "foo");
        // Each line gets /// prefix; internal indentation is preserved
        assert_eq!(
            result,
            "/// Short summary.\n/// \n/// Args:\n///     x: The input value.\n"
        );
    }
}
