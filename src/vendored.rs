//! Vendored-content detection — see #1221 (SEC-V1.30.1-5).
//!
//! Chunks whose `origin` path passes through one of a small list of
//! conventional "this code came from outside the project" directories
//! (e.g. `vendor/`, `node_modules/`, `third_party/`) get tagged
//! `vendored = true` at index time. The downstream effect is that
//! search/scout/onboard JSON output emits
//! `trust_level: "vendored-code"` for those chunks instead of the
//! default `"user-code"`, giving consuming agents an explicit
//! in-protocol signal to apply stricter handling to third-party
//! content.
//!
//! The matching is path-segment-based, not literal-prefix: the path
//! `"/abs/.../vendor/oss-lib/foo.rs"` is vendored regardless of how
//! deep `vendor/` sits in the tree, because vendored content stays
//! vendored when it's symlinked or moved into a sub-tree. A bare
//! `myvendor/`-prefixed directory is *not* matched — only an exact
//! path-component equality with one of the configured prefix names.

/// Default vendored-path components matched at index time.
///
/// Intentionally conservative. Each entry is matched as an exact path
/// segment (no trailing slash, no substring match). The list mirrors
/// the issue's "Proposed direction" set: the convention-cluster of
/// directories where third-party code typically lives in real-world
/// repos, plus a handful of build-artifact directories whose contents
/// are derived from sources elsewhere and would be misleading if
/// labelled `user-code`.
///
/// Operators can override via `[index].vendored_paths` in `.cqs.toml`
/// (passing an explicit empty list disables vendored detection).
pub const DEFAULT_VENDORED_PREFIXES: &[&str] = &[
    "vendor",
    "third_party",
    "node_modules",
    ".cargo",
    "target",
    "dist",
    "build",
];

/// Returns true if any forward-slash-separated segment of `origin`
/// exactly matches one of `prefixes`. `prefixes` entries should be
/// bare directory names without slashes (the default list satisfies
/// that contract).
///
/// Origins reach this helper after `crate::normalize_path`, which
/// rewrites Windows backslashes to forward slashes — so the splitter
/// works uniformly across platforms. Empty `prefixes` short-circuits
/// to `false`.
pub fn is_vendored_origin(origin: &str, prefixes: &[String]) -> bool {
    if prefixes.is_empty() {
        return false;
    }
    origin
        .split('/')
        .any(|seg| !seg.is_empty() && prefixes.iter().any(|p| p == seg))
}

/// Resolve effective vendored prefixes for an index pipeline.
///
/// `override_list = Some(...)` always wins, even if the override is
/// empty (operators can disable vendored detection by passing an
/// explicit empty list in `.cqs.toml`). `override_list = None` falls
/// back to [`DEFAULT_VENDORED_PREFIXES`].
pub fn effective_prefixes(override_list: Option<&[String]>) -> Vec<String> {
    match override_list {
        Some(ov) => ov.to_vec(),
        None => DEFAULT_VENDORED_PREFIXES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default prefix list contains the convention-cluster directories
    /// from the #1221 issue body.
    #[test]
    fn default_prefixes_cover_common_vendored_dirs() {
        let p: &[&str] = DEFAULT_VENDORED_PREFIXES;
        for expected in [
            "vendor",
            "third_party",
            "node_modules",
            ".cargo",
            "target",
            "dist",
            "build",
        ] {
            assert!(
                p.contains(&expected),
                "default prefix list missing {expected}"
            );
        }
    }

    /// Path-segment match: any intermediate `vendor/` flags the
    /// origin, regardless of nesting depth or absolute/relative form.
    #[test]
    fn path_segment_match_flags_vendored_at_any_depth() {
        let prefixes = effective_prefixes(None);
        assert!(is_vendored_origin(
            "/abs/path/vendor/oss-lib/foo.rs",
            &prefixes
        ));
        assert!(is_vendored_origin("vendor/foo.rs", &prefixes));
        assert!(is_vendored_origin(
            "packages/app/node_modules/x/y.js",
            &prefixes
        ));
        assert!(is_vendored_origin("third_party/zlib/zutil.c", &prefixes));
    }

    /// Substring containment is not a match: only exact path-segment
    /// equality. `myvendor/` shares the literal prefix `vendor` but is
    /// not the same directory and must not be flagged.
    #[test]
    fn substring_contains_does_not_falsely_match() {
        let prefixes = effective_prefixes(None);
        assert!(!is_vendored_origin("myvendor/lib.rs", &prefixes));
        assert!(!is_vendored_origin("vendoring/util.rs", &prefixes));
        assert!(!is_vendored_origin("src/vendor.rs", &prefixes));
        assert!(!is_vendored_origin("nontarget/x.rs", &prefixes));
    }

    /// Empty prefix list short-circuits to `false` (operator opt-out).
    #[test]
    fn empty_prefix_list_disables_detection() {
        let prefixes: Vec<String> = vec![];
        assert!(!is_vendored_origin("vendor/foo.rs", &prefixes));
        assert!(!is_vendored_origin(
            "/abs/path/node_modules/x.js",
            &prefixes
        ));
    }

    /// `effective_prefixes(Some(empty))` honours the explicit empty
    /// override — distinct from `None` which falls through to defaults.
    #[test]
    fn explicit_empty_override_disables_detection() {
        let p = effective_prefixes(Some(&[]));
        assert!(p.is_empty());
        assert!(!is_vendored_origin("vendor/foo.rs", &p));
    }

    /// `effective_prefixes(Some(custom))` honours operator override.
    #[test]
    fn explicit_custom_override_replaces_default_list() {
        let custom = vec!["external".to_string(), "deps".to_string()];
        let p = effective_prefixes(Some(&custom));
        assert!(is_vendored_origin("external/lib.rs", &p));
        assert!(is_vendored_origin("deps/foo.rs", &p));
        // Default list members no longer match under custom override.
        assert!(!is_vendored_origin("vendor/lib.rs", &p));
        assert!(!is_vendored_origin("node_modules/x.js", &p));
    }
}
