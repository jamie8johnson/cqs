# NL Field Extraction for All Languages (EX-25 / #680)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move field/method name extraction from hardcoded 6-language match to LanguageDef configuration, covering all 51 languages with correct extraction for both name-first (`name: Type`) and type-first (`Type name;`) languages.

**Architecture:** Add a `FieldStyle` enum to `LanguageDef` that distinguishes name-first from type-first field syntax. Field extraction strips language-specific prefixes (visibility + value keywords), then splits on the appropriate separator and takes the correct positional token. Method extraction uses the language's function keyword (already present in structural matchers).

**Tech Stack:** Rust, LanguageDef macro system

---

### Task 1: Baseline Regression Tests

**Files:**
- Modify: `src/nl.rs` (add tests to existing `#[cfg(test)]` module)

Write tests for the 6 currently-working languages BEFORE any refactoring. These are the regression safety net.

- [ ] **Step 1: Write `extract_field_names` tests for all 6 languages**

```rust
#[test]
fn extract_field_names_rust() {
    let content = "pub struct Config {\n    pub name: String,\n    pub(crate) max_size: usize,\n    enabled: bool,\n}";
    let fields = extract_field_names(content, Language::Rust);
    assert_eq!(fields, vec!["name", "max size", "enabled"]);
}

#[test]
fn extract_field_names_go() {
    let content = "type Config struct {\n    Name string\n    MaxSize int\n    Enabled bool\n}";
    let fields = extract_field_names(content, Language::Go);
    assert_eq!(fields, vec!["Name", "Max Size", "Enabled"]);
}

#[test]
fn extract_field_names_python() {
    let content = "class Config:\n    name: str\n    max_size: int = 100\n    enabled = True";
    let fields = extract_field_names(content, Language::Python);
    assert_eq!(fields, vec!["name", "max size", "enabled"]);
}

#[test]
fn extract_field_names_typescript() {
    let content = "class Config {\n    public name: string;\n    private maxSize: number;\n    readonly enabled: boolean;\n}";
    let fields = extract_field_names(content, Language::TypeScript);
    assert_eq!(fields, vec!["name", "max Size", "enabled"]);
}

#[test]
fn extract_field_names_javascript() {
    let content = "class Config {\n    name = 'default';\n    maxSize = 100;\n}";
    let fields = extract_field_names(content, Language::JavaScript);
    assert_eq!(fields, vec!["name", "max Size"]);
}

#[test]
fn extract_field_names_java() {
    let content = "class Config {\n    private String name;\n    protected int maxSize;\n    public boolean enabled;\n}";
    let fields = extract_field_names(content, Language::Java);
    assert_eq!(fields, vec!["name", "max Size", "enabled"]);
}
```

**IMPORTANT:** Run these BEFORE continuing. If any fail, the test expectation is wrong — fix the assertion to match actual current behavior. The goal is to capture the current output, not define ideal output.

- [ ] **Step 2: Write adversarial tests**

```rust
#[test]
fn extract_field_names_empty_content() {
    assert!(extract_field_names("", Language::Rust).is_empty());
}

#[test]
fn extract_field_names_only_comments() {
    let content = "// comment\n/* block */\n/// doc";
    assert!(extract_field_names(content, Language::Rust).is_empty());
}

#[test]
fn extract_field_names_no_fields_only_header() {
    let content = "pub struct Empty {\n}";
    assert!(extract_field_names(content, Language::Rust).is_empty());
}

#[test]
fn extract_field_names_unicode_identifiers() {
    let content = "pub struct S {\n    pub café: String,\n}";
    let fields = extract_field_names(content, Language::Rust);
    assert!(!fields.is_empty()); // Just verify no panic
}

#[test]
fn extract_field_names_caps_at_15() {
    let lines: Vec<String> = (0..20).map(|i| format!("    field_{i}: i32,")).collect();
    let content = format!("pub struct Big {{\n{}\n}}", lines.join("\n"));
    assert!(extract_field_names(&content, Language::Rust).len() <= 15);
}

#[test]
fn extract_field_names_unsupported_language_returns_empty() {
    let content = "field: value";
    assert!(extract_field_names(content, Language::Bash).is_empty());
}
```

- [ ] **Step 3: Run tests, all must pass**

```bash
cargo test --features gpu-index -- extract_field_names -v
```

- [ ] **Step 4: Commit baseline tests**

```bash
git add src/nl.rs
git commit -m "test: baseline regression tests for extract_field_names (6 languages + adversarial)"
```

### Task 2: Add FieldStyle to LanguageDef

**Files:**
- Modify: `src/language/mod.rs` (add enum + field)
- Modify: 51 language files in `src/language/`

- [ ] **Step 1: Define FieldStyle enum**

Add to `src/language/mod.rs` before `LanguageDef`:

```rust
/// How to extract field names from struct/class/record bodies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldStyle {
    /// No field extraction (markup, config, shell languages).
    None,
    /// Name appears before separator: `name: Type`, `name = value`.
    /// Contains separator chars and prefix keywords to strip.
    /// Example: Rust `name: Type`, Python `name = value`, Kotlin `val name: Type`.
    NameFirst {
        /// Characters to split on (e.g., ":=")
        separators: &'static str,
        /// Space-separated prefixes to strip before extraction.
        /// Includes both visibility (`pub`, `private`) and value keywords (`val`, `var`, `let`).
        /// e.g., "pub pub(crate) private protected public readonly val var let lazy"
        strip_prefixes: &'static str,
    },
    /// Type appears before name: `Type name;` (C, C++, Java, C#).
    /// Takes last whitespace-delimited token before `;`, `=`, or `,`.
    /// strip_prefixes works the same as NameFirst.
    TypeFirst {
        /// Space-separated prefixes to strip.
        strip_prefixes: &'static str,
    },
}
```

- [ ] **Step 2: Add field to LanguageDef**

```rust
/// Field extraction style for struct/class/record body parsing.
/// Used by `extract_field_names` in `src/nl.rs`.
pub field_style: FieldStyle,
```

- [ ] **Step 3: Populate for all 51 languages**

Classification (verified against actual syntax):

**NameFirst (colon separator):**
- Rust: `separators: ":", strip_prefixes: "pub pub(crate) pub(super)"`
- TypeScript/JavaScript: `separators: ":=;", strip_prefixes: "public private protected readonly static"`
- Python: `separators: ":=", strip_prefixes: ""`
- Kotlin: `separators: ":", strip_prefixes: "val var private protected public internal override lateinit"`
- Swift: `separators: ":", strip_prefixes: "let var private public internal fileprivate open static weak lazy"`
- Scala: `separators: ":", strip_prefixes: "val var private protected override lazy"`
- F#: `separators: ":", strip_prefixes: "mutable"`
- Elixir: `separators: ":", strip_prefixes: ""` (keyword lists: `name: value`)
- Zig: `separators: ":", strip_prefixes: "pub"`
- Gleam: `separators: ":", strip_prefixes: "pub"`
- Protobuf: `separators: " ", strip_prefixes: "optional repeated required"` (special: `type name = N`)
- GraphQL: `separators: ":", strip_prefixes: ""`

**NameFirst (double-colon):**
- Haskell: `separators: ":", strip_prefixes: ""` (splitting on single `:` still works — gets `fieldName ` before `:: Type`)
- Julia: `separators: ":", strip_prefixes: ""` (same — `name::Type` splits on first `:`)

**NameFirst (assignment only):**
- Ruby: `separators: "=", strip_prefixes: "attr_accessor attr_reader attr_writer"`
- Lua: `separators: "=", strip_prefixes: "local"`
- R: `separators: "=<", strip_prefixes: ""` (R uses `name <- value` or `name = value`)
- Perl: `separators: "=", strip_prefixes: "my our local"`

**TypeFirst:**
- Go: `strip_prefixes: ""` (special case: currently grouped with Rust but Go is actually name-first `Name Type`)
- C: `strip_prefixes: "static const volatile extern unsigned signed"`
- C++: `strip_prefixes: "static const volatile mutable virtual inline"`
- Java: `strip_prefixes: "private protected public static final volatile transient"`
- C#: `strip_prefixes: "private protected public internal static readonly virtual override abstract sealed new"`
- CUDA: same as C++
- GLSL: same as C
- Solidity: `strip_prefixes: "public private internal constant immutable"`
- Objective-C: `strip_prefixes: ""` (Note: `@property` syntax is too complex — use None instead)
- VB.NET: `strip_prefixes: "Dim Public Private Protected Friend Shared ReadOnly"` (Note: `As` keyword before type means VB.NET is actually NameFirst with separator `As ` — but this is a multi-char separator. Use TypeFirst as approximation: strip prefixes, last token before `As` is the name)

**ACTUALLY — Go correction:** Go struct fields are `Name Type` (name first, space separator). The current code handles this correctly by grouping Go with Rust and splitting on `[':', ' ']`. Go should be:
- Go: `NameFirst { separators: " ", strip_prefixes: "" }`

**None (no fields):**
- Bash, SQL, HTML, CSS, JSON, YAML, TOML, INI, XML, Markdown, Nix, Make, LaTeX, HCL, PowerShell, Erlang, OCaml, Svelte, Razor, Vue, ASPX

**Objective-C correction:** `@property` parsing is too complex for a simple separator model. Use `None` and leave for a future `Custom` variant if needed.

**VB.NET correction:** `Dim name As Type` is also too complex. Use `None`.

- [ ] **Step 4: Run tests — all baseline tests must still pass**

- [ ] **Step 5: Commit**

```bash
git add src/language/
git commit -m "feat: add FieldStyle enum to LanguageDef for 51 languages"
```

### Task 3: Refactor extract_field_names

**Files:**
- Modify: `src/nl.rs`

- [ ] **Step 1: Add tracing span**

```rust
fn extract_field_names(content: &str, language: Language) -> Vec<String> {
    let _span = tracing::debug_span!("extract_field_names", %language).entered();
```

- [ ] **Step 2: Replace match with LanguageDef lookup**

```rust
    let lang_def = language.def();
    let mut fields = Vec::new();

    match lang_def.field_style {
        FieldStyle::None => return fields,
        FieldStyle::NameFirst { separators, strip_prefixes } => {
            let prefixes: Vec<&str> = strip_prefixes.split_whitespace().collect();
            for line in content.lines() {
                let trimmed = line.trim();
                if should_skip_line(trimmed) { continue; }
                let mut work = trimmed;
                for prefix in &prefixes {
                    if let Some(rest) = work.strip_prefix(prefix) {
                        work = rest.trim_start();
                    }
                }
                // Take first token before any separator char
                let name = work.split(|c: char| separators.contains(c))
                    .next()
                    .map(|s| s.trim().trim_end_matches(','));
                if let Some(name) = validate_field_name(name) {
                    fields.push(tokenize_identifier(name).join(" "));
                }
                if fields.len() >= 15 { break; }
            }
        }
        FieldStyle::TypeFirst { strip_prefixes } => {
            let prefixes: Vec<&str> = strip_prefixes.split_whitespace().collect();
            for line in content.lines() {
                let trimmed = line.trim();
                if should_skip_line(trimmed) { continue; }
                let mut work = trimmed;
                for prefix in &prefixes {
                    if let Some(rest) = work.strip_prefix(prefix) {
                        work = rest.trim_start();
                    }
                }
                // Type-first: take LAST token before ; , = {
                let end_trimmed = work.split([';', ',', '=', '{'])
                    .next()
                    .unwrap_or(work)
                    .trim();
                let name = end_trimmed.split_whitespace().last();
                // Strip pointer/reference markers
                let name = name.map(|n| n.trim_start_matches('*').trim_start_matches('&'));
                if let Some(name) = validate_field_name(name) {
                    fields.push(tokenize_identifier(name).join(" "));
                }
                if fields.len() >= 15 { break; }
            }
        }
    }

    if fields.is_empty() && !content.is_empty() {
        tracing::trace!(%language, "No fields extracted from content");
    }
    fields
}
```

- [ ] **Step 3: Extract `should_skip_line` helper**

Move the skip-line logic into a shared function. Make it data-driven where possible, but keep the current comment/brace patterns as-is (they're universal).

```rust
fn should_skip_line(trimmed: &str) -> bool {
    trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed == "{"
        || trimmed == "}"
        || trimmed.starts_with("pub struct")
        || trimmed.starts_with("struct")
        || trimmed.starts_with("pub enum")
        || trimmed.starts_with("enum")
        || trimmed.starts_with("class")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("export")
        || trimmed.starts_with("data class")
        || trimmed.starts_with("sealed class")
        || trimmed.starts_with("case class")
        || trimmed.starts_with("interface")
        || trimmed.starts_with("@property")
        || trimmed.starts_with("defstruct")
}
```

- [ ] **Step 4: Extract `validate_field_name` helper**

```rust
fn validate_field_name(name: Option<&str>) -> Option<&str> {
    let name = name?.trim();
    if name.is_empty()
        || name.len() <= 1
        || name.contains('(')
        || name.contains('{')
        || !name.starts_with(|c: char| c.is_alphabetic() || c == '_')
    {
        return None;
    }
    Some(name)
}
```

- [ ] **Step 5: Run ALL tests — baseline must pass unchanged**

```bash
cargo test --features gpu-index -- extract_field_names -v
```

If any baseline test fails, the refactoring changed behavior for that language. Fix it.

- [ ] **Step 6: Commit**

### Task 4: Tests for Newly Covered Languages

**Files:**
- Modify: `src/nl.rs` (test module)

- [ ] **Step 1: Write tests for type-first languages**

```rust
#[test]
fn extract_field_names_c() {
    let content = "struct Config {\n    const char *name;\n    int max_size;\n    bool enabled;\n};";
    let fields = extract_field_names(content, Language::C);
    assert!(fields.iter().any(|f| f.contains("name")));
    assert!(fields.iter().any(|f| f.contains("max") || f.contains("size")));
}

#[test]
fn extract_field_names_csharp() {
    let content = "class Config {\n    public string Name;\n    private int MaxSize;\n    protected bool Enabled;\n}";
    let fields = extract_field_names(content, Language::CSharp);
    assert!(fields.iter().any(|f| f.contains("Name")));
}
```

- [ ] **Step 2: Write tests for name-first languages with keyword prefixes**

```rust
#[test]
fn extract_field_names_kotlin() {
    let content = "data class Config(\n    val name: String,\n    var maxSize: Int,\n    private val enabled: Boolean\n)";
    let fields = extract_field_names(content, Language::Kotlin);
    assert!(fields.iter().any(|f| f.contains("name")));
    assert!(fields.iter().any(|f| f.contains("max")));
}

#[test]
fn extract_field_names_swift() {
    let content = "struct Config {\n    let name: String\n    var maxSize: Int\n    weak var delegate: Delegate?\n}";
    let fields = extract_field_names(content, Language::Swift);
    assert!(fields.iter().any(|f| f.contains("name")));
}
```

- [ ] **Step 3: Run full test suite**

- [ ] **Step 4: Commit**

### Task 5: Method Extraction Improvement (Optional)

The existing `extract_method_name_from_line` already has a good generic fallback (lines 896-932) that handles most languages. The main gap is languages with unique keywords like `fun` (Kotlin), `func` (Swift), `proc` (Nim).

- [ ] **Step 1: Add function keywords to the generic fallback**

In the `_ =>` branch of `extract_method_name_from_line`, add:
```rust
if let Some(rest) = work.strip_prefix("fun ") { ... }   // Kotlin
if let Some(rest) = work.strip_prefix("proc ") { ... }  // Nim, Tcl
if let Some(rest) = work.strip_prefix("sub ") { ... }   // Perl, VB.NET
if let Some(rest) = work.strip_prefix("method ") { ... } // various
```

- [ ] **Step 2: Add tracing span**

```rust
fn extract_method_name_from_line(line: &str, language: Language) -> Option<String> {
    // No span here — called per-line, too noisy. Trace at caller level.
```

Actually, keep method extraction as-is for now. The generic fallback already handles the `name(` pattern which covers most languages. The function keyword additions are marginal value.

- [ ] **Step 3: Skip this task if time-constrained**

### Task 6: Verify and Reindex

- [ ] **Step 1: Reindex cqs project**

```bash
cqs index
```

- [ ] **Step 2: Spot-check NL descriptions**

```bash
cqs explain ScoringContext --json | jq .nl
cqs explain LanguageDef --json | jq .nl
```

Verify that struct descriptions now include field names for languages beyond the original 6.

- [ ] **Step 3: Run full test suite**

```bash
cargo test --features gpu-index
```

- [ ] **Step 4: Commit and create PR**
