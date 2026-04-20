//! Fenced code block extraction and language detection
//!
//! Scans for `` ```lang `` and `~~~lang` markers, normalizes language aliases
//! (e.g., `js` -> `javascript`), and returns blocks with recognized languages.

use std::collections::HashMap;
use std::sync::LazyLock;

/// A fenced code block found in markdown source
#[derive(Debug)]
pub struct FencedBlock {
    /// Language identifier from the fence (e.g., "rust", "js", "python")
    pub lang: String,
    /// Content inside the fence (excluding the ``` markers)
    pub content: String,
    /// 1-indexed line number of the opening fence
    pub line_start: u32,
    /// 1-indexed line number of the closing fence
    pub line_end: u32,
}

/// Plain-text fenced-block alias map, derived from the language registry.
///
/// Iterates `crate::language::REGISTRY.all()`, inserting `(canonical_name →
/// canonical_name)` plus `(alias → canonical_name)` for each `def.aliases`
/// entry. Adding a `Language` variant with an `aliases: &["foo"]` row in
/// `languages.rs` propagates here automatically — no parallel table to keep
/// in sync.
static FENCED_LANG_ALIASES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for def in crate::language::REGISTRY.all() {
        m.insert(def.name, def.name);
        for alias in def.aliases {
            m.insert(*alias, def.name);
        }
    }
    m
});

/// Common language aliases in markdown fenced code blocks.
///
/// Resolves an incoming fence tag (e.g., `js`, `c++`) to a canonical
/// `Language` name for downstream parsing. Returns `None` for unknown tags
/// or for tags whose canonical name is not a `Language` variant.
pub(super) fn normalize_lang(lang: &str) -> Option<&'static str> {
    FENCED_LANG_ALIASES.get(lang).copied()
}

/// Extract fenced code blocks from markdown source.
/// Scans for `` ```lang `` and `~~~lang` markers, returning blocks with
/// recognized language identifiers. Blocks without a language tag or with
/// unrecognized languages are skipped.
pub fn extract_fenced_blocks(source: &str) -> Vec<FencedBlock> {
    let _span = tracing::debug_span!("extract_fenced_blocks").entered();
    let lines: Vec<&str> = source.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        // Check for opening fence
        let (fence_char, fence_len) = if trimmed.starts_with("```") {
            ('`', trimmed.bytes().take_while(|&b| b == b'`').count())
        } else if trimmed.starts_with("~~~") {
            ('~', trimmed.bytes().take_while(|&b| b == b'~').count())
        } else {
            i += 1;
            continue;
        };

        if fence_len < 3 {
            i += 1;
            continue;
        }

        // Extract language tag (everything after the fence chars, trimmed)
        let lang_raw = trimmed[fence_len..].trim();
        // Strip anything after whitespace (e.g., "python title='example'" -> "python")
        let lang_tag = lang_raw.split_whitespace().next().unwrap_or("");

        let normalized = normalize_lang(&lang_tag.to_ascii_lowercase());
        let open_line = i;
        i += 1;

        // Find closing fence (same char, at least same length)
        let content_start = i;
        while i < lines.len() {
            let close_trimmed = lines[i].trim();
            let is_close = if fence_char == '`' {
                close_trimmed.starts_with("```")
                    && close_trimmed.bytes().take_while(|&b| b == b'`').count() >= fence_len
                    && close_trimmed.trim_start_matches('`').trim().is_empty()
            } else {
                close_trimmed.starts_with("~~~")
                    && close_trimmed.bytes().take_while(|&b| b == b'~').count() >= fence_len
                    && close_trimmed.trim_start_matches('~').trim().is_empty()
            };

            if is_close {
                if let Some(lang) = normalized {
                    let content = lines[content_start..i].join("\n");
                    if !content.trim().is_empty() {
                        blocks.push(FencedBlock {
                            lang: lang.to_string(),
                            content,
                            line_start: open_line as u32 + 1,
                            line_end: i as u32 + 1,
                        });
                    }
                }
                i += 1;
                break;
            }
            i += 1;
        }

        // Unclosed fence -- rest of file consumed without finding closing fence
        if i >= lines.len() {
            tracing::debug!(
                line = open_line + 1,
                lang = ?normalized,
                "Unclosed fenced code block (no closing fence found)"
            );
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_fenced_blocks_basic() {
        let source = "# Example\n\n```rust\nfn hello() {}\n```\n\nSome text.\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "rust");
        assert_eq!(blocks[0].content, "fn hello() {}");
        assert_eq!(blocks[0].line_start, 3); // 1-indexed line of opening fence
        assert_eq!(blocks[0].line_end, 5); // 1-indexed line of closing fence
    }

    /// Verify normalize_lang covers all Language variants that have grammars.
    /// If this fails after adding a new language, add a mapping in normalize_lang().
    #[test]
    fn test_normalize_lang_covers_all_languages() {
        use crate::parser::Language;

        // These languages have no grammar (custom parser) -- normalize_lang should still map them
        // but they won't produce tree-sitter chunks. Just verify the mapping exists.
        let exceptions: &[Language] = &[];

        for lang in Language::all_variants() {
            if exceptions.contains(lang) {
                continue;
            }
            let name_lower = lang.to_string().to_ascii_lowercase();
            let result = normalize_lang(&name_lower);
            assert!(
                result.is_some(),
                "normalize_lang({:?}) returned None -- add a mapping for Language::{}",
                name_lower,
                lang
            );
        }
    }

    /// Pin that the registry-driven alias list is populated for the languages
    /// most likely to appear as fence tags. Without this, an alias regression
    /// in `languages.rs` would only be caught if it dropped the canonical
    /// name itself.
    #[test]
    fn aliases_populated_for_common_languages() {
        use crate::parser::Language;
        assert!(
            Language::Python.def().aliases.contains(&"py"),
            "Python must register the `py` alias",
        );
        assert!(
            Language::Go.def().aliases.contains(&"golang"),
            "Go must register the `golang` alias",
        );
        assert!(
            Language::TypeScript.def().aliases.contains(&"ts"),
            "TypeScript must register the `ts` alias",
        );
    }

    /// Regression pin: `normalize_lang` resolves a representative alias subset
    /// to the correct canonical name. If the registry conversion drifts (a
    /// language drops an alias, or `normalize_lang` is rewired wrong), this
    /// surfaces immediately.
    #[test]
    fn fenced_lang_aliases_match_legacy_table() {
        for (alias, canonical) in [
            ("py", "python"),
            ("ts", "typescript"),
            ("js", "javascript"),
            ("golang", "go"),
            ("c++", "cpp"),
            ("cs", "csharp"),
            ("kt", "kotlin"),
            ("ml", "ocaml"),
            ("rb", "ruby"),
            ("yml", "yaml"),
            ("md", "markdown"),
            ("tf", "hcl"),
            ("proto", "protobuf"),
            ("gql", "graphql"),
            ("hs", "haskell"),
            ("sol", "solidity"),
            ("cu", "cuda"),
        ] {
            assert_eq!(
                normalize_lang(alias),
                Some(canonical),
                "alias `{alias}` must resolve to `{canonical}`",
            );
        }
    }

    #[test]
    fn test_extract_fenced_blocks_aliases() {
        let source = "```js\nconst x = 1;\n```\n\n```py\ndef foo(): pass\n```\n\n```ts\nconst y: number = 2;\n```\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].lang, "javascript");
        assert_eq!(blocks[1].lang, "python");
        assert_eq!(blocks[2].lang, "typescript");
    }

    #[test]
    fn test_extract_fenced_blocks_unknown_lang() {
        let source = "```unknown\nsome code\n```\n\n```\nno lang\n```\n";
        let blocks = extract_fenced_blocks(source);
        assert!(blocks.is_empty(), "Unknown languages should be skipped");
    }

    #[test]
    fn test_extract_fenced_blocks_tilde() {
        let source = "~~~python\ndef bar(): pass\n~~~\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "python");
    }

    #[test]
    fn test_extract_fenced_blocks_with_metadata() {
        // Some markdown processors allow metadata after the language tag
        let source = "```python title='example'\ndef baz(): pass\n```\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "python");
    }

    #[test]
    fn test_extract_fenced_blocks_empty() {
        let source = "```rust\n```\n";
        let blocks = extract_fenced_blocks(source);
        assert!(blocks.is_empty(), "Empty blocks should be skipped");
    }

    #[test]
    fn test_fenced_blocks_parsed_as_chunks() {
        use crate::parser::Parser;
        use std::io::Write;

        let content = "# API Reference\n\n```rust\nfn calculate_sum(a: i32, b: i32) -> i32 {\n    a + b\n}\n\nfn multiply(x: f64, y: f64) -> f64 {\n    x * y\n}\n```\n\nSome explanation.\n";
        let mut f = tempfile::Builder::new().suffix(".md").tempfile().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();

        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(f.path()).unwrap();

        // Should have markdown section chunks + Rust function chunks
        let rust_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Rust)
            .collect();
        assert!(
            rust_chunks.iter().any(|c| c.name == "calculate_sum"),
            "Expected Rust function 'calculate_sum' from fenced block, got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );
        assert!(
            rust_chunks.iter().any(|c| c.name == "multiply"),
            "Expected Rust function 'multiply' from fenced block"
        );

        // Line numbers should be adjusted to markdown file position
        let calc = rust_chunks
            .iter()
            .find(|c| c.name == "calculate_sum")
            .unwrap();
        assert!(
            calc.line_start >= 4,
            "calculate_sum should start at or after line 4, got {}",
            calc.line_start
        );
    }

    #[test]
    fn test_fenced_blocks_multiple_languages() {
        use crate::parser::Parser;
        use std::io::Write;

        let content = "# Examples\n\n```python\ndef greet(name):\n    return f'Hello {name}'\n```\n\n```javascript\nfunction add(a, b) {\n    return a + b;\n}\n```\n";
        let mut f = tempfile::Builder::new().suffix(".md").tempfile().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();

        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(f.path()).unwrap();

        let py_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::Python)
            .collect();
        assert!(
            py_chunks.iter().any(|c| c.name == "greet"),
            "Expected Python function 'greet', got: {:?}",
            chunks
                .iter()
                .map(|c| (&c.name, &c.language))
                .collect::<Vec<_>>()
        );

        let js_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.language == crate::parser::Language::JavaScript)
            .collect();
        assert!(
            js_chunks.iter().any(|c| c.name == "add"),
            "Expected JavaScript function 'add'"
        );
    }

    // TC-3: extract_fenced_blocks edge case tests

    #[test]
    fn test_extract_fenced_blocks_unclosed() {
        let source = "```rust\nfn foo() {}\n";
        let blocks = extract_fenced_blocks(source);
        // Unclosed fences are skipped (no matching closing fence)
        assert_eq!(blocks.len(), 0);
    }

    #[test]
    fn test_extract_fenced_blocks_nested_longer_fence() {
        // 4-backtick fence containing a 3-backtick fence
        let source = "````rust\nfn outer() {\n```\ninner\n```\n}\n````\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(
            blocks.len(),
            1,
            "Nested shorter fence should not close outer"
        );
        assert!(blocks[0].content.contains("inner"));
    }

    #[test]
    fn test_extract_fenced_blocks_mixed_fence_types() {
        // Backtick open + tilde close should NOT close
        let source = "```rust\nfn foo() {}\n~~~\nmore\n```\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(blocks.len(), 1);
        // Tilde line should be included in content (doesn't close backtick fence)
        assert!(blocks[0].content.contains("~~~"));
    }

    #[test]
    fn test_extract_fenced_blocks_indented() {
        let source = "  ```python\n  def foo(): pass\n  ```\n";
        let blocks = extract_fenced_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "python");
    }
}
