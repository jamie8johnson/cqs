# Language Definition Code Generation Design

## Problem

18,927 lines in `src/language/`, 52 language definitions with ~20 identical boilerplate fields each. Adding a language requires copying a Rust file, filling in fields, and registering in `mod.rs`. The data is genuinely data but encoded as Rust code.

## Solution

TOML data files + `build.rs` code generator. Each language becomes a `.toml` file. Build script generates `LanguageDef` statics and the `Language` enum at compile time.

## Architecture

### Data format: `languages/rust.toml`

```toml
name = "rust"
extensions = ["rs"]
grammar = "tree_sitter_rust::LANGUAGE"
signature_style = "UntilOpenBrace"
doc_nodes = ["line_comment", "block_comment"]
doc_format = "triple_slash"
doc_convention = "/// "
method_node_kinds = ["function_item"]
method_containers = ["impl_item"]
stopwords = ["fn", "let", "mut", "pub", "use", "mod", "impl", "struct", "enum", "trait"]
test_markers = ["#[test]", "#[cfg(test)]"]
test_path_patterns = ["tests/", "_test.rs"]
entry_point_names = ["main"]
trait_method_names = []

[field_style]
type = "NameFirst"
separators = ":="
strip_prefixes = "pub pub(crate) mut let const static"

[extract_return]
# Language-specific return type extraction is a Rust function reference — stays in code
function = "extract_return_rust"
```

### Tree-sitter queries: `languages/rust.chunks.scm`, `languages/rust.calls.scm`, `languages/rust.types.scm`

Queries stay as separate `.scm` files — they're tree-sitter patterns, not configuration. The build script embeds them as `include_str!()`.

### Code generator: `build.rs`

1. Read all `languages/*.toml` files
2. Validate required fields, check for typos against known field names
3. Generate `src/language/generated.rs`:
   - `Language` enum with one variant per TOML file
   - `LanguageDef` statics for each language
   - `LanguageRegistry` population
   - `from_extension()` lookup table
4. Language-specific Rust functions (extract_return, post_process_chunk, detect_language) stay as handwritten code in `src/language/custom.rs` — referenced by name in the TOML

### What stays as Rust code

- `LanguageDef` struct definition (the schema)
- `InjectionRule` definitions (complex, reference Rust functions)
- `post_process_chunk` functions (per-language logic)
- `extract_return_nl` functions (per-language logic)
- `detect_language` functions for injection (e.g., HTML script tag language detection)

### What moves to TOML

- All 20+ data fields (name, extensions, stopwords, doc_nodes, etc.)
- Chunk/call/type queries (as .scm files)
- Field style configuration
- Test markers and path patterns
- Signature style selection

## Why TOML + build.rs over proc-macro

- Data is data — TOML is validatable, diffable, readable by non-Rust tools
- Compile-time errors on malformed TOML, no runtime surprises
- Adding a language = copy TOML + write queries, zero Rust knowledge needed
- `build.rs` is simpler to debug than proc-macros
- External tools could generate language definitions from tree-sitter grammar metadata

## Effort estimate

Multi-day project:
1. Design TOML schema, write build.rs generator (~1 day)
2. Convert first 5 languages as validation (~0.5 day)
3. Convert remaining 47 languages (~1 day, mostly mechanical)
4. Delete old Rust language files, update imports (~0.5 day)
5. Test, verify all 52 languages still parse correctly (~0.5 day)

## Dependencies

None. Can be done independently of all other roadmap items.
