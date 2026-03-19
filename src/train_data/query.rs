use regex::Regex;
use std::sync::OnceLock;

/// Strip conventional commit prefix: `type(scope)!: ...` → `...`
fn conventional_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^[a-z]+(\([^)]*\))?!?\s*:\s*").unwrap())
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
    RE.get_or_init(|| Regex::new(r"\s*\(?#\d+\)?\s*$").unwrap())
}

/// Normalize a commit message into a search query.
///
/// Applies three transformations in order:
/// 1. Strip conventional commit prefix (`fix(parser): ...` → `...`)
/// 2. Strip leading verb (`add retry logic` → `retry logic`)
/// 3. Strip trailing PR/issue references (`... (#234)` → `...`)
///
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

    #[test]
    fn strip_conventional_commit_prefix() {
        assert_eq!(
            normalize_query("fix(parser): timeout handling"),
            "timeout handling"
        );
        assert_eq!(normalize_query("feat!: breaking change"), "breaking change");
        assert_eq!(normalize_query("FIX: case insensitive"), "case insensitive");
    }

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

    #[test]
    fn strip_trailing_pr_reference() {
        assert_eq!(normalize_query("fix timeout (#234)"), "timeout");
        assert_eq!(normalize_query("update config #123"), "config");
    }

    #[test]
    fn combined_stripping() {
        assert_eq!(
            normalize_query("fix(parser): add timeout handling (#456)"),
            "timeout handling"
        );
    }

    #[test]
    fn empty_after_strip_returns_original() {
        assert_eq!(normalize_query("fix:"), "fix:");
        assert_eq!(normalize_query("wip"), "wip");
    }

    #[test]
    fn no_stripping_needed() {
        assert_eq!(
            normalize_query("config parser timeout"),
            "config parser timeout"
        );
    }
}
