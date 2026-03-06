//! LaTeX language definition
//!
//! LaTeX is a document preparation system. Chunks are sections (chapter, section,
//! subsection), command definitions, and environments. No call graph.

use super::{ChunkType, LanguageDef, SignatureStyle};

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
