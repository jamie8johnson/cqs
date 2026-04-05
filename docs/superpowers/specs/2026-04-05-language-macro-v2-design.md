# Language Macro V2 Design

## Problem

18,927 lines in `src/language/`, 52 files with identical structure. Adding a language means copying a Rust file, filling in ~30 fields (half of which are `None`/`&[]`), and registering in mod.rs. Tree-sitter queries are embedded as Rust string literals with no syntax highlighting or editor support.

## Solution

Consolidate 52 per-language `.rs` files into a single `src/language/languages.rs` using a `language!{}` macro with sensible defaults. Extract tree-sitter queries to standalone `.scm` files loaded via `include_str!()`.

## Architecture

### Query files: `src/language/queries/<lang>.chunks.scm`

```scheme
;; Rust chunk query
(function_item name: (identifier) @name) @function
(struct_item name: (type_identifier) @name) @struct
```

One file per query type per language. Loaded at compile time via `include_str!()`. Benefits: syntax highlighting, diffable, no Rust string escaping, language-server support in editors.

File naming: `<lang>.chunks.scm`, `<lang>.calls.scm`, `<lang>.types.scm`. If a language has no call/type query, no file needed.

### Macro: `language!{}`

```rust
language! {
    name: "bash",
    grammar: tree_sitter_bash::LANGUAGE,
    extensions: ["sh", "bash"],
    chunks: include_str!("queries/bash.chunks.scm"),
    calls: include_str!("queries/bash.calls.scm"),
    signature: UntilBrace,
    doc_nodes: ["comment"],
    stopwords: ["if", "then", "else", "fi", "for", "do", "done"],
    entry_points: ["main"],
    test_path_patterns: ["%/tests/%", "%\\_test.sh", "%.bats"],
}
```

**Defaults** (omitted fields get these):
- `calls` → `None`
- `types` → `None`
- `doc_nodes` → `&[]`
- `stopwords` → `&[]`
- `common_types` → `&[]`
- `method_node_kinds` → `&[]`
- `method_containers` → `&[]`
- `container_body_kinds` → `&[]`
- `test_markers` → `&[]`
- `test_path_patterns` → `&[]`
- `entry_points` → `&[]`
- `trait_method_names` → `&[]`
- `injections` → `&[]`
- `skip_line_prefixes` → `&[]`
- `doc_format` → `"default"`
- `doc_convention` → `""`
- `field_style` → `FieldStyle::None`
- `signature` → `UntilBrace`
- `extract_return` → `|_| None`
- `post_process` → `None`
- `extract_container_name` → `None`
- `extract_qualified_method` → `None`
- `test_file_suggestion` → `None`
- `test_name_suggestion` → `None`
- `structural_matchers` → `None`

The macro expands to a `static LANG_<NAME>: LanguageDef = LanguageDef { ... };` and a registration call.

### Custom logic

Custom functions stay as named functions in `languages.rs`, referenced by the macro:

```rust
fn extract_return_rust(sig: &str) -> Option<String> {
    sig.find("->").map(|i| {
        let ret = sig[i + 2..].trim();
        format!("Returns {}", crate::nl::tokenize_identifier(ret).join(" "))
    })
}

fn post_process_rust(chunk: &mut Chunk, node: &tree_sitter::Node, source: &[u8]) {
    // ... existing logic
}

language! {
    name: "rust",
    grammar: tree_sitter_rust::LANGUAGE,
    extensions: ["rs"],
    chunks: include_str!("queries/rust.chunks.scm"),
    calls: include_str!("queries/rust.calls.scm"),
    types: include_str!("queries/rust.types.scm"),
    signature: UntilOpenBrace,
    extract_return: extract_return_rust,
    post_process: post_process_rust,
    // ...
}
```

Languages sharing the same return extractor pattern (e.g., `->` for Rust/Python/TypeScript/Go) can share the same function.

### File layout after migration

```
src/language/
  mod.rs          — Language enum (define_languages! stays), LanguageDef struct, registry
  languages.rs    — All 52 language!{} invocations + custom functions
  queries/        — .scm files (one per query per language)
    rust.chunks.scm
    rust.calls.scm
    rust.types.scm
    python.chunks.scm
    python.calls.scm
    ...
```

### Tests

Move from 52 per-file `#[cfg(test)]` blocks (~9,122 lines) to `tests/language_test.rs`. The tests are integration tests anyway — they use `Parser::new()` which goes through the public API.

### define_languages! macro (mod.rs)

Stays as-is. It generates the `Language` enum, `Display`/`FromStr`, `all_variants()`, and feature-gated module imports. The only change: instead of importing 52 modules, it imports one (`languages`).

### Registration

`LanguageRegistry::new()` currently has 52 feature-gated blocks:
```rust
#[cfg(feature = "lang-rust")]
registry.register(Language::Rust, rust::definition());
```

After: the `language!{}` macro generates a `pub fn definition_<name>() -> &'static LanguageDef` for each language, and `LanguageRegistry::new()` calls them. The feature gates move into the macro expansion.

## What this does NOT change

- `LanguageDef` struct definition — unchanged
- `define_languages!` macro — unchanged  
- Parser, NL generation, search — no changes
- `InjectionRule` struct — unchanged
- Custom parsers (`aspx.rs`, `l5x.rs`, `markdown/`) — these are NOT language defs, they're custom parsers that bypass tree-sitter. They stay as separate files.

## Migration strategy

1. Write the `language!{}` macro and `queries/` directory
2. Convert 5 languages as validation (bash, rust, python, go, json — covering simple, complex, injection, no-grammar cases)
3. Convert remaining 47 mechanically
4. Move tests to `tests/language_test.rs`
5. Delete 52 old `.rs` files
6. Verify all 52 languages parse correctly via existing test suite

## Expected line counts

| Before | After | Delta |
|--------|-------|-------|
| 52 language `.rs` files: 16,929 | `languages.rs`: ~2,100 | -14,800 |
| — | `queries/` (~100 files): ~1,100 | +1,100 |
| — | `tests/language_test.rs`: ~9,100 | (moved, not new) |
| mod.rs: 1,998 | mod.rs: ~1,600 | -400 |
| **Total: 18,927** | **~13,900** | **~-5,000** |

Net reduction of ~5,000 lines, with much better organization. The remaining code is denser — less boilerplate per language.
