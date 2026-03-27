//! FTS normalization and identifier tokenization.

/// Returns true for CJK Unified Ideographs and common CJK ranges.
/// Covers Chinese, Japanese kanji, Korean hanja, and extensions.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}' // CJK Extension A
        | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        | '\u{3000}'..='\u{303F}' // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{309F}' // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        | '\u{1100}'..='\u{11FF}' // Hangul Jamo
    )
}

/// Split identifier on snake_case and camelCase boundaries.
///
/// Note: This function splits on every uppercase letter, so acronyms like
/// "XMLParser" become individual letters. This is intentional for search
/// tokenization where "xml parser" is more useful than preserving "XML".
///
/// # Examples
///
/// ```ignore
/// use cqs::nl::tokenize_identifier;
///
/// assert_eq!(tokenize_identifier("parseConfigFile"), vec!["parse", "config", "file"]);
/// assert_eq!(tokenize_identifier("get_user_name"), vec!["get", "user", "name"]);
/// assert_eq!(tokenize_identifier("XMLParser"), vec!["x", "m", "l", "parser"]); // acronyms split per-letter
/// assert_eq!(tokenize_identifier("获取用户"), vec!["获", "取", "用", "户"]); // CJK: one token per character
/// ```
pub fn tokenize_identifier(s: &str) -> Vec<String> {
    tokenize_identifier_iter(s).collect()
}

/// Iterator-based tokenize_identifier for streaming - avoids intermediate Vec allocation
pub(super) fn tokenize_identifier_iter(s: &str) -> impl Iterator<Item = String> + '_ {
    TokenizeIdentifierIter {
        chars: s.chars().peekable(),
        current: String::new(),
        done: false,
    }
}

struct TokenizeIdentifierIter<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    current: String,
    done: bool,
}

impl<'a> Iterator for TokenizeIdentifierIter<'a> {
    type Item = String;

    /// Retrieves the next token from the input string.
    ///
    /// Splits the input into tokens by treating underscores, hyphens, and spaces as delimiters. CJK (Chinese, Japanese, Korean) characters are emitted as individual tokens. Uppercase letters trigger token boundaries and are converted to lowercase. All other characters are converted to lowercase and accumulated into the current token.
    ///
    /// # Returns
    ///
    /// `Some(String)` containing the next token, or `None` if no more tokens are available.
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        loop {
            match self.chars.next() {
                Some(c) if c == '_' || c == '-' || c == ' ' => {
                    if !self.current.is_empty() {
                        return Some(std::mem::take(&mut self.current));
                    }
                }
                Some(c) if is_cjk(c) => {
                    // CJK characters become individual tokens
                    if !self.current.is_empty() {
                        // Stash the CJK char for next iteration by pushing to current
                        // after yielding — but simpler to just yield current first,
                        // then handle CJK on next call. Use peekable workaround:
                        // Actually, we already consumed c. Flush current, return it,
                        // but we need to also emit c. Push c to current so it's yielded next.
                        let result = std::mem::take(&mut self.current);
                        self.current.push(c);
                        return Some(result);
                    }
                    return Some(c.to_string());
                }
                Some(c) if c.is_uppercase() && !self.current.is_empty() => {
                    let result = std::mem::take(&mut self.current);
                    self.current.push(c.to_lowercase().next().unwrap_or(c));
                    return Some(result);
                }
                Some(c) => {
                    self.current.push(c.to_lowercase().next().unwrap_or(c));
                }
                None => {
                    self.done = true;
                    if !self.current.is_empty() {
                        return Some(std::mem::take(&mut self.current));
                    }
                    return None;
                }
            }
        }
    }
}

/// Maximum output length for FTS normalization.
/// Prevents memory exhaustion from pathological inputs where tokenization
/// expands text (e.g., "ABCD" → "a b c d" doubles length).
const MAX_FTS_OUTPUT_LEN: usize = 16384;

/// Normalize code text for FTS5 indexing.
///
/// Splits identifiers on camelCase/snake_case boundaries and joins with spaces.
/// Used to make code searchable with natural language queries.
/// Output is capped at 16KB to prevent memory issues with pathological inputs.
///
/// # Security: FTS5 Injection Protection
///
/// This function provides implicit protection against FTS5 injection attacks.
/// By only emitting alphanumeric tokens joined by spaces, special FTS5 operators
/// like `OR`, `AND`, `NOT`, `NEAR`, `*`, `"`, `(`, `)` are neutralized:
/// - Operators in the input become separate tokens (e.g., "foo OR bar" -> "foo or bar")
/// - Quotes and parentheses are stripped entirely (only alphanumeric + underscore pass)
/// - The resulting output is safe for direct use in FTS5 MATCH queries
///
/// # Example
///
/// ```
/// use cqs::normalize_for_fts;
///
/// assert_eq!(normalize_for_fts("parseConfigFile"), "parse config file");
/// assert_eq!(normalize_for_fts("fn get_user() {}"), "fn get user");
/// ```
pub fn normalize_for_fts(text: &str) -> String {
    let mut result = String::new();
    let mut current_word = String::new();

    let flush_word = |word: &str, result: &mut String| {
        for token in tokenize_identifier_iter(word) {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(&token);
        }
    };

    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            current_word.push(c);
        } else if !current_word.is_empty() {
            flush_word(&current_word, &mut result);
            current_word.clear();

            // Cap output to prevent memory issues - truncate at last space boundary
            if result.len() >= MAX_FTS_OUTPUT_LEN {
                let boundary = result.floor_char_boundary(MAX_FTS_OUTPUT_LEN);
                let truncate_at = result[..boundary].rfind(' ').unwrap_or(boundary);
                result.truncate(truncate_at);
                return result;
            }
        }
    }
    if !current_word.is_empty() {
        flush_word(&current_word, &mut result);
    }

    // Final cap check - truncate at last space to avoid splitting words
    if result.len() > MAX_FTS_OUTPUT_LEN {
        let boundary = result.floor_char_boundary(MAX_FTS_OUTPUT_LEN);
        let truncate_at = result[..boundary].rfind(' ').unwrap_or(boundary);
        result.truncate(truncate_at);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_identifier() {
        assert_eq!(
            tokenize_identifier("parseConfigFile"),
            vec!["parse", "config", "file"]
        );
        assert_eq!(
            tokenize_identifier("get_user_name"),
            vec!["get", "user", "name"]
        );
        assert_eq!(tokenize_identifier("simple"), vec!["simple"]);
        assert_eq!(tokenize_identifier(""), Vec::<String>::new());
    }

    #[test]
    fn test_tokenize_identifier_cjk() {
        // Pure CJK: each character becomes its own token
        assert_eq!(
            tokenize_identifier("获取用户名"),
            vec!["获", "取", "用", "户", "名"]
        );
        // Mixed Latin + CJK
        assert_eq!(
            tokenize_identifier("get用户Name"),
            vec!["get", "用", "户", "name"]
        );
        // Japanese hiragana
        assert_eq!(
            tokenize_identifier("こんにちは"),
            vec!["こ", "ん", "に", "ち", "は"]
        );
        // Korean hangul
        assert_eq!(tokenize_identifier("사용자"), vec!["사", "용", "자"]);
        // CJK with underscores
        assert_eq!(
            tokenize_identifier("get_用户_name"),
            vec!["get", "用", "户", "name"]
        );
    }

    #[test]
    fn test_normalize_for_fts_cjk() {
        // CJK characters split into individual tokens
        assert_eq!(normalize_for_fts("获取用户名"), "获 取 用 户 名");
        // Mixed: CJK in a code context
        assert_eq!(normalize_for_fts("fn get_用户()"), "fn get 用 户");
    }

    #[test]
    fn test_normalize_for_fts_output_bounded() {
        // Pathological input: all uppercase chars tokenize to "a b c d ..."
        // which roughly doubles the length
        let long_upper = "A".repeat(20000);
        let result = normalize_for_fts(&long_upper);
        assert!(
            result.len() <= MAX_FTS_OUTPUT_LEN,
            "FTS output should be capped at {} but was {}",
            MAX_FTS_OUTPUT_LEN,
            result.len()
        );
    }

    #[test]
    fn test_normalize_for_fts_normal_input_unchanged() {
        // Normal inputs should work as expected
        assert_eq!(normalize_for_fts("hello"), "hello");
        assert_eq!(normalize_for_fts("HelloWorld"), "hello world");
        assert_eq!(normalize_for_fts("get_user_name"), "get user name");
    }

    #[test]
    fn test_normalize_for_fts_cjk_truncation_no_panic() {
        // CJK characters are 3 bytes each in UTF-8. Build a string of CJK chars
        // that exceeds MAX_FTS_OUTPUT_LEN so truncation triggers inside multi-byte chars.
        // Each CJK char becomes a separate token with spaces: "X Y Z ..." so
        // output length ~ 2*num_chars. Need enough to exceed 16384.
        let cjk_heavy: String = "获".repeat(10000);
        let result = normalize_for_fts(&cjk_heavy);
        assert!(
            result.len() <= MAX_FTS_OUTPUT_LEN,
            "CJK FTS output should be capped but was {}",
            result.len()
        );
        // Verify the result is valid UTF-8 (implicit — it's a String)
        // and doesn't end mid-character
        assert!(result.is_char_boundary(result.len()));
    }

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Fuzz: tokenize_identifier should never panic
            #[test]
            fn fuzz_tokenize_identifier_no_panic(input in "\\PC{0,200}") {
                let _ = tokenize_identifier(&input);
            }

            /// Fuzz: tokenize_identifier with identifier-like strings
            #[test]
            fn fuzz_tokenize_identifier_like(input in "[a-zA-Z_][a-zA-Z0-9_]{0,50}") {
                let result = tokenize_identifier(&input);
                // Result can be empty if input is all underscores/non-alpha
                // Just verify it doesn't panic and returns valid tokens
                for token in &result {
                    prop_assert!(!token.is_empty(), "Empty token in result");
                }
            }
        }
    }
}
