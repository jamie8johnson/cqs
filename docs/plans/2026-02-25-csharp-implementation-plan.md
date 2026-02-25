# C# Language Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add C# as the 10th supported language with infrastructure improvements that make future language additions cheaper.

**Architecture:** Three layers — (1) infrastructure refactors to LanguageDef, ChunkType, and SQL queries that generalize language support, (2) C# language module implementing the LanguageDef interface, (3) backfill existing 9 languages with new fields. Infrastructure first so C# is just "fill in the struct."

**Tech Stack:** tree-sitter-c-sharp 0.23, Rust feature flags, SQLite text columns for chunk types.

**Design doc:** `docs/plans/2026-02-25-csharp-language-support-design.md`

---

## Task 1: Add ChunkType variants (Property, Delegate, Event)

**Files:**
- Modify: `src/language/mod.rs:168-251` (ChunkType enum, Display, FromStr, is_callable, error message)
- Modify: `src/nl.rs:338-348` (type_word match)
- Modify: `src/parser/chunk.rs:18-26` (capture_types table)
- Modify: `src/cli/mod.rs:150` (help text)
- Modify: `src/cli/commands/query.rs:54` (error message)
- Modify: `src/cli/commands/explain.rs:94` (use is_callable)
- Modify: `src/cli/commands/read.rs:135` (use is_callable)

**Step 1: Add variants to ChunkType enum**

In `src/language/mod.rs`, add after `Section` (line 188):

```rust
/// Property (C# get/set properties)
Property,
/// Delegate type declaration (C#)
Delegate,
/// Event declaration (C#)
Event,
```

**Step 2: Update is_callable to include Property**

At line 193-194, change to:

```rust
pub fn is_callable(self) -> bool {
    matches!(self, ChunkType::Function | ChunkType::Method | ChunkType::Property)
}
```

**Step 3: Add callable_sql_list() method**

Below `is_callable`, add:

```rust
/// SQL IN clause string for all callable chunk types.
/// Exhaustive: update when adding new callable ChunkType variants.
pub fn callable_sql_list() -> String {
    // Derived from is_callable() — keep in sync
    let callable = [ChunkType::Function, ChunkType::Method, ChunkType::Property];
    callable
        .iter()
        .map(|ct| format!("'{}'", ct))
        .collect::<Vec<_>>()
        .join(",")
}
```

**Step 4: Update Display impl**

Add 3 arms after `Section` line:

```rust
ChunkType::Property => write!(f, "property"),
ChunkType::Delegate => write!(f, "delegate"),
ChunkType::Event => write!(f, "event"),
```

**Step 5: Update FromStr impl**

Add 3 arms after `"section"` line:

```rust
"property" => Ok(ChunkType::Property),
"delegate" => Ok(ChunkType::Delegate),
"event" => Ok(ChunkType::Event),
```

**Step 6: Update ParseChunkTypeError message**

At line 225, add `property, delegate, event` to the valid options string.

**Step 7: Update nl.rs type_word match**

In `src/nl.rs:338-348`, add 3 arms:

```rust
ChunkType::Property => "property",
ChunkType::Delegate => "delegate",
ChunkType::Event => "event",
```

**Step 8: Update capture_types table in parser/chunk.rs**

In `src/parser/chunk.rs:18-26`, add 3 entries:

```rust
("property", ChunkType::Property),
("delegate", ChunkType::Delegate),
("event", ChunkType::Event),
```

**Step 9: Update CLI help text**

In `src/cli/mod.rs:150`, add `property, delegate, event` to the help string.

In `src/cli/commands/query.rs:54`, add `property, delegate, event` to the error message.

**Step 10: Replace inline callable checks with is_callable()**

In `src/cli/commands/explain.rs:94`, change:
```rust
let hints = if matches!(chunk.chunk_type, ChunkType::Function | ChunkType::Method) {
```
to:
```rust
let hints = if chunk.chunk_type.is_callable() {
```

Same in `src/cli/commands/read.rs:135`.

**Step 11: Update ChunkType tests**

In `src/language/mod.rs` test section, add to `test_chunk_type_from_str_valid`:
```rust
assert_eq!("property".parse::<ChunkType>().unwrap(), ChunkType::Property);
assert_eq!("delegate".parse::<ChunkType>().unwrap(), ChunkType::Delegate);
assert_eq!("event".parse::<ChunkType>().unwrap(), ChunkType::Event);
```

Add Property, Delegate, Event to the `test_chunk_type_display_roundtrip` array.

Add test for `callable_sql_list`:
```rust
#[test]
fn test_callable_sql_list() {
    let list = ChunkType::callable_sql_list();
    assert!(list.contains("'function'"));
    assert!(list.contains("'method'"));
    assert!(list.contains("'property'"));
    assert!(!list.contains("'class'"));
    assert!(!list.contains("'delegate'"));
    assert!(!list.contains("'event'"));
}
```

**Step 12: Build and test**

Run: `cargo build --features gpu-index 2>&1 | grep -E "error|warning"`
Run: `cargo test --features gpu-index -- chunk_type 2>&1`
Expected: All pass, no warnings.

**Step 13: Commit**

```bash
cargo fmt
git add src/language/mod.rs src/nl.rs src/parser/chunk.rs src/cli/mod.rs src/cli/commands/query.rs src/cli/commands/explain.rs src/cli/commands/read.rs
git commit -m "feat: add Property, Delegate, Event ChunkType variants + callable_sql_list()"
```

---

## Task 2: Replace hardcoded callable SQL queries

**Files:**
- Modify: `src/store/calls.rs:716,967` (2 hardcoded queries)
- Modify: `src/store/chunks.rs:934` (1 hardcoded query)

**Step 1: Replace in calls.rs line 716**

Change:
```sql
WHERE c.chunk_type IN ('function', 'method')
```
to:
```rust
format!("...WHERE c.chunk_type IN ({})...", ChunkType::callable_sql_list())
```

The query at line 712-719 is a static `sqlx::query(...)`. Convert to `format!()` then `sqlx::query(&sql)`.

**Step 2: Replace in calls.rs line 967**

Same pattern — the query at line 963-972 already uses `format!()`, just replace the hardcoded IN clause.

**Step 3: Replace in chunks.rs line 934**

Same pattern — convert to `format!()` with `callable_sql_list()`.

**Step 4: Add import**

Add `use crate::parser::types::ChunkType;` (or appropriate path) to both files if not already imported.

**Step 5: Build and test**

Run: `cargo build --features gpu-index 2>&1 | grep -E "error|warning"`
Run: `cargo test --features gpu-index -- "dead\|callable\|find_functions" 2>&1`
Expected: Pass. Existing behavior unchanged (list still contains function + method).

**Step 6: Commit**

```bash
cargo fmt
git add src/store/calls.rs src/store/chunks.rs
git commit -m "refactor: replace hardcoded callable SQL with ChunkType::callable_sql_list()"
```

---

## Task 3: Add LanguageDef fields for container extraction and common types

**Files:**
- Modify: `src/language/mod.rs:118-152` (LanguageDef struct)
- Modify: `src/parser/chunk.rs:210-283` (extract_container_type_name + infer_chunk_type)
- Modify: `src/focused_read.rs` (COMMON_TYPES → union of per-language sets)
- Modify: `src/lib.rs:100` (re-export may need adjustment)

**Step 1: Add 3 new fields to LanguageDef**

In `src/language/mod.rs:118-152`, add after `type_query`:

```rust
/// Standard library / builtin types to exclude from type-edge analysis.
/// Each language defines its own set. At runtime, these are unioned into
/// a single COMMON_TYPES set in focused_read.rs.
pub common_types: &'static [&'static str],

/// Node kinds that are intermediate body containers (walk up to parent for name).
/// e.g., "class_body" (JS/TS/Java), "declaration_list" (C#/Rust).
/// Used by the generic container type extraction algorithm.
pub container_body_kinds: &'static [&'static str],

/// Override for extracting parent type name from a method container node.
/// None = use default algorithm (walk up from body kinds, read "name" field).
/// Only Rust needs an override (impl_item uses "type" field, not "name").
pub extract_container_name: Option<fn(tree_sitter::Node, &str) -> Option<String>>,
```

**Step 2: Replace extract_container_type_name with data-driven algorithm**

In `src/parser/chunk.rs`, replace the entire `extract_container_type_name` function (lines 236-283) with:

```rust
/// Extract type name from a method container node.
/// Uses LanguageDef fields: container_body_kinds and extract_container_name.
fn extract_container_type_name(
    container: tree_sitter::Node,
    language: Language,
    source: &str,
) -> Option<String> {
    let def = language.def();

    // If language provides a custom extractor, use it
    if let Some(extractor) = def.extract_container_name {
        return extractor(container, source);
    }

    // Default algorithm:
    // 1. If container is a body node (e.g., class_body, declaration_list), walk up
    // 2. Read the "name" field from the resulting node
    let type_node = if def.container_body_kinds.contains(&container.kind()) {
        container.parent()
    } else {
        Some(container)
    };

    type_node.and_then(|n| {
        n.child_by_field_name("name")
            .map(|name| source[name.byte_range()].to_string())
    })
}
```

**Step 3: Create Rust-specific extractor**

In `src/language/rust.rs`, add a function (before the DEFINITION static):

```rust
/// Custom container name extraction for Rust.
/// impl_item uses "type" field (not "name"), and may wrap in generic_type.
fn extract_container_name_rust(container: tree_sitter::Node, source: &str) -> Option<String> {
    if container.kind() == "impl_item" {
        container.child_by_field_name("type").and_then(|t| {
            if t.kind() == "type_identifier" {
                Some(source[t.byte_range()].to_string())
            } else {
                // generic_type wraps type_identifier: Foo<T>
                let mut cursor = t.walk();
                for child in t.children(&mut cursor) {
                    if child.kind() == "type_identifier" {
                        return Some(source[child.byte_range()].to_string());
                    }
                }
                None
            }
        })
    } else {
        // trait_item: read "name" field
        container
            .child_by_field_name("name")
            .map(|n| source[n.byte_range()].to_string())
    }
}
```

**Step 4: Update COMMON_TYPES in focused_read.rs**

Replace the hardcoded set with a union built from all enabled languages' `common_types` fields:

```rust
use crate::language::REGISTRY;

pub static COMMON_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut set = HashSet::new();
    for def in REGISTRY.all() {
        set.extend(def.common_types.iter().copied());
    }
    set
});
```

**Step 5: Build and verify**

Run: `cargo build --features gpu-index 2>&1 | grep -E "error|warning"`
Expected: Won't compile yet — existing language files don't have the new fields. That's Task 4.

**Step 6: Commit (partial — will compile after Task 4)**

Don't commit yet. Continue to Task 4.

---

## Task 4: Backfill existing 9 languages with new LanguageDef fields

**Files:**
- Modify: `src/language/rust.rs` (add common_types, container_body_kinds, extract_container_name)
- Modify: `src/language/python.rs`
- Modify: `src/language/typescript.rs`
- Modify: `src/language/javascript.rs`
- Modify: `src/language/go.rs`
- Modify: `src/language/c.rs`
- Modify: `src/language/java.rs`
- Modify: `src/language/sql.rs`
- Modify: `src/language/markdown.rs`

**Step 1: Rust**

Add to DEFINITION in `rust.rs`:
```rust
common_types: &[
    "String", "Vec", "Result", "Option", "Box", "Arc", "Rc",
    "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Path", "PathBuf",
    "Value", "Error", "Self", "None", "Some", "Ok", "Err",
    "Mutex", "RwLock", "Cow", "Pin", "Future", "Iterator",
    "Display", "Debug", "Clone", "Default", "Send", "Sync", "Sized", "Copy",
    "From", "Into", "AsRef", "AsMut", "Deref", "DerefMut",
    "Read", "Write", "Seek", "BufRead", "ToString", "Serialize", "Deserialize",
],
container_body_kinds: &["declaration_list"],
extract_container_name: Some(extract_container_name_rust),
```

Note: Rust's `impl_item` already appears in `method_containers`. The `declaration_list` is the body of `trait_item`. But `impl_item` is the actual container in the match — it doesn't have a body kind to walk up from since `infer_chunk_type` matches `impl_item` directly. Actually, looking at the code: `method_containers: &["impl_item", "trait_item"]`. These are the containers themselves, not their body nodes. So `container_body_kinds` for Rust should be `&[]` — the custom extractor handles everything.

Revised: `container_body_kinds: &[],` for Rust.

**Step 2: Python**

```rust
common_types: &[
    "str", "int", "float", "bool", "list", "dict", "set", "tuple",
    "None", "Any", "Optional", "Union", "List", "Dict", "Set", "Tuple",
    "Type", "Callable", "Iterator", "Generator", "Coroutine",
    "Exception", "ValueError", "TypeError", "KeyError", "IndexError",
    "Path", "Self",
],
container_body_kinds: &[],
extract_container_name: None,
```

Python's `method_containers` is `["class_definition"]` — the container IS the class_definition, which has a `name` field directly. No body walking needed. The default algorithm's `container_body_kinds` check won't match, and it'll try `child_by_field_name("name")` directly on `class_definition`. That works.

**Step 3: TypeScript**

```rust
common_types: &[
    "string", "number", "boolean", "void", "null", "undefined", "any", "never", "unknown",
    "Array", "Map", "Set", "Promise", "Record", "Partial", "Required", "Readonly",
    "Pick", "Omit", "Exclude", "Extract", "NonNullable", "ReturnType",
    "Date", "Error", "RegExp", "Function", "Object", "Symbol",
],
container_body_kinds: &["class_body"],
extract_container_name: None,
```

**Step 4: JavaScript**

```rust
common_types: &[
    "Array", "Map", "Set", "Promise", "Date", "Error", "RegExp",
    "Function", "Object", "Symbol", "WeakMap", "WeakSet",
],
container_body_kinds: &["class_body"],
extract_container_name: None,
```

**Step 5: Go**

```rust
common_types: &[
    "string", "int", "int8", "int16", "int32", "int64",
    "uint", "uint8", "uint16", "uint32", "uint64",
    "float32", "float64", "bool", "byte", "rune", "error",
    "any", "comparable", "Context",
],
container_body_kinds: &[],
extract_container_name: None,
```

Go uses `method_node_kinds` + receiver extraction — no containers.

**Step 6: C**

```rust
common_types: &[
    "int", "char", "float", "double", "void", "long", "short", "unsigned",
    "size_t", "ssize_t", "ptrdiff_t", "FILE", "bool",
],
container_body_kinds: &[],
extract_container_name: None,
```

**Step 7: Java**

```rust
common_types: &[
    "String", "Object", "Integer", "Long", "Double", "Float", "Boolean", "Byte", "Character",
    "List", "ArrayList", "Map", "HashMap", "Set", "HashSet", "Collection",
    "Iterator", "Iterable", "Optional", "Stream",
    "Exception", "RuntimeException", "IOException",
    "Class", "Void", "Comparable", "Serializable", "Cloneable",
],
container_body_kinds: &["class_body"],
extract_container_name: None,
```

**Step 8: SQL**

```rust
common_types: &[],
container_body_kinds: &[],
extract_container_name: None,
```

**Step 9: Markdown**

```rust
common_types: &[],
container_body_kinds: &[],
extract_container_name: None,
```

**Step 10: Build and test all existing languages**

Run: `cargo build --features gpu-index 2>&1 | grep -E "error|warning"`
Run: `cargo test --features gpu-index 2>&1 | tail -5`
Expected: All 1100+ tests pass. No behavioral change — Rust container extraction uses custom extractor that does the same thing as the old match arm, COMMON_TYPES union still contains all the same Rust types plus new per-language types.

**Step 11: Verify COMMON_TYPES backward compatibility**

The old COMMON_TYPES had 44 entries (all Rust types). The new union will have those 44 plus types from other languages. Verify the Rust types are still present:

Run: `cargo test --features gpu-index -- "common_type\|focused_read" 2>&1`

Add a regression test if none exists:
```rust
#[test]
fn test_common_types_contains_rust_basics() {
    assert!(COMMON_TYPES.contains("String"));
    assert!(COMMON_TYPES.contains("Vec"));
    assert!(COMMON_TYPES.contains("Result"));
    assert!(COMMON_TYPES.contains("Option"));
}
```

**Step 12: Commit Tasks 3 + 4 together**

```bash
cargo fmt
git add src/language/ src/parser/chunk.rs src/focused_read.rs src/lib.rs
git commit -m "refactor: per-language common_types + data-driven container extraction

Add common_types, container_body_kinds, extract_container_name fields to
LanguageDef. Backfill all 9 existing languages. COMMON_TYPES is now a
union of per-language sets. Container name extraction is data-driven
(Rust has custom override for impl_item)."
```

---

## Task 5: Add tree-sitter-c-sharp dependency and feature flag

**Files:**
- Modify: `Cargo.toml:30-38` (add dep), `102-115` (add feature)

**Step 1: Add dependency**

After the `tree-sitter-java` line in Cargo.toml, add:
```toml
tree-sitter-c-sharp = { version = "0.23", optional = true }
```

**Step 2: Add feature flag**

After `lang-java` in features section:
```toml
lang-csharp = ["dep:tree-sitter-c-sharp"]
```

Add `"lang-csharp"` to both the `default` and `lang-all` feature lists.

**Step 3: Verify it resolves**

Run: `cargo check --features gpu-index 2>&1 | grep -E "error|Compiling tree-sitter-c-sharp"`
Expected: `Compiling tree-sitter-c-sharp v0.23.x`

**Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add tree-sitter-c-sharp dependency and lang-csharp feature"
```

---

## Task 6: Create C# language module

**Files:**
- Create: `src/language/csharp.rs`
- Modify: `src/language/mod.rs:314-333` (add to define_languages!)

**Step 1: Create csharp.rs**

Create `src/language/csharp.rs` with the full LanguageDef. Follow the Java pattern (closest language structurally). Include:

- CHUNK_QUERY — from design doc, all 16 patterns
- CALL_QUERY — 5 patterns (invocation_expression + object_creation_expression)
- TYPE_QUERY — from design doc, all classified captures + catch-all
- STOPWORDS — from design doc
- COMMON_TYPES — C# builtin and framework types
- `extract_return` — C# return type extraction (like Java: type is before method name, skip modifiers including `async`)
- DEFINITION static with all fields including new ones (common_types, container_body_kinds, extract_container_name)

```rust
//! C# language definition

use super::{LanguageDef, SignatureStyle};

const CHUNK_QUERY: &str = r#"
;; Functions/methods
(method_declaration name: (identifier) @name) @function
(constructor_declaration name: (identifier) @name) @function
(operator_declaration) @function
(indexer_declaration) @function
(local_function_statement name: (identifier) @name) @function

;; Properties
(property_declaration name: (identifier) @name) @property

;; Delegates
(delegate_declaration name: (identifier) @name) @delegate

;; Events
(event_field_declaration (variable_declaration (variable_declarator (identifier) @name))) @event
(event_declaration name: (identifier) @name) @event

;; Types
(class_declaration name: (identifier) @name) @class
(struct_declaration name: (identifier) @name) @struct
(record_declaration name: (identifier) @name) @struct
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum
"#;

const CALL_QUERY: &str = r#"
(invocation_expression
  function: (member_access_expression name: (identifier) @callee))
(invocation_expression
  function: (identifier) @callee)
(object_creation_expression type: (identifier) @callee)
(object_creation_expression type: (generic_name (identifier) @callee))
(object_creation_expression type: (qualified_name (identifier) @callee))
"#;

const TYPE_QUERY: &str = r#"
;; Param
(parameter type: (identifier) @param_type)
(parameter type: (generic_name (identifier) @param_type))
(parameter type: (qualified_name (identifier) @param_type))
(parameter type: (nullable_type (identifier) @param_type))
(parameter type: (array_type (identifier) @param_type))

;; Return — method_declaration uses "returns" field (not "type"!)
(method_declaration returns: (identifier) @return_type)
(method_declaration returns: (generic_name (identifier) @return_type))
(method_declaration returns: (qualified_name (identifier) @return_type))
(method_declaration returns: (nullable_type (identifier) @return_type))
(delegate_declaration type: (identifier) @return_type)
(delegate_declaration type: (generic_name (identifier) @return_type))
(local_function_statement type: (identifier) @return_type)
(local_function_statement type: (generic_name (identifier) @return_type))

;; Field
(field_declaration (variable_declaration type: (identifier) @field_type))
(field_declaration (variable_declaration type: (generic_name (identifier) @field_type)))
(property_declaration type: (identifier) @field_type)
(property_declaration type: (generic_name (identifier) @field_type))

;; Impl — base class, interface implementations
(base_list (identifier) @impl_type)
(base_list (generic_name (identifier) @impl_type))
(base_list (qualified_name (identifier) @impl_type))

;; Bound — generic constraints
(type_parameter_constraint (type (identifier) @bound_type))
(type_parameter_constraint (type (generic_name (identifier) @bound_type)))

;; Alias
(using_directive name: (identifier) @alias_type)
"#;
// NOTE: No catch-all (identifier) @type_ref — C# uses "identifier" for everything
// (variables, methods, params, etc.), so a catch-all would drown real types in noise.
// Classified captures above are sufficient.

const DOC_NODES: &[&str] = &["comment"];

const STOPWORDS: &[&str] = &[
    "public", "private", "protected", "internal", "static", "readonly",
    "sealed", "abstract", "virtual", "override", "async", "await",
    "class", "struct", "interface", "enum", "namespace", "using",
    "return", "if", "else", "for", "foreach", "while", "do", "switch",
    "case", "break", "continue", "new", "this", "base", "try", "catch",
    "finally", "throw", "var", "void", "int", "string", "bool", "true",
    "false", "null", "get", "set", "value", "where", "partial",
    "event", "delegate", "record", "yield", "in", "out", "ref",
];

const COMMON_TYPES_LIST: &[&str] = &[
    "string", "int", "bool", "object", "void", "double", "float",
    "long", "byte", "char", "decimal", "short", "uint", "ulong",
    "Task", "ValueTask", "List", "Dictionary", "HashSet", "Queue", "Stack",
    "IEnumerable", "IList", "IDictionary", "ICollection", "IQueryable",
    "Action", "Func", "Predicate", "EventHandler", "EventArgs",
    "IDisposable", "CancellationToken", "ILogger",
    "StringBuilder", "Exception", "Nullable", "Span", "Memory", "ReadOnlySpan",
    "IServiceProvider", "HttpContext", "IConfiguration",
];

fn extract_return(signature: &str) -> Option<String> {
    // C#: return type before method name, like Java
    // e.g., "public async Task<int> GetValue(..." → "Task<int>"
    // Must skip: access modifiers, static, async, virtual, override, etc.
    if let Some(paren) = signature.find('(') {
        let before = signature[..paren].trim();
        let words: Vec<&str> = before.split_whitespace().collect();
        if words.len() >= 2 {
            let ret_type = words[words.len() - 2];
            if !matches!(
                ret_type,
                "void" | "public" | "private" | "protected" | "internal"
                | "static" | "abstract" | "virtual" | "override" | "sealed"
                | "async" | "extern" | "partial" | "new" | "unsafe"
            ) {
                let ret_words = crate::nl::tokenize_identifier(ret_type).join(" ");
                return Some(format!("Returns {}", ret_words));
            }
        }
    }
    None
}

static DEFINITION: LanguageDef = LanguageDef {
    name: "csharp",
    grammar: Some(|| tree_sitter_c_sharp::LANGUAGE.into()),
    extensions: &["cs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: DOC_NODES,
    method_node_kinds: &[],
    method_containers: &[
        "class_declaration", "struct_declaration", "record_declaration",
        "interface_declaration", "declaration_list",
    ],
    stopwords: STOPWORDS,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.cs")),
    type_query: Some(TYPE_QUERY),
    common_types: COMMON_TYPES_LIST,
    container_body_kinds: &["declaration_list"],
    extract_container_name: None,
};

pub fn definition() -> &'static LanguageDef {
    &DEFINITION
}
```

**Step 2: Register in define_languages! macro**

In `src/language/mod.rs`, add after the Java line (line 328):

```rust
/// C# (.cs files)
CSharp => "csharp", feature = "lang-csharp", module = csharp;
```

**Step 3: Build**

Run: `cargo build --features gpu-index 2>&1 | grep -E "error|warning"`
Expected: Clean build.

**Step 4: Smoke test — parse a C# file**

Create a temp C# file and verify parsing works:

```bash
cat > /tmp/test.cs << 'EOF'
namespace MyApp;

public class Calculator
{
    public int Value { get; set; }

    public int Add(int a, int b)
    {
        return a + b;
    }

    public static Calculator Create()
    {
        return new Calculator();
    }
}

public delegate void OnCalculated(int result);

public interface ICalculator
{
    int Add(int a, int b);
}
EOF
```

Run: `cqs index` on a project with .cs files, or test via `cargo test`.

**Step 5: Commit**

```bash
cargo fmt
git add src/language/csharp.rs src/language/mod.rs
git commit -m "feat: add C# language support (tree-sitter-c-sharp)

Chunk extraction: class, struct, record, interface, enum, method,
constructor, property, delegate, event, local function, operator, indexer.
Call extraction: method calls, constructor calls (new expressions).
Type extraction: params, returns, fields, base_list, generic constraints."
```

---

## Task 7: C# unit tests

**Files:**
- Modify: `src/parser/chunk.rs` (add C# parse tests alongside existing language tests)
- Modify: `src/language/csharp.rs` (add return extraction unit tests)

Uses the existing `write_temp_file(content, "cs")` + `Parser::new().unwrap()` + `parser.parse_file(file.path())` pattern from `parser/chunk.rs:381-404`.

**Step 1: Add chunk extraction tests in parser/chunk.rs**

Add a `csharp_tests` module inside the existing `parse_tests` module (after the other language tests):

```rust
#[cfg(feature = "lang-csharp")]
mod csharp_tests {
    use super::*;

    #[test]
    fn test_parse_csharp_class_and_method() {
        let content = r#"
public class Calculator {
    public int Add(int a, int b) {
        return a + b;
    }
}
"#;
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        let class = chunks.iter().find(|c| c.name == "Calculator").unwrap();
        assert_eq!(class.chunk_type, ChunkType::Class);

        let method = chunks.iter().find(|c| c.name == "Add").unwrap();
        assert_eq!(method.chunk_type, ChunkType::Method);
    }

    #[test]
    fn test_parse_csharp_property() {
        let content = r#"
public class Foo {
    public int Value { get; set; }
}
"#;
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "Value" && c.chunk_type == ChunkType::Property));
    }

    #[test]
    fn test_parse_csharp_delegate() {
        let content = "public delegate void OnComplete(int result);";
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "OnComplete" && c.chunk_type == ChunkType::Delegate));
    }

    #[test]
    fn test_parse_csharp_event() {
        let content = r#"
public class Foo {
    public event EventHandler Changed;
}
"#;
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "Changed" && c.chunk_type == ChunkType::Event));
    }

    #[test]
    fn test_parse_csharp_interface() {
        let content = "public interface IFoo { void Bar(); }";
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "IFoo" && c.chunk_type == ChunkType::Interface));
    }

    #[test]
    fn test_parse_csharp_enum() {
        let content = "public enum Color { Red, Green, Blue }";
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "Color" && c.chunk_type == ChunkType::Enum));
    }

    #[test]
    fn test_parse_csharp_struct() {
        let content = "public struct Point { public int X; public int Y; }";
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "Point" && c.chunk_type == ChunkType::Struct));
    }

    #[test]
    fn test_parse_csharp_record_maps_to_struct() {
        let content = "public record Person(string Name, int Age);";
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "Person" && c.chunk_type == ChunkType::Struct));
    }

    #[test]
    fn test_parse_csharp_constructor_inferred_method() {
        let content = r#"
public class Foo {
    public Foo(int x) { }
}
"#;
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        // Constructor → Function → inferred to Method (inside class declaration_list)
        let ctor = chunks.iter().find(|c| c.name == "Foo" && c.chunk_type == ChunkType::Method);
        assert!(ctor.is_some(), "Constructor should be inferred as Method");
    }

    #[test]
    fn test_parse_csharp_local_function() {
        let content = r#"
public class Foo {
    public void Bar() {
        int Helper(int x) { return x + 1; }
        Helper(5);
    }
}
"#;
        let file = write_temp_file(content, "cs");
        let parser = Parser::new().unwrap();
        let chunks = parser.parse_file(file.path()).unwrap();

        assert!(chunks.iter().any(|c| c.name == "Helper"));
    }
}
```

**Step 2: Add return type extraction tests in csharp.rs**

Add at the bottom of `csharp.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_return_csharp() {
        assert_eq!(
            extract_return("public int Add(int a, int b)"),
            Some("Returns int".to_string())
        );
        assert_eq!(
            extract_return("public void DoSomething()"),
            None
        );
        assert_eq!(
            extract_return("private static string GetValue()"),
            Some("Returns string".to_string())
        );
    }
}
```

**Step 3: Run tests**

Run: `cargo test --features gpu-index -- "csharp\|extract_return" 2>&1`
Expected: All pass.

**Step 4: Commit**

```bash
cargo fmt
git add src/parser/chunk.rs src/language/csharp.rs
git commit -m "test: C# chunk, call, and type extraction tests"
```

---

## Task 8: Update registry tests and integration test

**Files:**
- Modify: `src/language/mod.rs` (test section — add C# to existing tests)

**Step 1: Update registry tests**

In `test_registry_by_extension`, add:
```rust
#[cfg(feature = "lang-csharp")]
assert!(REGISTRY.from_extension("cs").is_some());
```

In `test_registry_all_languages`, add:
```rust
#[cfg(feature = "lang-csharp")]
{
    expected += 1;
}
```

In `test_from_extension`, add:
```rust
assert_eq!(Language::from_extension("cs"), Some(Language::CSharp));
```

In `test_language_from_str`, add:
```rust
assert_eq!("csharp".parse::<Language>().unwrap(), Language::CSharp);
```

In `test_language_display`, add:
```rust
assert_eq!(Language::CSharp.to_string(), "csharp");
```

In `test_language_def_extract_return`, add:
```rust
assert_eq!(
    (Language::CSharp.def().extract_return_nl)("public int Add(int a, int b)"),
    Some("Returns int".to_string())
);
```

**Step 2: Run full test suite**

Run: `cargo test --features gpu-index 2>&1 | tail -5`
Expected: All pass, count increased by new C# tests.

**Step 3: Commit**

```bash
cargo fmt
git add src/language/mod.rs
git commit -m "test: add C# to language registry tests"
```

---

## Task 9: Documentation updates

**Files:**
- Modify: `README.md` (language count and list)
- Modify: `CONTRIBUTING.md` (architecture section)
- Modify: `CHANGELOG.md` (new entry)
- Modify: `ROADMAP.md` (check off C#)
- Modify: `PROJECT_CONTINUITY.md` (update state)

**Step 1: Update README.md**

Find the supported languages list. Change "9 languages" → "10 languages". Add C# to the list.

**Step 2: Update CONTRIBUTING.md**

In the Architecture Overview section, add C# to the language list. If LanguageDef fields are documented there, add the new fields.

**Step 3: Update CHANGELOG.md**

Add entry under the next version:
```markdown
### Added
- C# language support: class, struct, record, interface, enum, method, constructor, property, delegate, event, local function, operator, indexer
- 3 new ChunkType variants: Property, Delegate, Event
- Per-language common type filtering (LanguageDef.common_types)
- Dynamic callable SQL filter (ChunkType::callable_sql_list())
- Data-driven container type extraction (LanguageDef.container_body_kinds)
```

**Step 4: Update ROADMAP.md**

Check off `C# language support` under Expansion.

**Step 5: Update PROJECT_CONTINUITY.md**

Update "Right Now" section.

**Step 6: Commit**

```bash
cargo fmt
git add README.md CONTRIBUTING.md CHANGELOG.md ROADMAP.md PROJECT_CONTINUITY.md
git commit -m "docs: add C# to supported languages, update architecture docs"
```

---

## Task 10: Final verification and PR

**Step 1: Full build**

Run: `cargo build --release --features gpu-index 2>&1 | grep -E "error|warning"`
Expected: Clean.

**Step 2: Full test suite**

Run: `cargo test --features gpu-index 2>&1 | tail -10`
Expected: All pass. Note new test count.

**Step 3: Clippy**

Run: `cargo clippy --features gpu-index 2>&1 | grep -E "warning|error"`
Expected: Clean.

**Step 4: Install binary and reindex**

```bash
systemctl --user stop cqs-watch
cp ~/.cargo-target/cqs/release/cqs ~/.cargo/bin/cqs
systemctl --user start cqs-watch
cqs index
```

**Step 5: Smoke test with real C# code**

If C# files are available, verify `cqs search` finds them. Otherwise create a test file and index it.

**Step 6: Create PR**

Branch name: `feat/csharp-language-support`
PR title: `feat: C# language support + language infrastructure improvements`

Use `/pr` skill or manual PR creation via PowerShell with `--body-file`.
