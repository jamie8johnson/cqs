//! LaTeX language definition
//!
//! LaTeX is a document preparation system. Chunks are sections (chapter, section,
//! subsection), command definitions, and environments. No call graph.

use super::{ChunkType, FieldStyle, InjectionRule, LanguageDef, SignatureStyle};

/// Tree-sitter query for extracting LaTeX definitions as chunks.
///
/// Captures:
/// - Sectioning commands: \chapter, \section, \subsection, etc.
/// - Command definitions: \newcommand, \renewcommand, etc.
/// - Environments: \begin{name}...\end{name}
const CHUNK_QUERY: &str = r#"
;; Part
(part
  text: (curly_group) @name) @section

;; Chapter
(chapter
  text: (curly_group) @name) @section

;; Section
(section
  text: (curly_group) @name) @section

;; Subsection
(subsection
  text: (curly_group) @name) @section

;; Subsubsection
(subsubsection
  text: (curly_group) @name) @section

;; Paragraph (LaTeX \paragraph{})
(paragraph
  text: (curly_group) @name) @section

;; New command definitions (declaration in curly group)
(new_command_definition
  declaration: (curly_group_command_name) @name) @function

;; New command definitions (bare command name)
(new_command_definition
  declaration: (command_name) @name) @function

;; Old-style command definitions (\def)
(old_command_definition
  declaration: (command_name) @name) @function

;; Named environments
(generic_environment
  begin: (begin
    name: (curly_group_text) @name)) @struct
"#;

/// Doc comment node types — LaTeX uses `% comments`
const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "begin", "end", "documentclass", "usepackage", "input", "include", "label", "ref",
    "cite", "bibliography", "maketitle", "tableofcontents", "textbf", "textit", "emph",
    "item", "hline", "vspace", "hspace", "newline", "newpage", "par",
];

/// Map minted/lstlisting language names to cqs language identifiers.
///
/// Returns `None` if the language name maps to the default target,
/// `Some("_skip")` if unrecognized, or `Some(lang)` for a specific language.
fn map_code_language(lang: &str) -> Option<&'static str> {
    match lang.to_lowercase().as_str() {
        "python" | "python3" | "py" => Some("python"),
        "rust" => Some("rust"),
        "c" => Some("c"),
        "cpp" | "c++" => Some("cpp"),
        "java" => Some("java"),
        "javascript" | "js" => Some("javascript"),
        "typescript" | "ts" => Some("typescript"),
        "go" | "golang" => Some("go"),
        "bash" | "sh" | "shell" => Some("bash"),
        "ruby" | "rb" => Some("ruby"),
        "sql" => Some("sql"),
        "haskell" | "hs" => Some("haskell"),
        "lua" => Some("lua"),
        "scala" => Some("scala"),
        "r" => Some("r"),
        _ => {
            tracing::debug!(language = lang, "Unrecognized code listing language, skipping");
            Some("_skip")
        }
    }
}

/// Detect code language from a `minted_environment` node.
///
/// Checks the `begin` child's `language` field (`\begin{minted}{python}`).
fn detect_minted_language(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "begin" {
            // Look for the language field (curly_group_text)
            let mut begin_cursor = child.walk();
            let mut found_name = false;
            for begin_child in child.children(&mut begin_cursor) {
                if begin_child.kind() == "curly_group_text" {
                    if !found_name {
                        // First curly_group_text is the environment name (minted)
                        found_name = true;
                        continue;
                    }
                    // Second curly_group_text is the language
                    let text = source[begin_child.byte_range()].trim();
                    // Strip braces: {python} → python
                    let lang = text
                        .strip_prefix('{')
                        .and_then(|s| s.strip_suffix('}'))
                        .unwrap_or(text)
                        .trim();
                    if !lang.is_empty() {
                        tracing::debug!(language = lang, "Minted environment language detected");
                        return map_code_language(lang);
                    }
                }
            }
        }
    }
    Some("_skip")
}

/// Detect code language from a `listing_environment` node.
///
/// The LaTeX grammar includes `[language=X]` options in the `source_code`
/// content (not as a parsed `begin` attribute). This function checks the
/// `source_code` content prefix for `[language=X]`.
fn detect_listing_language(node: tree_sitter::Node, source: &str) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "source_code" {
            let text = &source[child.byte_range()];
            let trimmed = text.trim_start();
            // Check for [language=X] prefix
            if trimmed.starts_with('[') {
                let text_lower = trimmed.to_ascii_lowercase();
                if let Some(pos) = text_lower.find("language=") {
                    let after = &trimmed[pos + 9..];
                    let lang: String = after
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '+')
                        .collect();
                    if !lang.is_empty() {
                        tracing::debug!(
                            language = %lang,
                            "Listing environment language detected"
                        );
                        return map_code_language(&lang);
                    }
                }
            }
        }
    }
    // No language option found — skip (don't guess)
    Some("_skip")
}

/// Post-process LaTeX chunks: clean up names by stripping braces and backslashes.
fn post_process_latex(
    name: &mut String,
    _chunk_type: &mut ChunkType,
    _node: tree_sitter::Node,
    _source: &str,
) -> bool {
    // Strip surrounding braces from curly_group captures: {Title} → Title
    if name.starts_with('{') && name.ends_with('}') {
        *name = name[1..name.len() - 1].trim().to_string();
    }
    // Strip leading backslash from command names: \mycommand → mycommand
    if name.starts_with('\\') {
        *name = name[1..].to_string();
    }
    // Skip empty names
    !name.is_empty()
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "latex",
    grammar: Some(|| tree_sitter_latex::LANGUAGE.into()),
    extensions: &["tex", "sty", "cls"],
    chunk_query: CHUNK_QUERY,
    call_query: None,
    signature_style: SignatureStyle::FirstLine,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[],
    stopwords: STOPWORDS,
    extract_return_nl: |_| None,
    test_file_suggestion: None,
    test_name_suggestion: None,
    type_query: None,
    common_types: &[],
    container_body_kinds: &[],
    extract_container_name: None,
    extract_qualified_method: None,
    post_process_chunk: Some(post_process_latex),
    test_markers: &[],
    test_path_patterns: &[],
    structural_matchers: None,
    entry_point_names: &[],
    trait_method_names: &[],
    injections: &[
        // \begin{minted}{python} ... \end{minted} — language from argument
        InjectionRule {
            container_kind: "minted_environment",
            content_kind: "source_code",
            target_language: "python", // default, overridden by detect_minted_language
            detect_language: Some(detect_minted_language),
            content_scoped_lines: false,
        },
        // \begin{lstlisting}[language=Python] ... \end{lstlisting}
        InjectionRule {
            container_kind: "listing_environment",
            content_kind: "source_code",
            target_language: "c", // default, overridden by detect_listing_language
            detect_language: Some(detect_listing_language),
            content_scoped_lines: false,
        },
    ],
    doc_format: "default",
    doc_convention: "",
    field_style: FieldStyle::None,
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
    /// Tests that the parser correctly identifies and extracts LaTeX document sections and subsections.
    /// 
    /// This test creates a temporary LaTeX file containing multiple sections and subsections, parses it using the Parser, and verifies that all expected sections ("Introduction", "Background", "Methods") are extracted as chunks with their correct types and names.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts test conditions and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if any assertion fails, including:
    /// - If the expected "Introduction", "Background", or "Methods" sections are not found in the parsed chunks
    /// - If the "Introduction" chunk does not have the correct ChunkType::Section type

    #[test]
    fn parse_latex_sections() {
        let content = r#"\documentclass{article}
\begin{document}

\section{Introduction}
This is the introduction.

\subsection{Background}
Some background information.

\section{Methods}
The methods section.

\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"Introduction"),
            "Expected 'Introduction' section, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Background"),
            "Expected 'Background' subsection, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Methods"),
            "Expected 'Methods' section, got: {:?}",
            names
        );
        let intro = chunks.iter().find(|c| c.name == "Introduction").unwrap();
        assert_eq!(intro.chunk_type, ChunkType::Section);
    }
    /// Parses a LaTeX file containing custom command definitions and verifies they are correctly extracted.
    /// 
    /// This is a test function that creates a temporary LaTeX file with `\newcommand` definitions, parses it using the Parser, and asserts that the custom commands (`highlight` and `todo`) are properly recognized and classified as functions in the resulting chunks.
    /// 
    /// # Arguments
    /// 
    /// None. This function operates on hardcoded LaTeX content.
    /// 
    /// # Panics
    /// 
    /// Panics if:
    /// - The temporary file cannot be created
    /// - The parser initialization fails
    /// - File parsing fails
    /// - The expected `highlight` or `todo` commands are not found in parsed chunks
    /// - The `highlight` command is not classified as `ChunkType::Function`

    #[test]
    fn parse_latex_command_definition() {
        let content = r#"\documentclass{article}

\newcommand{\highlight}[1]{\textbf{#1}}
\newcommand{\todo}[1]{\textcolor{red}{TODO: #1}}

\begin{document}
\highlight{Important text}
\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"highlight"),
            "Expected 'highlight' command, got: {:?}",
            names
        );
        assert!(
            names.contains(&"todo"),
            "Expected 'todo' command, got: {:?}",
            names
        );
        let cmd = chunks.iter().find(|c| c.name == "highlight").unwrap();
        assert_eq!(cmd.chunk_type, ChunkType::Function);
    }
    /// Parses LaTeX environments from a temporary file and verifies that theorem and proof environments are correctly identified.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data internally.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to validate parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, fails to parse the file, or if any of the assertions fail (missing expected environments or incorrect chunk type classification).

    #[test]
    fn parse_latex_environment() {
        let content = r#"\documentclass{article}
\begin{document}

\begin{theorem}
Every even integer greater than 2 can be expressed as the sum of two primes.
\end{theorem}

\begin{proof}
This is left as an exercise.
\end{proof}

\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        let names: Vec<_> = chunks.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"theorem"),
            "Expected 'theorem' environment, got: {:?}",
            names
        );
        assert!(
            names.contains(&"proof"),
            "Expected 'proof' environment, got: {:?}",
            names
        );
        let thm = chunks.iter().find(|c| c.name == "theorem").unwrap();
        assert_eq!(thm.chunk_type, ChunkType::Struct);
    }

    // --- Injection tests ---
    /// Verifies that the parser correctly extracts code blocks from LaTeX minted environments and preserves LaTeX section structure.
    /// 
    /// This is a unit test that validates the parser's ability to:
    /// 1. Recognize `\begin{minted}{language}` blocks in LaTeX documents
    /// 2. Extract the code content with the correct language designation (Python in this case)
    /// 3. Parse nested code structures like function and class definitions
    /// 4. Maintain LaTeX sections alongside extracted code chunks
    /// 
    /// # Arguments
    /// 
    /// None. This is a self-contained test function that creates its own test data and parser instance.
    /// 
    /// # Returns
    /// 
    /// None. This function is a test assertion that either passes silently or panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to extract the Python function `greet` from the minted block, or if the LaTeX section `Code Example` is not found in the parsed chunks.

    #[test]
    fn parse_latex_minted_extracts_code() {
        // \begin{minted}{python} should inject Python
        let content = r#"\documentclass{article}
\usepackage{minted}
\begin{document}

\section{Code Example}

\begin{minted}{python}
def greet(name):
    return f"Hello, {name}!"

class Calculator:
    def add(self, a, b):
        return a + b
\end{minted}

\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Python chunks should be extracted
        let py_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Python)
            .collect();
        assert!(
            py_chunks.iter().any(|c| c.name == "greet"),
            "Expected Python function 'greet' from minted block, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );

        // LaTeX section should still exist
        assert!(
            chunks.iter().any(|c| c.name == "Code Example"),
            "Expected LaTeX section 'Code Example'"
        );
    }
    /// Verifies that the parser correctly extracts code chunks from LaTeX lstlisting environments with language specifications.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function asserts expected behavior and panics on failure.
    /// 
    /// # Panics
    /// 
    /// Panics if no Rust language chunks are extracted from a LaTeX document containing a `\begin{lstlisting}[language=Rust]` block, or if file operations fail.

    #[test]
    fn parse_latex_listing_extracts_code() {
        // \begin{lstlisting}[language=Rust] should inject Rust
        let content = r#"\documentclass{article}
\usepackage{listings}
\begin{document}

\section{Rust Example}

\begin{lstlisting}[language=Rust]
fn main() {
    println!("Hello, world!");
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}
\end{lstlisting}

\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Rust chunks should be extracted
        let rust_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Rust)
            .collect();
        assert!(
            !rust_chunks.is_empty(),
            "Expected Rust chunks from lstlisting[language=Rust], got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
    }
    /// Tests that a LaTeX file without code listings is parsed as a single LaTeX chunk with no code injection.
    /// 
    /// # Arguments
    /// 
    /// This is a test function with no parameters.
    /// 
    /// # Returns
    /// 
    /// Returns nothing. The function performs assertions to verify expected behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if any assertion fails, specifically if any parsed chunk is not identified as LaTeX language, or if file operations fail unexpectedly.

    #[test]
    fn parse_latex_without_listings_unchanged() {
        // LaTeX file with no code listings — injection should not fire
        let content = r#"\documentclass{article}
\begin{document}
\section{Introduction}
Hello world.
\section{Methods}
Some methods.
\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        for chunk in &chunks {
            assert_eq!(
                chunk.language,
                crate::parser::Language::Latex,
                "File without code listings should only have LaTeX chunks"
            );
        }
    }
    /// Tests that the parser correctly identifies a LaTeX document with no function calls.
    /// 
    /// # Arguments
    /// 
    /// None. This is a test function that creates its own test data.
    /// 
    /// # Returns
    /// 
    /// None. This function performs assertions to verify parser behavior.
    /// 
    /// # Panics
    /// 
    /// Panics if the parser fails to initialize, fails to parse the test file, or if any extracted chunks contain function calls when none are expected.

    #[test]
    fn parse_latex_no_calls() {
        let content = r#"\documentclass{article}
\begin{document}
\section{Test}
Hello world.
\end{document}
"#;
        let file = write_temp_file(content, "tex");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();
        for chunk in &chunks {
            let calls = parser.extract_calls_from_chunk(chunk);
            assert!(calls.is_empty(), "LaTeX should have no calls");
        }
    }
}
