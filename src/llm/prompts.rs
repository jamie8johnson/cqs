//! Prompt construction for LLM summary, doc comment, and HyDE passes.

use super::{Client, MAX_CONTENT_CHARS};

impl Client {
    /// Build the prompt for a code chunk.
    pub(super) fn build_prompt(content: &str, chunk_type: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
        } else {
            content
        };
        format!(
            "Describe what makes this {} unique and distinguishable from similar {}s. \
             Focus on the specific algorithm, approach, or behavioral characteristics \
             that distinguish it. One sentence only. Be specific, not generic.\n\n```{}\n{}\n```",
            chunk_type, chunk_type, language, truncated
        )
    }

    /// Build the prompt for generating a doc comment for a code chunk.
    ///
    /// Unlike `build_prompt` (one-sentence summary), this generates a full documentation
    /// comment with language-specific conventions (Rust `# Arguments`/`# Returns`, Python
    /// Google-style docstrings, Go function-name-first, etc.).
    pub(super) fn build_doc_prompt(content: &str, chunk_type: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
        } else {
            content
        };

        // EX-15: Language-specific doc comment conventions
        let appendix = match language {
            "rust" => "\n\nUse `# Arguments`, `# Returns`, `# Errors`, `# Panics` sections as appropriate.",
            "python" => "\n\nFormat as a Google-style docstring (Args/Returns/Raises sections).",
            "go" => "\n\nStart with the function name per Go conventions.",
            "java" => "\n\nUse Javadoc format: @param, @return, @throws tags.",
            "csharp" => "\n\nUse XML doc comments: <summary>, <param>, <returns>, <exception> tags.",
            "typescript" | "javascript" => "\n\nUse JSDoc format: @param {type} name, @returns {type}, @throws {type}.",
            _ => "",
        };

        format!(
            "Write a concise doc comment for this {}. \
             Describe what it does, its parameters, and return value. \
             Output only the doc text, no code fences or comment markers.{}\n\n\
             ```{}\n{}\n```",
            chunk_type, appendix, language, truncated
        )
    }

    /// Build the prompt for HyDE query prediction.
    ///
    /// Given a function's content, signature, and language, produces a prompt that
    /// asks the LLM to generate 3-5 search queries a developer would use to find
    /// this function.
    pub(super) fn build_hyde_prompt(content: &str, signature: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
        } else {
            content
        };
        format!(
            "You are a code search query predictor. Given a function, output 3-5 short search \
             queries a developer would type to find this function. One query per line. No \
             numbering, no explanation. Queries should be natural language, not code.\n\n\
             Language: {}\nSignature: {}\n\n{}",
            language, signature, truncated
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prompt() {
        let prompt = Client::build_prompt("fn foo() {}", "function", "rust");
        assert!(prompt.contains("function"));
        assert!(prompt.contains("```rust"));
        assert!(prompt.contains("fn foo()"));
    }

    #[test]
    fn test_build_prompt_truncation() {
        let long = "x".repeat(10000);
        let prompt = Client::build_prompt(&long, "function", "rust");
        // Prompt should contain truncated content
        assert!(prompt.len() < 10000 + 200); // prompt overhead + truncated
    }

    #[test]
    fn build_prompt_multibyte_no_panic() {
        let content: String = std::iter::repeat('あ').take(2667).collect();
        let prompt = Client::build_prompt(&content, "function", "rust");
        assert!(prompt.len() <= 8300); // discriminating prompt is slightly longer
    }

    // ===== build_doc_prompt tests =====

    #[test]
    fn test_build_doc_prompt_rust() {
        let prompt =
            Client::build_doc_prompt("fn foo() -> Result<(), Error> {}", "function", "rust");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```rust"));
        assert!(prompt.contains("# Arguments"));
        assert!(prompt.contains("# Returns"));
        assert!(prompt.contains("# Errors"));
        assert!(prompt.contains("# Panics"));
    }

    #[test]
    fn test_build_doc_prompt_python() {
        let prompt = Client::build_doc_prompt("def foo(x: int) -> str:", "function", "python");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```python"));
        assert!(prompt.contains("Google-style docstring"));
        assert!(prompt.contains("Args/Returns/Raises"));
    }

    #[test]
    fn test_build_doc_prompt_go() {
        let prompt = Client::build_doc_prompt("func Foo() error {}", "function", "go");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```go"));
        assert!(prompt.contains("function name per Go conventions"));
    }

    #[test]
    fn test_build_doc_prompt_default() {
        // Use a language with no specific appendix
        let prompt = Client::build_doc_prompt("defmodule Foo do end", "module", "elixir");
        assert!(prompt.contains("doc comment"));
        assert!(prompt.contains("```elixir"));
        // No language-specific appendix for elixir
        assert!(!prompt.contains("# Arguments"));
        assert!(!prompt.contains("Google-style"));
        assert!(!prompt.contains("Go conventions"));
        assert!(!prompt.contains("JSDoc"));
        assert!(!prompt.contains("Javadoc"));
    }

    #[test]
    fn test_build_doc_prompt_truncation() {
        let long = "x".repeat(10000);
        let prompt = Client::build_doc_prompt(&long, "function", "rust");
        assert!(prompt.len() < 10000 + 300);
    }

    // EX-15: Language-specific appendices for Java, C#, TypeScript, JavaScript
    #[test]
    fn test_build_doc_prompt_java() {
        let prompt = Client::build_doc_prompt("public void foo() {}", "method", "java");
        assert!(prompt.contains("Javadoc"));
        assert!(prompt.contains("@param"));
    }

    #[test]
    fn test_build_doc_prompt_csharp() {
        let prompt = Client::build_doc_prompt("public void Foo() {}", "method", "csharp");
        assert!(prompt.contains("XML doc"));
        assert!(prompt.contains("<summary>"));
    }

    #[test]
    fn test_build_doc_prompt_typescript() {
        let prompt =
            Client::build_doc_prompt("function foo(): string {}", "function", "typescript");
        assert!(prompt.contains("JSDoc"));
        assert!(prompt.contains("@param"));
    }

    #[test]
    fn test_build_doc_prompt_javascript() {
        let prompt = Client::build_doc_prompt("function foo() {}", "function", "javascript");
        assert!(prompt.contains("JSDoc"));
        assert!(prompt.contains("@param"));
    }

    // TC-2: build_hyde_prompt
    #[test]
    fn test_build_hyde_prompt_basic() {
        let prompt = Client::build_hyde_prompt(
            "fn search(query: &str) -> Vec<Result> { ... }",
            "fn search(query: &str) -> Vec<Result>",
            "rust",
        );
        assert!(prompt.contains("search query predictor"));
        assert!(prompt.contains("3-5 short search"));
        assert!(prompt.contains("Language: rust"));
        assert!(prompt.contains("fn search"));
    }

    #[test]
    fn test_build_hyde_prompt_truncation() {
        let long_content = "x".repeat(10000);
        let prompt = Client::build_hyde_prompt(&long_content, "fn big()", "rust");
        assert!(prompt.len() < 10000 + 300, "Should truncate long content");
    }
}
