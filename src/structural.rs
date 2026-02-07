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
