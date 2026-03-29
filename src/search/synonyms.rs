//! Query expansion via synonym mapping for FTS search.
//!
//! Expands abbreviated query tokens into OR-groups for FTS5, improving recall
//! when users search with abbreviations (e.g., "auth" finds "authentication").

use std::collections::HashMap;
use std::sync::LazyLock;

/// Static synonym map: abbreviation → list of expansions.
///
/// Each key maps to tokens that FTS should also match. The original token
/// is always included in the OR group (handled by `expand_query_for_fts`).
static SYNONYMS: LazyLock<HashMap<&'static str, &'static [&'static str]>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("auth", &["authentication", "authorize", "credential"][..]);
    m.insert("config", &["configuration", "settings"][..]);
    m.insert("cfg", &["configuration", "config", "settings"][..]);
    m.insert("err", &["error", "failure", "exception"][..]);
    m.insert("fn", &["function", "method"][..]);
    m.insert("func", &["function", "method"][..]);
    m.insert("init", &["initialize", "setup", "initialization"][..]);
    m.insert("parse", &["parsing", "deserialize", "decode"][..]);
    m.insert("req", &["request"][..]);
    m.insert("res", &["response", "result"][..]);
    m.insert("fmt", &["format", "formatting"][..]);
    m.insert("db", &["database", "storage"][..]);
    m.insert("ctx", &["context"][..]);
    m.insert("msg", &["message"][..]);
    m.insert("cmd", &["command"][..]);
    m.insert("buf", &["buffer"][..]);
    m.insert("str", &["string"][..]);
    m.insert("impl", &["implementation", "implement"][..]);
    m.insert("alloc", &["allocate", "allocation"][..]);
    m.insert("dealloc", &["deallocate", "free"][..]);
    m.insert("arg", &["argument", "parameter"][..]);
    m.insert("args", &["arguments", "parameters"][..]);
    m.insert("param", &["parameter", "argument"][..]);
    m.insert("params", &["parameters", "arguments"][..]);
    m.insert("iter", &["iterator", "iteration"][..]);
    m.insert("async", &["asynchronous"][..]);
    m.insert("sync", &["synchronous", "synchronize"][..]);
    m.insert("env", &["environment"][..]);
    m.insert("dir", &["directory", "folder"][..]);
    m.insert("deps", &["dependencies", "dependency"][..]);
    m.insert("repo", &["repository"][..]);
    m
});

/// Expand a single FTS-sanitized query string with synonym OR groups.
///
/// Tokens that have synonyms are replaced with `(token OR syn1 OR syn2)`.
/// Tokens without synonyms pass through unchanged.
///
/// Input must already be FTS-sanitized (no special chars). Output is safe for
/// FTS5 MATCH because we only inject known-safe alpha tokens inside OR groups.
pub fn expand_query_for_fts(sanitized_query: &str) -> String {
    let tokens: Vec<&str> = sanitized_query.split_whitespace().collect();
    if tokens.is_empty() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::with_capacity(tokens.len());
    for token in &tokens {
        let lower = token.to_lowercase();
        if let Some(synonyms) = SYNONYMS.get(lower.as_str()) {
            // Build OR group: (original OR syn1 OR syn2 ...)
            let mut group = format!("({}", token);
            for syn in *synonyms {
                group.push_str(" OR ");
                group.push_str(syn);
            }
            group.push(')');
            parts.push(group);
        } else {
            parts.push(token.to_string());
        }
    }

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_empty() {
        assert_eq!(expand_query_for_fts(""), "");
        assert_eq!(expand_query_for_fts("   "), "");
    }

    #[test]
    fn no_synonyms_passes_through() {
        assert_eq!(expand_query_for_fts("hello world"), "hello world");
    }

    #[test]
    fn single_synonym_expands() {
        let result = expand_query_for_fts("auth");
        assert!(result.contains("auth"));
        assert!(result.contains("authentication"));
        assert!(result.contains("authorize"));
        assert!(result.contains("credential"));
        assert!(result.starts_with('('));
        assert!(result.contains(" OR "));
    }

    #[test]
    fn mixed_tokens_expand_selectively() {
        let result = expand_query_for_fts("auth middleware");
        // "auth" should expand, "middleware" should not
        assert!(result.contains("(auth OR authentication"));
        assert!(result.contains("middleware"));
        assert!(!result.contains("(middleware"));
    }

    #[test]
    fn all_synonyms_expand() {
        let result = expand_query_for_fts("config err");
        assert!(result.contains("(config OR configuration"));
        assert!(result.contains("(err OR error"));
    }

    #[test]
    fn case_insensitive_lookup() {
        let result = expand_query_for_fts("Auth");
        assert!(result.contains("Auth"));
        assert!(result.contains("authentication"));
    }

    #[test]
    fn synonym_map_has_expected_entries() {
        // Verify key synonyms from the spec exist
        let map = &*SYNONYMS;
        assert!(map.contains_key("auth"));
        assert!(map.contains_key("config"));
        assert!(map.contains_key("err"));
        assert!(map.contains_key("fn"));
        assert!(map.contains_key("init"));
        assert!(map.contains_key("parse"));
        assert!(map.contains_key("req"));
        assert!(map.contains_key("res"));
        assert!(map.contains_key("fmt"));
        assert!(map.contains_key("db"));
        assert!(map.len() >= 30, "Expected at least 30 synonym entries");
    }
}
