//! Structural pattern matching on code chunks.
//!
//! Heuristic regex-based patterns applied post-search.
//! NOT AST analysis — best-effort matching on source text.

use crate::language::Language;

// ---------------------------------------------------------------------------
// Macro: define_patterns!
//
// Generates from a single declaration table:
//   - `Pattern` enum with Debug, Clone, Copy, PartialEq, Eq
//   - `Display` impl (variant → name string)
//   - `FromStr` impl (name string → variant, with optional aliases)
//   - `Pattern::all_names()` — canonical names only
//
// Adding a pattern = one new line here. Display, FromStr, all_names() stay
// in sync automatically. Behavioral methods (`matches`, per-pattern fns)
// remain hand-written below.
// ---------------------------------------------------------------------------
/// Generates a `Pattern` enum with associated trait implementations for parsing and displaying structural patterns.
///
/// # Arguments
///
/// - `$variant`: Identifier for each enum variant
/// - `$name`: String literal for the primary name of the pattern
/// - `$alias`: Optional string literals for alternative names that map to the same variant
///
/// # Returns
///
/// Expands to:
/// - A `Pattern` enum with all specified variants
/// - `Display` impl that maps variants to their primary names
/// - `FromStr` impl that parses primary names and aliases (case-sensitive) into variants
/// - `all_names()` method returning a slice of all primary pattern names
///
/// # Errors
///
/// The `FromStr` implementation returns an error with a helpful message listing all valid pattern names when an unknown string is parsed.
macro_rules! define_patterns {
    ( $( $variant:ident => $name:expr $(, aliases = [ $($alias:expr),* ])? ; )* ) => {
        /// Known structural patterns
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Pattern {
            $( $variant, )*
        }

        impl std::fmt::Display for Pattern {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $( Pattern::$variant => write!(f, $name), )*
                }
            }
        }

        impl std::str::FromStr for Pattern {
            type Err = anyhow::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $name => Ok(Pattern::$variant), )*
                    $( $( $( $alias => Ok(Pattern::$variant), )* )? )*
                    _ => anyhow::bail!(
                        "Unknown pattern '{}'. Valid: {}",
                        s,
                        Self::all_names().join(", ")
                    ),
                }
            }
        }

        impl Pattern {
            /// All valid pattern names (for schema generation and validation)
            pub fn all_names() -> &'static [&'static str] {
                &[ $( $name, )* ]
            }
        }
    };
}

define_patterns! {
    Builder => "builder";
    ErrorSwallow => "error_swallow", aliases = ["error-swallow"];
    Async => "async";
    Mutex => "mutex";
    Unsafe => "unsafe";
    Recursion => "recursion";
}

impl Pattern {
    /// Check if a code chunk matches this pattern.
    ///
    /// If the language provides a specific structural matcher for this pattern
    /// (via `LanguageDef::structural_matchers`), uses that. Otherwise falls
    /// through to the generic heuristics.
    pub fn matches(&self, content: &str, name: &str, language: Option<Language>) -> bool {
        // Check for language-specific matcher first
        if let Some(lang) = language {
            if let Some(matchers) = lang.def().structural_matchers {
                let pattern_name = self.to_string();
                for (matcher_name, matcher_fn) in matchers {
                    if *matcher_name == pattern_name {
                        return matcher_fn(content, name);
                    }
                }
            }
        }

        // Fall through to generic heuristics
        match self {
            Self::Builder => matches_builder(content, name),
            Self::ErrorSwallow => matches_error_swallow(content, language),
            Self::Async => matches_async(content, language),
            Self::Mutex => matches_mutex(content, language),
            Self::Unsafe => matches_unsafe(content, language),
            Self::Recursion => matches_recursion(content, name),
        }
    }
}

/// Builder pattern: returns self/Self, method chaining
fn matches_builder(content: &str, _name: &str) -> bool {
    // Look for returning self/Self or &self/&mut self
    content.contains("-> Self")
        || content.contains("-> &Self")
        || content.contains("-> &mut Self")
        || content.contains("return self")
        || content.contains("return this")
        || (content.contains(".set") && content.contains("return"))
}

// Generic cross-language fallback marker slices. Used when `language == None`
// or when the language's per-pattern slice is empty. Substring `any()` scan.
//
// Adding a new language with bespoke markers does not need to touch these
// slices — set the per-language fields on `LanguageDef` instead.

/// Generic error-swallow markers for cross-language fallback.
const GENERIC_ERROR_SWALLOW: &[&str] =
    &["catch (e) {}", "catch {}", "except:", "except Exception:"];

/// Generic async markers for cross-language fallback.
const GENERIC_ASYNC_MARKERS: &[&str] = &["async", "await"];

/// Generic mutex markers for cross-language fallback.
const GENERIC_MUTEX_MARKERS: &[&str] = &["mutex", "Mutex", "lock()", "Lock()"];

/// Generic unsafe markers for cross-language fallback.
const GENERIC_UNSAFE_MARKERS: &[&str] = &["unsafe"];

/// Resolve a per-language marker slice with fallback semantics:
///   - If `language` is `Some(L)` AND `L`'s `markers` are non-empty → use them.
///   - Otherwise use the supplied `generic` slice.
///
/// Each slice is treated disjunctively: any single substring hit triggers the
/// pattern. Conjunctive markers (e.g. Python "except: AND pass") were folded
/// into single specific phrases ("except:") that distinguish positive from
/// negative cases without the AND.
fn matches_any_marker(
    content: &str,
    language: Option<Language>,
    select: fn(&'static crate::language::LanguageDef) -> &'static [&'static str],
    generic: &'static [&'static str],
) -> bool {
    let markers = match language {
        Some(lang) => {
            let per_lang = select(lang.def());
            if per_lang.is_empty() {
                generic
            } else {
                per_lang
            }
        }
        None => generic,
    };
    markers.iter().any(|m| content.contains(m))
}

/// Error swallowing: catch/except with empty body, unwrap_or_default, `_ => {}`, etc.
///
/// Per-language markers live on `LanguageDef::error_swallow_patterns`. None of
/// the language-specific slices include the conjunctive logic that the old
/// dispatcher had — they were rewritten as specific disjunctive phrases that
/// pass the same test cases (e.g. Python `["except:", "except Exception:"]`
/// distinguishes bare-except from typed-except).
fn matches_error_swallow(content: &str, language: Option<Language>) -> bool {
    matches_any_marker(
        content,
        language,
        |def| def.error_swallow_patterns,
        GENERIC_ERROR_SWALLOW,
    )
}

/// Whether the chunk contains language-specific async / concurrency markers.
/// Per-language markers live on `LanguageDef::async_markers`.
fn matches_async(content: &str, language: Option<Language>) -> bool {
    matches_any_marker(
        content,
        language,
        |def| def.async_markers,
        GENERIC_ASYNC_MARKERS,
    )
}

/// Whether the chunk contains mutex/lock markers.
/// Per-language markers live on `LanguageDef::mutex_markers`.
fn matches_mutex(content: &str, language: Option<Language>) -> bool {
    matches_any_marker(
        content,
        language,
        |def| def.mutex_markers,
        GENERIC_MUTEX_MARKERS,
    )
}

/// Whether the chunk contains unsafe-code markers.
/// Per-language markers live on `LanguageDef::unsafe_markers`.
fn matches_unsafe(content: &str, language: Option<Language>) -> bool {
    matches_any_marker(
        content,
        language,
        |def| def.unsafe_markers,
        GENERIC_UNSAFE_MARKERS,
    )
}

/// Recursion: function calls itself by name
fn matches_recursion(content: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Look for the function name appearing in its own body (excluding the definition line)
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 1 {
        return false;
    }
    // Skip first line (function signature) and check for self-reference
    let call_paren = format!("{}(", name);
    let call_space = format!("{} (", name);
    lines[1..]
        .iter()
        .any(|line| line.contains(&call_paren) || line.contains(&call_space))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_parse_all_variants() {
        assert!(matches!(
            "builder".parse::<Pattern>().unwrap(),
            Pattern::Builder
        ));
        assert!(matches!(
            "error_swallow".parse::<Pattern>().unwrap(),
            Pattern::ErrorSwallow
        ));
        assert!(matches!(
            "error-swallow".parse::<Pattern>().unwrap(),
            Pattern::ErrorSwallow
        ));
        assert!(matches!(
            "async".parse::<Pattern>().unwrap(),
            Pattern::Async
        ));
        assert!(matches!(
            "mutex".parse::<Pattern>().unwrap(),
            Pattern::Mutex
        ));
        assert!(matches!(
            "unsafe".parse::<Pattern>().unwrap(),
            Pattern::Unsafe
        ));
        assert!(matches!(
            "recursion".parse::<Pattern>().unwrap(),
            Pattern::Recursion
        ));
        assert!("unknown".parse::<Pattern>().is_err());
    }

    #[test]
    fn test_pattern_display_roundtrip() {
        for name in Pattern::all_names() {
            let p: Pattern = name.parse().unwrap();
            assert_eq!(p.to_string(), *name);
        }
    }

    #[test]
    fn test_all_names_covers_all_variants() {
        // Ensure all_names has the same count as the roundtrip test variants
        // If a new variant is added to Pattern but not to all_names(), this fails
        assert_eq!(Pattern::all_names().len(), 6);
        for name in Pattern::all_names() {
            assert!(
                name.parse::<Pattern>().is_ok(),
                "all_names entry '{}' failed to parse",
                name
            );
        }
    }

    #[test]
    fn test_builder_pattern() {
        let pat = Pattern::Builder;
        assert!(pat.matches(
            "fn with_name(self, name: &str) -> Self { ... }",
            "with_name",
            None
        ));
        assert!(pat.matches("fn build(self) -> &Self { ... }", "build", None));
        assert!(!pat.matches("fn foo() -> i32 { 42 }", "foo", None));
    }

    #[test]
    fn test_error_swallow_rust() {
        let pat = Pattern::ErrorSwallow;
        let lang = Some(Language::Rust);
        assert!(pat.matches("let _ = result.unwrap_or_default();", "", lang));
        assert!(pat.matches("result.ok();", "", lang));
        assert!(pat.matches("match x { Ok(v) => v, _ => {} }", "", lang));
        assert!(!pat.matches("let v = result?;", "", lang));
    }

    #[test]
    fn test_error_swallow_python() {
        let pat = Pattern::ErrorSwallow;
        let lang = Some(Language::Python);
        assert!(pat.matches("try:\n    foo()\nexcept:\n    pass", "", lang));
        assert!(pat.matches("try:\n    foo()\nexcept Exception:\n    pass", "", lang));
        assert!(!pat.matches(
            "try:\n    foo()\nexcept ValueError as e:\n    log(e)",
            "",
            lang
        ));
    }

    #[test]
    fn test_error_swallow_js() {
        let pat = Pattern::ErrorSwallow;
        let lang = Some(Language::JavaScript);
        assert!(pat.matches("try { foo(); } catch (e) {}", "", lang));
        assert!(pat.matches("try { foo(); } catch (e) { // ignore }", "", lang));
        assert!(!pat.matches("try { foo(); } catch (e) { console.log(e); }", "", lang));
    }

    #[test]
    fn test_async_rust() {
        let pat = Pattern::Async;
        assert!(pat.matches("async fn fetch() { ... }", "", Some(Language::Rust)));
        assert!(pat.matches("let r = client.get(url).await?;", "", Some(Language::Rust)));
        assert!(!pat.matches("fn sync_fetch() { ... }", "", Some(Language::Rust)));
    }

    #[test]
    fn test_async_python() {
        let pat = Pattern::Async;
        assert!(pat.matches("async def fetch():", "", Some(Language::Python)));
        assert!(pat.matches("result = await client.get(url)", "", Some(Language::Python)));
        assert!(!pat.matches("def sync_fetch():", "", Some(Language::Python)));
    }

    #[test]
    fn test_async_go() {
        let pat = Pattern::Async;
        let lang = Some(Language::Go);
        assert!(pat.matches("go func() { ... }()", "", lang));
        assert!(pat.matches("ch <- value", "", lang));
        assert!(!pat.matches("func sync() { ... }", "", lang));
    }

    #[test]
    fn test_mutex_rust() {
        let pat = Pattern::Mutex;
        let lang = Some(Language::Rust);
        assert!(pat.matches("let guard = data.lock().unwrap();", "", lang));
        assert!(pat.matches("let m = Mutex::new(0);", "", lang));
        assert!(pat.matches("let rw = RwLock::new(vec![]);", "", lang));
        assert!(!pat.matches("fn pure_function(x: i32) -> i32 { x + 1 }", "", lang));
    }

    #[test]
    fn test_unsafe_rust() {
        let pat = Pattern::Unsafe;
        assert!(pat.matches("unsafe { ptr::read(src) }", "", Some(Language::Rust)));
        assert!(!pat.matches("fn safe_function() { ... }", "", Some(Language::Rust)));
    }

    #[test]
    fn test_unsafe_c() {
        let pat = Pattern::Unsafe;
        let lang = Some(Language::C);
        assert!(pat.matches("memcpy(dst, src, n);", "", lang));
        assert!(pat.matches("strcpy(buf, input);", "", lang));
        assert!(pat.matches("sprintf(buf, fmt, arg);", "", lang));
        assert!(!pat.matches("int add(int a, int b) { return a + b; }", "", lang));
    }

    #[test]
    fn test_recursion_self_call() {
        let pat = Pattern::Recursion;
        let code =
            "fn factorial(n: u32) -> u32 {\n    if n <= 1 { 1 } else { n * factorial(n - 1) }\n}";
        assert!(pat.matches(code, "factorial", None));
    }

    #[test]
    fn test_recursion_no_self_call() {
        let pat = Pattern::Recursion;
        let code = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}";
        assert!(!pat.matches(code, "add", None));
    }

    #[test]
    fn test_recursion_empty_name() {
        let pat = Pattern::Recursion;
        assert!(!pat.matches("fn foo() { foo() }", "", None));
    }

    #[test]
    fn test_recursion_single_line() {
        let pat = Pattern::Recursion;
        // Single-line content should not match (can't distinguish sig from body)
        assert!(!pat.matches("fn foo() { foo() }", "foo", None));
    }

    #[test]
    fn test_structural_matchers_fallback() {
        // When no language-specific matcher exists, generic heuristics are used
        let pat = Pattern::Unsafe;
        // Rust has no structural_matchers set (None), so it falls through
        assert!(pat.matches("unsafe { ptr::read(p) }", "read_ptr", Some(Language::Rust)));
        assert!(!pat.matches("fn safe() -> i32 { 42 }", "safe", Some(Language::Rust)));
    }

    #[test]
    fn test_pattern_matches_no_language() {
        // None language should use generic heuristics
        let pat = Pattern::Async;
        assert!(pat.matches("async function fetch() {}", "fetch", None));
        assert!(!pat.matches("function sync() {}", "sync", None));
    }
}
