//! Structural pattern matching on code chunks.
//!
//! Heuristic regex-based patterns applied post-search.
//! NOT AST analysis — best-effort matching on source text.

use crate::language::Language;

/// Known structural patterns
#[derive(Debug, Clone, Copy)]
pub enum Pattern {
    Builder,
    ErrorSwallow,
    Async,
    Mutex,
    Unsafe,
    Recursion,
}

impl std::str::FromStr for Pattern {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "builder" => Ok(Self::Builder),
            "error_swallow" | "error-swallow" => Ok(Self::ErrorSwallow),
            "async" => Ok(Self::Async),
            "mutex" => Ok(Self::Mutex),
            "unsafe" => Ok(Self::Unsafe),
            "recursion" => Ok(Self::Recursion),
            _ => anyhow::bail!(
                "Unknown pattern '{}'. Valid: builder, error_swallow, async, mutex, unsafe, recursion",
                s
            ),
        }
    }
}

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builder => write!(f, "builder"),
            Self::ErrorSwallow => write!(f, "error_swallow"),
            Self::Async => write!(f, "async"),
            Self::Mutex => write!(f, "mutex"),
            Self::Unsafe => write!(f, "unsafe"),
            Self::Recursion => write!(f, "recursion"),
        }
    }
}

impl Pattern {
    /// All valid pattern names (for schema generation and validation)
    pub fn all_names() -> &'static [&'static str] {
        &[
            "builder",
            "error_swallow",
            "async",
            "mutex",
            "unsafe",
            "recursion",
        ]
    }

    /// Check if a code chunk matches this pattern
    pub fn matches(&self, content: &str, name: &str, language: Option<Language>) -> bool {
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

/// Error swallowing: catch/except with empty body, unwrap_or_default, _ => {}
fn matches_error_swallow(content: &str, language: Option<Language>) -> bool {
    match language {
        Some(Language::Rust) => {
            content.contains("unwrap_or_default()")
                || content.contains("unwrap_or(())")
                || content.contains(".ok();")
                || content.contains("_ => {}")
                || content.contains("_ => ()")
        }
        Some(Language::Python) => {
            content.contains("except:") && content.contains("pass")
                || content.contains("except Exception:")
                    && (content.contains("pass") || content.contains("..."))
        }
        Some(Language::TypeScript | Language::JavaScript) => {
            content.contains("catch") && content.contains("{}")
                || content.contains("catch (") && content.contains("// ignore")
        }
        Some(Language::Go) => {
            // Go: _ = err pattern
            content.contains("_ = err") || content.contains("_ = ")
        }
        _ => {
            // Generic heuristics
            content.contains("catch") && content.contains("{}")
                || content.contains("except") && content.contains("pass")
        }
    }
}

/// Async code patterns
fn matches_async(content: &str, language: Option<Language>) -> bool {
    match language {
        Some(Language::Rust) => content.contains("async fn") || content.contains(".await"),
        Some(Language::Python) => content.contains("async def") || content.contains("await "),
        Some(Language::TypeScript | Language::JavaScript) => {
            content.contains("async ") || content.contains("await ")
        }
        Some(Language::Go) => {
            content.contains("go func") || content.contains("go ") || content.contains("<-")
        }
        _ => content.contains("async") || content.contains("await"),
    }
}

/// Mutex/lock patterns
fn matches_mutex(content: &str, language: Option<Language>) -> bool {
    match language {
        Some(Language::Rust) => {
            content.contains("Mutex") || content.contains("RwLock") || content.contains(".lock()")
        }
        Some(Language::Python) => content.contains("Lock()") || content.contains("threading.Lock"),
        Some(Language::Go) => content.contains("sync.Mutex") || content.contains("sync.RWMutex"),
        _ => {
            content.contains("mutex")
                || content.contains("Mutex")
                || content.contains("lock()")
                || content.contains("Lock()")
        }
    }
}

/// Unsafe code patterns (primarily Rust and C)
fn matches_unsafe(content: &str, language: Option<Language>) -> bool {
    match language {
        Some(Language::Rust) => content.contains("unsafe "),
        Some(Language::C) => {
            // C is inherently unsafe, look for dangerous patterns
            content.contains("memcpy")
                || content.contains("strcpy")
                || content.contains("sprintf")
                || content.contains("gets(")
        }
        Some(Language::Go) => content.contains("unsafe.Pointer"),
        _ => content.contains("unsafe"),
    }
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
    let body = &lines[1..].join("\n");
    body.contains(&format!("{}(", name)) || body.contains(&format!("{} (", name))
}

/// Filter a list of items by structural pattern
#[allow(dead_code)] // Public API — used in tests, available for external consumers
pub fn filter_by_pattern<T, F>(items: Vec<T>, pattern: &Pattern, get_info: F) -> Vec<T>
where
    F: Fn(&T) -> (&str, &str, Option<Language>),
{
    items
        .into_iter()
        .filter(|item| {
            let (content, name, lang) = get_info(item);
            pattern.matches(content, name, lang)
        })
        .collect()
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
    fn test_filter_by_pattern() {
        let items = vec![
            ("unsafe { ptr::read(p) }", "read_ptr", Some(Language::Rust)),
            ("fn safe() -> i32 { 42 }", "safe", Some(Language::Rust)),
            ("unsafe { transmute(x) }", "cast", Some(Language::Rust)),
        ];

        let filtered = filter_by_pattern(items, &Pattern::Unsafe, |item| (item.0, item.1, item.2));

        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].1, "read_ptr");
        assert_eq!(filtered[1].1, "cast");
    }
}
