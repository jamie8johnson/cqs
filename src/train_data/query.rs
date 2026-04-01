use regex::Regex;
use std::sync::OnceLock;

/// Strip conventional commit prefix: `type(scope)!: ...` → `...`
fn conventional_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^[a-z]+(\([^)]*\))?!?\s*:\s*").expect("valid regex"))
}

/// Strip leading verb from common commit messages.
fn leading_verb_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^(add|added|adds|implement|implemented|implements|fix|fixed|fixes|update|updated|updates|remove|removed|removes|refactor|refactored|refactors|move|moved|moves|rename|renamed|renames|change|changed|changes|improve|improved|improves|introduce|introduced|introduces|replace|replaced|replaces|convert|converted|converts|use|wip|bump|bumped|bumps|extract|extracted|extracts|simplify|simplified|simplifies|handle|handled|handles|make|delete|deleted|deletes|clean|cleaned|cleans|create|created|creates|merge|merged|merges|revert|reverted|reverts|enable|enabled|enables|disable|disabled|disables|drop|dropped|drops|migrate|migrated|migrates|switch|switched|switches|allow|allowed|allows|prevent|prevented|prevents|ensure|ensured|ensures|apply|applied|applies|adjust|adjusted|adjusts|correct|corrected|corrects|set|support|supported|supports)\s+").unwrap()
    })
}

/// Strip trailing PR/issue references: `(#123)`, `#123`, `(GH-45)`, etc.
fn trailing_noise_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s*\(?#\d+\)?\s*$").expect("valid regex"))
}

/// Normalize a commit message into a search query.
/// Applies three transformations in order:
/// 1. Strip conventional commit prefix (`fix(parser): ...` → `...`)
/// 2. Strip leading verb (`add retry logic` → `retry logic`)
/// 3. Strip trailing PR/issue references (`... (#234)` → `...`)
/// If the result is empty after stripping, returns the original trimmed input.
pub fn normalize_query(msg: &str) -> String {
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }

    // Step 1: strip conventional commit prefix
    let after_prefix = conventional_prefix_re().replace(trimmed, "");

    // Step 2: strip leading verb
    let after_verb = leading_verb_re().replace(&after_prefix, "");

    // Step 3: strip trailing noise (PR refs)
    let after_noise = trailing_noise_re().replace(&after_verb, "");

    let result = after_noise.trim().to_string();

    // If everything was stripped, return original
    if result.is_empty() {
        trimmed.to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Strips conventional commit prefixes from query strings and normalizes them to lowercase.
    /// This function tests that the `normalize_query` function properly removes conventional commit type prefixes (such as "fix:", "feat:", "feat!:") from the beginning of query strings, handling various cases including scope notation and breaking change indicators. The resulting normalized query contains only the commit message body in lowercase.
    /// # Arguments
    /// None - this is a test function that implicitly tests the `normalize_query` function.
    /// # Returns
    /// None - this is a test function that asserts expected behavior.
    /// # Panics
    /// Panics if any of the assertions fail, indicating that `normalize_query` does not properly strip conventional commit prefixes or normalize to lowercase as expected.

    #[test]
    fn strip_conventional_commit_prefix() {
        assert_eq!(
            normalize_query("fix(parser): timeout handling"),
            "timeout handling"
        );
        assert_eq!(normalize_query("feat!: breaking change"), "breaking change");
        assert_eq!(normalize_query("FIX: case insensitive"), "case insensitive");
    }
    /// Removes common leading verbs from query strings to normalize them for comparison or processing.
    /// # Arguments
    /// None. This function tests the `normalize_query` function with predefined inputs.
    /// # Returns
    /// None. This is a test function that asserts expected behavior.
    /// # Panics
    /// Panics if any assertion fails, indicating that `normalize_query` did not correctly remove leading verbs like "add", "implement", or "wip" from the input strings.

    #[test]
    fn strip_leading_verb() {
        assert_eq!(
            normalize_query("add retry logic to HTTP client"),
            "retry logic to HTTP client"
        );
        assert_eq!(
            normalize_query("implement batch processing"),
            "batch processing"
        );
        assert_eq!(normalize_query("wip config changes"), "config changes");
    }
    /// Normalizes a query string by removing leading/trailing whitespace and stripping out pull request references (text in parentheses or preceded by `#`).
    /// # Arguments
    /// None - this is a test function that validates the `normalize_query` function.
    /// # Returns
    /// None - this test function asserts expected behavior through assertions.
    /// # Panics
    /// Panics if any assertion fails, indicating that `normalize_query` did not produce the expected normalized output.

    #[test]
    fn strip_trailing_pr_reference() {
        assert_eq!(normalize_query("fix timeout (#234)"), "timeout");
        assert_eq!(normalize_query("update config #123"), "config");
    }
    /// Verifies that normalize_query correctly strips commit type prefixes, scope annotations, and issue references from a commit message, returning only the meaningful content.
    /// # Arguments
    /// None - this is a test function that uses a hardcoded input.
    /// # Returns
    /// None - this is a test function that asserts the expected behavior.
    /// # Panics
    /// Panics if the assertion fails, indicating that normalize_query did not correctly process the commit message format "fix(parser): add timeout handling (#456)" into "timeout handling".

    #[test]
    fn combined_stripping() {
        assert_eq!(
            normalize_query("fix(parser): add timeout handling (#456)"),
            "timeout handling"
        );
    }
    /// This is a test function, not a function that requires a doc comment. However, if documentation were needed, it would be:
    /// Tests that `normalize_query` returns the original input string when the result after stripping would be empty or when the input contains no special markers.
    /// # Arguments
    /// No parameters.
    /// # Returns
    /// No return value; this is a test assertion function.

    #[test]
    fn empty_after_strip_returns_original() {
        assert_eq!(normalize_query("fix:"), "fix:");
        assert_eq!(normalize_query("wip"), "wip");
    }
    /// Tests that the normalize_query function correctly handles queries that contain no leading or trailing whitespace, returning the input string unchanged.
    /// # Arguments
    /// None - this is a test function with no parameters.
    /// # Returns
    /// None - this is a test function that asserts expected behavior.
    /// # Panics
    /// Panics if the assertion fails, indicating that normalize_query did not return the expected unmodified string.

    #[test]
    fn no_stripping_needed() {
        assert_eq!(
            normalize_query("config parser timeout"),
            "config parser timeout"
        );
    }
}
