# C# Language Support Design

Date: 2026-02-25

## Goal

Add C# as the 10th supported language. Design infrastructure changes to make future language additions cheaper.

## Scope

- C# parsing via tree-sitter-c-sharp (chunks, calls, type edges)
- 3 new ChunkType variants: Property, Delegate, Event
- Per-language common types (replaces global Rust-centric set)
- Dynamic callable SQL filter (replaces hardcoded `IN ('function', 'method')`)
- `extract_container_type_name` driven by LanguageDef data, not per-language match arms

## Non-goals

- Roslyn-level analysis (semantic model, overload resolution)
- `.csproj`/`.sln` project file parsing
- NuGet dependency resolution

## Design Decisions

### ChunkType variants

Add 3 new variants:

| Variant | Maps from | Callable? | Rationale |
|---------|-----------|-----------|-----------|
| `Property` | `property_declaration` | Yes | Has get/set bodies that call other code |
| `Delegate` | `delegate_declaration` | No | Type declaration (callable signature contract) |
| `Event` | `event_field_declaration`, `event_declaration` | No | Wiring declaration, rarely searched |

`record_declaration` maps to `Struct` (same as Java). Records are value types with syntactic sugar.

`constructor_declaration` maps to `Function` → inferred to `Method` via `method_containers`. Same pattern as Java.

`local_function_statement` maps to `Function`. Nested inside methods, common C# pattern.

`operator_declaration` and `indexer_declaration` map to `Function` → inferred to `Method`.

### Per-language common types (infrastructure)

**Problem:** `COMMON_TYPES` in `focused_read.rs` is a single Rust-centric `HashSet`. Adding C# types (`List`, `Task`, `Dictionary`) would pollute Rust filtering. This is already wrong for Python/JS/Go — just not painful yet.

**Solution:** Add `common_types: &'static [&'static str]` field to `LanguageDef`. Each language defines its own set. The focused_read filter checks the chunk's language and uses the appropriate set.

Existing languages get their current Rust types moved to `rust.rs`, and each other language gets its own appropriate set. Languages without meaningful common types (C, SQL, Markdown) use `&[]`.

C# common types: `string`, `int`, `bool`, `object`, `void`, `double`, `float`, `long`, `byte`, `char`, `decimal`, `Task`, `List`, `Dictionary`, `HashSet`, `IEnumerable`, `IList`, `IDictionary`, `ICollection`, `Action`, `Func`, `EventHandler`, `IDisposable`, `CancellationToken`, `ILogger`, `StringBuilder`, `Exception`, `Nullable`, `Span`, `Memory`, `ValueTask`.

### Dynamic callable SQL filter (infrastructure)

**Problem:** 3 SQL queries hardcode `WHERE chunk_type IN ('function', 'method')`. Adding Property as callable requires updating all 3, and every future callable type means the same hunt.

**Solution:** Add a `ChunkType::callable_sql_list()` class method that generates the SQL IN clause from `is_callable()`:

```rust
impl ChunkType {
    /// SQL IN clause for callable types, e.g. "'function','method','property'"
    pub fn callable_sql_list() -> String {
        [Function, Method, Property, Delegate, Event, Class, Struct, Enum,
         Trait, Interface, Constant, Section]
            .iter()
            .filter(|ct| ct.is_callable())
            .map(|ct| format!("'{}'", ct))
            .collect::<Vec<_>>()
            .join(",")
    }
}
```

Replace the 3 hardcoded queries with `format!("WHERE chunk_type IN ({})", ChunkType::callable_sql_list())`.

### Data-driven container type extraction (infrastructure)

**Problem:** `extract_container_type_name` in `parser/chunk.rs` has per-language match arms. Adding C# (and every future language) means another arm. Most languages follow the same pattern: if the container is an intermediate node (like `class_body` or `declaration_list`), walk up one level; then read the `name` field.

**Solution:** Add two new fields to `LanguageDef`:

```rust
/// Container node kinds that are intermediate (walk up to parent for name).
/// e.g., "class_body" in JS/TS/Java, "declaration_list" in C#
pub container_body_kinds: &'static [&'static str],

/// Container node kinds where the "name" field gives the type name directly.
/// e.g., "impl_item" in Rust (uses "type" field, not "name" — handled specially)
pub container_name_field: &'static str,  // typically "name"
```

Wait — Rust's `impl_item` uses the `type` field, not `name`. This is a special case. Better approach:

Add one field to LanguageDef:

```rust
/// Extract parent type name from a method container node.
/// Receives the container node and source text.
/// Default implementation: walk up from body nodes, read "name" field.
pub extract_parent_type: fn(container: tree_sitter::Node, source: &str,
                            method_containers: &[&str]) -> Option<String>,
```

Provide a default implementation that covers the common pattern (JS/TS/Java/Python/C# — walk up from body kinds, read `name`). Rust overrides with its `impl_item` → `type` field logic. Go overrides with receiver extraction. Languages without containers use a no-op.

Actually, simpler: keep the current `method_containers` field but add `container_body_kinds` to LanguageDef. The generic algorithm becomes:

1. If the matched container is in `container_body_kinds`, walk up one parent.
2. Read the `name` field from the resulting node.

Rust is special (impl_item uses `type` field not `name`). Add a `container_name_extraction` callback to LanguageDef for the override, with a sensible default.

Final design:

```rust
/// Node kinds that are intermediate body containers (walk up for name).
/// e.g., "class_body" (JS/TS/Java), "declaration_list" (C#/Rust)
pub container_body_kinds: &'static [&'static str],

/// Override for extracting parent type name from container.
/// None = use default (walk up from body, read "name" field).
pub extract_container_name: Option<fn(tree_sitter::Node, &str) -> Option<String>>,
```

Default behavior (when `extract_container_name` is `None`):
1. If container kind is in `container_body_kinds`, walk to parent.
2. Read `child_by_field_name("name")`.

Only Rust needs the override (for `impl_item` → `type` field). Go doesn't use containers (uses `method_node_kinds` + receiver). Everyone else uses the default.

### Tree-sitter queries

#### Chunk query

```scheme
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
```

Note: `operator_declaration` and `indexer_declaration` don't have `name` fields — they'll get `<anonymous>` names. This is correct; operators are searched by their containing type, not by name.

#### Call query

```scheme
;; Method calls: foo.Bar(), Bar()
(invocation_expression
  function: (member_access_expression name: (identifier) @callee))

(invocation_expression
  function: (identifier) @callee)

;; Constructor calls: new Foo()
(object_creation_expression type: (identifier) @callee)
(object_creation_expression type: (generic_name (identifier) @callee))
(object_creation_expression type: (qualified_name (identifier) @callee))

;; Base/this calls
(constructor_initializer (argument_list) @callee)
```

Note on `constructor_initializer`: captures `: base(...)` and `: this(...)` calls in constructors. The `@callee` capture on `argument_list` won't resolve well — may need refinement during implementation. Start without this, add if needed.

Revised — drop `constructor_initializer` for now:

```scheme
(invocation_expression
  function: (member_access_expression name: (identifier) @callee))
(invocation_expression
  function: (identifier) @callee)
(object_creation_expression type: (identifier) @callee)
(object_creation_expression type: (generic_name (identifier) @callee))
(object_creation_expression type: (qualified_name (identifier) @callee))
```

#### Type query

```scheme
;; Param — method parameters
(parameter type: (identifier) @param_type)
(parameter type: (generic_name (identifier) @param_type))
(parameter type: (qualified_name (identifier) @param_type))
(parameter type: (nullable_type (identifier) @param_type))
(parameter type: (array_type (identifier) @param_type))

;; Return — method/delegate return types
;; method_declaration uses "returns" field (not "type"!)
(method_declaration returns: (identifier) @return_type)
(method_declaration returns: (generic_name (identifier) @return_type))
(method_declaration returns: (qualified_name (identifier) @return_type))
(method_declaration returns: (nullable_type (identifier) @return_type))
(delegate_declaration type: (identifier) @return_type)
(delegate_declaration type: (generic_name (identifier) @return_type))
(local_function_statement type: (identifier) @return_type)
(local_function_statement type: (generic_name (identifier) @return_type))

;; Field — field declarations and property types
(field_declaration (variable_declaration type: (identifier) @field_type))
(field_declaration (variable_declaration type: (generic_name (identifier) @field_type)))
(property_declaration type: (identifier) @field_type)
(property_declaration type: (generic_name (identifier) @field_type))

;; Impl — base class, interface implementations
(base_list (identifier) @impl_type)
(base_list (generic_name (identifier) @impl_type))
(base_list (qualified_name (identifier) @impl_type))

;; Bound — generic constraints (where T : IFoo)
(type_parameter_constraint (type (identifier) @bound_type))
(type_parameter_constraint (type (generic_name (identifier) @bound_type)))

;; Alias — using alias directives
(using_directive name: (identifier) @alias_type)

;; Catch-all
(identifier) @type_ref
```

Note: The catch-all `(identifier) @type_ref` is intentionally broad. The type extraction logic in `parser/types.rs` deduplicates and prioritizes classified captures over catch-all.

### LanguageDef for C#

```rust
static DEFINITION: LanguageDef = LanguageDef {
    name: "csharp",
    grammar: Some(|| tree_sitter_c_sharp::LANGUAGE.into()),
    extensions: &["cs"],
    chunk_query: CHUNK_QUERY,
    call_query: Some(CALL_QUERY),
    signature_style: SignatureStyle::UntilBrace,
    doc_nodes: &["comment"],
    method_node_kinds: &[],
    method_containers: &["class_declaration", "struct_declaration",
                         "record_declaration", "interface_declaration",
                         "declaration_list"],
    container_body_kinds: &["declaration_list"],
    extract_container_name: None,  // default: walk up from body, read "name"
    stopwords: STOPWORDS,
    common_types: COMMON_TYPES,
    extract_return_nl: extract_return,
    test_file_suggestion: Some(|stem, parent| format!("{parent}/{stem}Tests.cs")),
    type_query: Some(TYPE_QUERY),
};
```

### Signature style

`UntilBrace` — same as Rust/Go/Java/C. Works for all C# declarations.

### Return type extraction

C# return types appear before the method name (like Java):
`public async Task<int> GetValue(...)` → extract `Task<int>`.

```rust
fn extract_return(signature: &str) -> Option<String> {
    // Skip modifiers, find the type before the method name
    // Similar to Java but must handle async, nullable (?)
    ...
}
```

### Stopwords

```rust
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
```

### Test file suggestion

Primary convention: `Foo.cs` → `FooTests.cs` in the same or parallel directory.
Secondary: `Foo.cs` → `Foo.Tests.cs`. Use the primary pattern.

## Files Changed

### New files

| File | Purpose |
|------|---------|
| `src/language/csharp.rs` | C# LanguageDef, queries, stopwords, common types |

### Infrastructure changes (benefits all future languages)

| File | Change |
|------|--------|
| `src/language/mod.rs` | Add `common_types`, `container_body_kinds`, `extract_container_name` to LanguageDef. Add Property, Delegate, Event to ChunkType. |
| `src/parser/chunk.rs` | Replace per-language `extract_container_type_name` match with data-driven algorithm using new LanguageDef fields. Add `"property"`, `"delegate"`, `"event"` to capture_types table. |
| `src/focused_read.rs` | Replace global `COMMON_TYPES` with per-language lookup via `LanguageDef.common_types`. |
| `src/store/calls.rs` | Replace 2 hardcoded `IN ('function', 'method')` with `ChunkType::callable_sql_list()`. |
| `src/store/chunks.rs` | Replace 1 hardcoded `IN ('function', 'method')` with `ChunkType::callable_sql_list()`. |

### C#-specific changes

| File | Change |
|------|--------|
| `src/language/mod.rs` | Add `CSharp` to `define_languages!` macro. |
| `src/nl.rs` | Add 3 match arms for Property, Delegate, Event in `type_word`. |
| `Cargo.toml` | Add `tree-sitter-c-sharp` dep, `lang-csharp` feature, update default + lang-all. |

### Existing language updates (backfill new LanguageDef fields)

All 9 existing language files get:
- `common_types: &[...]` — Rust gets current global set, others get language-appropriate sets or `&[]`
- `container_body_kinds: &[...]` — JS/TS/Java get `&["class_body"]`, Rust gets `&["declaration_list"]`, others `&[]`
- `extract_container_name: None` — all use default except Rust (override for impl_item)

### Documentation

| File | Change |
|------|--------|
| `README.md` | 9→10 languages, add C# to supported list |
| `CONTRIBUTING.md` | Update architecture section with new LanguageDef fields |
| `CHANGELOG.md` | New entry |
| `ROADMAP.md` | Check off C# |

### Tests

- Unit tests in `csharp.rs` for chunk extraction (class, method, property, delegate, event, record, local function, constructor)
- Unit tests for call extraction (method calls, new expressions)
- Unit tests for type extraction (params, returns, fields, base_list, constraints)
- Integration test: index a small C# file, search, verify results
- Regression tests: verify existing language behavior unchanged after infrastructure refactor

## Implementation Order

1. Infrastructure: LanguageDef fields, per-language common types, dynamic callable SQL, data-driven container extraction
2. ChunkType variants: Property, Delegate, Event + Display/FromStr/is_callable/nl.rs
3. C# language module: csharp.rs with all queries
4. Cargo.toml + define_languages! wiring
5. Backfill existing languages with new fields
6. Tests
7. Documentation

Infrastructure first so C# is just "fill in the LanguageDef" — and so is every language after it.

## Risk

- **Tree-sitter query correctness**: Node names come from grammar research, not runtime testing. Will need iteration against real C# code during implementation.
- **catch-all `@type_ref`**: The `(identifier) @type_ref` catch-all may be too broad for C# (identifiers used everywhere). May need tightening to `(type_identifier)` or qualified positions only. Test against real code.
- **`event_field_declaration` name extraction**: The name is nested inside `variable_declaration > variable_declarator > identifier`. The tree-sitter query syntax may need adjustment to reach it.
