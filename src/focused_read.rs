//! Focused-read shared logic
//!
//! Extracts type names from function signatures for dependency resolution
//! in focused-read mode. Used by the CLI read command.

use std::collections::HashSet;
use std::sync::LazyLock;

static TYPE_NAME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\b([A-Z][a-zA-Z0-9_]+)\b").expect("hardcoded regex"));

static COMMON_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "String",
        "Vec",
        "Result",
        "Option",
        "Box",
        "Arc",
        "Rc",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "Path",
        "PathBuf",
        "Value",
        "Error",
        "Self",
        "None",
        "Some",
        "Ok",
        "Err",
        "Mutex",
        "RwLock",
        "Cow",
        "Pin",
        "Future",
        "Iterator",
        "Display",
        "Debug",
        "Clone",
        "Default",
        "Send",
        "Sync",
        "Sized",
        "Copy",
        "From",
        "Into",
        "AsRef",
        "AsMut",
        "Deref",
        "DerefMut",
        "Read",
        "Write",
        "Seek",
        "BufRead",
        "ToString",
        "Serialize",
        "Deserialize",
    ]
    .into_iter()
    .collect()
});

/// Extract non-standard type names from a function signature.
///
/// Finds capitalized identifiers (e.g., `ChunkSummary`, `SearchResult`)
/// and filters out common standard library types. Returns sorted, deduplicated names.
pub fn extract_type_names(signature: &str) -> Vec<String> {
    let mut names: Vec<String> = TYPE_NAME_RE
        .find_iter(signature)
        .map(|m| m.as_str().to_string())
        .filter(|name| !COMMON_TYPES.contains(name.as_str()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_type_names_filters_common() {
        let sig = "fn foo(s: String, v: Vec<ChunkSummary>) -> Result<SearchResult>";
        let names = extract_type_names(sig);
        assert!(names.contains(&"ChunkSummary".to_string()));
        assert!(names.contains(&"SearchResult".to_string()));
        assert!(!names.contains(&"String".to_string()));
        assert!(!names.contains(&"Vec".to_string()));
        assert!(!names.contains(&"Result".to_string()));
    }

    #[test]
    fn test_extract_type_names_empty() {
        let names = extract_type_names("fn bar(x: i32) -> bool");
        assert!(names.is_empty());
    }

    #[test]
    fn test_extract_type_names_deduplicates() {
        let sig = "fn baz(a: Foo, b: Foo) -> Foo";
        let names = extract_type_names(sig);
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "Foo");
    }
}
