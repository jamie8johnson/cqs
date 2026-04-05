# Language Macro V2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate 52 per-language `.rs` files into a single `languages.rs` with a `language!{}` macro and external `.scm` query files, cutting ~5,000 lines.

**Architecture:** A declarative `language!{}` macro with defaults for all optional fields. Tree-sitter queries move to `src/language/queries/*.scm` loaded via `include_str!()`. Custom functions (post_process, extract_return, detect_language) stay as named Rust functions in `languages.rs`. Tests move to `tests/language_test.rs`.

**Tech Stack:** Rust declarative macros, `include_str!()`, tree-sitter queries

**Spec:** `docs/superpowers/specs/2026-04-05-language-macro-v2-design.md`

---

## File Structure

```
src/language/
  mod.rs              — MODIFY: Language enum stays, remove 52 module imports, add `mod languages;`
  languages.rs        — CREATE: 52 language!{} invocations + custom functions
  queries/            — CREATE: ~100 .scm files
    rust.chunks.scm, rust.calls.scm, rust.types.scm, ...
tests/
  language_test.rs    — CREATE: consolidated language tests (moved from per-file #[cfg(test)])
src/language/*.rs     — DELETE: 52 individual language files (after migration complete)
```

## Phase overview

1. **Tasks 1-2:** Write the `language!{}` macro + query directory
2. **Task 3:** Convert first 5 languages as validation
3. **Task 4:** Convert remaining 47 languages mechanically
4. **Task 5:** Move tests to `tests/language_test.rs`
5. **Task 6:** Delete old files, clean up mod.rs
6. **Task 7:** Final verification

---

### Task 1: Write the `language!{}` macro

**Files:**
- Create: `src/language/languages.rs`

This macro expands a compact declaration into a `static LanguageDef` and a `pub fn definition_<name>()`. All optional fields have defaults.

- [ ] **Step 1: Create `src/language/languages.rs` with the macro definition**

```rust
//! Consolidated language definitions.
//!
//! Each language is a `language!{}` invocation that expands to a `LanguageDef` static.
//! Tree-sitter queries live in `queries/*.scm` and are embedded via `include_str!()`.
//! Custom functions (post_process, extract_return, etc.) are defined as named functions
//! alongside the language they serve.

use super::{
    ChunkType, FieldStyle, InjectionRule, LanguageDef, PostProcessChunkFn, SignatureStyle,
    StructuralMatcherFn,
};

/// Declare a language definition with sensible defaults for optional fields.
///
/// Required fields: `name`, `grammar`, `extensions`, `chunks`, `signature`.
/// All other fields default to `None`, `&[]`, or appropriate zero values.
///
/// Expands to:
/// - A `static LANG_<NAME>: LanguageDef` with all fields populated
/// - A `pub fn definition_<snake>() -> &'static LanguageDef` accessor
macro_rules! language {
    (
        name: $name:literal,
        definition_fn: $def_fn:ident,
        grammar: $grammar:expr,
        extensions: [$($ext:literal),* $(,)?],
        chunks: $chunks:expr,
        signature: $sig:expr
        $(, calls: $calls:expr)?
        $(, types: $types:expr)?
        $(, doc_nodes: [$($dn:literal),* $(,)?])?
        $(, method_node_kinds: [$($mnk:literal),* $(,)?])?
        $(, method_containers: [$($mc:literal),* $(,)?])?
        $(, stopwords: [$($sw:literal),* $(,)?])?
        $(, common_types: [$($ct:literal),* $(,)?])?
        $(, container_body_kinds: [$($cbk:literal),* $(,)?])?
        $(, test_markers: [$($tm:literal),* $(,)?])?
        $(, test_path_patterns: [$($tpp:literal),* $(,)?])?
        $(, entry_points: [$($ep:literal),* $(,)?])?
        $(, trait_method_names: [$($tmn:literal),* $(,)?])?
        $(, skip_line_prefixes: [$($slp:literal),* $(,)?])?
        $(, doc_format: $df:literal)?
        $(, doc_convention: $dc:literal)?
        $(, field_style: $fs:expr)?
        $(, extract_return: $er:expr)?
        $(, post_process: $pp:expr)?
        $(, extract_container_name: $ecn:expr)?
        $(, extract_qualified_method: $eqm:expr)?
        $(, test_file_suggestion: $tfs:expr)?
        $(, test_name_suggestion: $tns:expr)?
        $(, structural_matchers: $sm:expr)?
        $(, injections: $inj:expr)?
        $(,)?
    ) => {
        pub fn $def_fn() -> &'static LanguageDef {
            static DEFINITION: LanguageDef = LanguageDef {
                name: $name,
                grammar: Some(|| $grammar.into()),
                extensions: &[$($ext),*],
                chunk_query: $chunks,
                signature_style: $sig,
                call_query: language!(@opt_str $($calls)?),
                type_query: language!(@opt_str $($types)?),
                doc_nodes: language!(@arr $(&[$($dn),*])?),
                method_node_kinds: language!(@arr $(&[$($mnk),*])?),
                method_containers: language!(@arr $(&[$($mc),*])?),
                stopwords: language!(@arr $(&[$($sw),*])?),
                common_types: language!(@arr $(&[$($ct),*])?),
                container_body_kinds: language!(@arr $(&[$($cbk),*])?),
                test_markers: language!(@arr $(&[$($tm),*])?),
                test_path_patterns: language!(@arr $(&[$($tpp),*])?),
                entry_point_names: language!(@arr $(&[$($ep),*])?),
                trait_method_names: language!(@arr $(&[$($tmn),*])?),
                skip_line_prefixes: language!(@arr $(&[$($slp),*])?),
                doc_format: language!(@str_default $($df)? ; "default"),
                doc_convention: language!(@str_default $($dc)? ; ""),
                field_style: language!(@field_style $($fs)?),
                extract_return_nl: language!(@fn_or_noop $($er)?),
                post_process_chunk: language!(@opt_fn $($pp)?),
                extract_container_name: language!(@opt_fn $($ecn)?),
                extract_qualified_method: language!(@opt_fn $($eqm)?),
                test_file_suggestion: language!(@opt_fn $($tfs)?),
                test_name_suggestion: language!(@opt_fn $($tns)?),
                structural_matchers: language!(@opt_fn $($sm)?),
                injections: language!(@inj $($inj)?),
            };
            &DEFINITION
        }
    };

    // --- Helper arms ---

    // Optional &str → Option<&'static str>
    (@opt_str $val:expr) => { Some($val) };
    (@opt_str) => { None };

    // Optional array → &'static [&'static str]
    (@arr $val:expr) => { $val };
    (@arr) => { &[] };

    // String with default
    (@str_default $val:literal ; $default:literal) => { $val };
    (@str_default ; $default:literal) => { $default };

    // FieldStyle with default
    (@field_style $val:expr) => { $val };
    (@field_style) => { FieldStyle::None };

    // extract_return_nl: fn or |_| None
    (@fn_or_noop $val:expr) => { $val };
    (@fn_or_noop) => { |_| None };

    // Optional function pointer
    (@opt_fn $val:expr) => { Some($val) };
    (@opt_fn) => { None };

    // Injections
    (@inj $val:expr) => { $val };
    (@inj) => { &[] };
}

// Special variant for grammar-less languages (Markdown, ASPX)
macro_rules! language_no_grammar {
    (
        name: $name:literal,
        definition_fn: $def_fn:ident,
        extensions: [$($ext:literal),* $(,)?],
        chunks: $chunks:expr,
        signature: $sig:expr
        $(, $field:ident : $val:tt)*
        $(,)?
    ) => {
        // Grammar-less languages use the same LanguageDef but with grammar: None
        // These are custom parsers (Markdown, ASPX) — they set grammar: None
        // and their chunk_query is unused but must be non-empty for the struct.
        pub fn $def_fn() -> &'static LanguageDef {
            static DEFINITION: LanguageDef = LanguageDef {
                name: $name,
                grammar: None,
                extensions: &[$($ext),*],
                chunk_query: $chunks,
                signature_style: $sig,
                call_query: None,
                type_query: None,
                doc_nodes: &[],
                method_node_kinds: &[],
                method_containers: &[],
                stopwords: &[],
                common_types: &[],
                container_body_kinds: &[],
                test_markers: &[],
                test_path_patterns: &[],
                entry_point_names: &[],
                trait_method_names: &[],
                skip_line_prefixes: &[],
                doc_format: "default",
                doc_convention: "",
                field_style: FieldStyle::None,
                extract_return_nl: |_| None,
                post_process_chunk: None,
                extract_container_name: None,
                extract_qualified_method: None,
                test_file_suggestion: None,
                test_name_suggestion: None,
                structural_matchers: None,
                injections: &[],
            };
            &DEFINITION
        }
    };
}
```

- [ ] **Step 2: Verify the file compiles**

Add `mod languages;` to `src/language/mod.rs` (after existing module imports) temporarily. Run:

```bash
cargo check --features gpu-index 2>&1 | tail -5
```

Expected: compiles (unused macro warnings OK at this stage).

- [ ] **Step 3: Commit**

```bash
git add src/language/languages.rs src/language/mod.rs
git commit -m "feat: add language!{} macro skeleton for consolidated language defs"
```

---

### Task 2: Create queries directory and extract first 5 languages

**Files:**
- Create: `src/language/queries/` directory
- Create: `.scm` files for bash, rust, python, go, json
- Modify: `src/language/languages.rs` — add 5 language definitions

Convert 5 representative languages covering different complexity levels:
- **bash** — simplest (no custom functions, no types)
- **json** — no grammar calls, custom post_process
- **go** — medium (extract_return, post_process, type_query, injection-free)
- **python** — medium (extract_return, post_process, structural_matchers)
- **rust** — complex (all custom functions, type_query)

- [ ] **Step 1: Create queries directory and extract bash queries**

```bash
mkdir -p src/language/queries
```

Create `src/language/queries/bash.chunks.scm` with the content from `CHUNK_QUERY` in `src/language/bash.rs`.
Create `src/language/queries/bash.calls.scm` with the content from `CALL_QUERY` in `src/language/bash.rs`.

- [ ] **Step 2: Add bash language definition to `languages.rs`**

Copy the `LanguageDef` fields from `src/language/bash.rs` into a `language!{}` invocation. Reference queries via `include_str!()`. Only include fields that differ from defaults.

- [ ] **Step 3: Extract queries and add definitions for json, go, python, rust**

Same process for each:
1. Extract query strings to `.scm` files
2. Copy custom functions (post_process, extract_return, etc.) to `languages.rs`
3. Write `language!{}` invocation referencing the functions and queries

For rust, also extract `rust.types.scm`.

- [ ] **Step 4: Wire the 5 new definitions into the registry**

In `src/language/mod.rs`, within the `LanguageRegistry::new()` function inside `define_languages!`, change the 5 registration lines from:

```rust
#[cfg(feature = "lang-bash")]
reg.register(bash::definition());
```

to:

```rust
#[cfg(feature = "lang-bash")]
reg.register(languages::definition_bash());
```

Keep the old module imports for now (other 47 languages still use them).

- [ ] **Step 5: Run the tests for the 5 converted languages**

```bash
cargo test --features gpu-index -- bash python rust go json 2>&1 | grep "test result"
```

Expected: all existing tests for these 5 languages still pass. The old `.rs` files are still present but their `definition()` functions are no longer called by the registry.

- [ ] **Step 6: Commit**

```bash
git add src/language/queries/ src/language/languages.rs src/language/mod.rs
git commit -m "feat: convert 5 pilot languages to language!{} macro (bash, json, go, python, rust)"
```

---

### Task 3: Convert remaining 47 languages

**Files:**
- Create: `.scm` files for all remaining languages
- Modify: `src/language/languages.rs` — add 47 language definitions
- Modify: `src/language/mod.rs` — update all registry calls

This is mechanical: for each language file, extract queries to `.scm`, copy custom functions, write `language!{}` invocation, update registry.

- [ ] **Step 1: Convert languages in batches of ~10**

Work through the remaining languages alphabetically. For each:
1. Read the existing `.rs` file
2. Extract `CHUNK_QUERY` → `queries/<lang>.chunks.scm`
3. Extract `CALL_QUERY` (if present) → `queries/<lang>.calls.scm`
4. Extract `TYPE_QUERY` (if present) → `queries/<lang>.types.scm`
5. Copy any custom functions (`post_process_*`, `extract_return`, `detect_*`, etc.)
6. Write `language!{}` invocation
7. Update registry call in `mod.rs`

**Languages with injections** (html, svelte, vue, razor, php, aspx): the `InjectionRule` arrays and `detect_language` functions must be copied verbatim. Pass them via the `injections:` field.

**Languages without grammars** (markdown, aspx): use `language_no_grammar!{}`.

**Shared functions:** Many languages share the same `extract_return` pattern (look for `->` or `:` return type). Consolidate into shared functions:
- `extract_return_arrow(sig)` — Rust, Python, Kotlin, Swift, etc.
- `extract_return_colon(sig)` — TypeScript, Go, Scala, etc.
- `extract_return_c_style(sig)` — C, C++, Java, C#, etc.

Check the existing code — many already use a local `extract_return` with the same body.

- [ ] **Step 2: Build after each batch**

```bash
cargo check --features gpu-index
```

Fix any compilation errors before moving to the next batch.

- [ ] **Step 3: Run full test suite after all 47 are converted**

```bash
cargo test --features gpu-index 2>&1 | grep "test result"
```

Expected: all tests pass. At this point, both old `.rs` files and new `languages.rs` definitions exist, but only `languages.rs` is wired into the registry.

- [ ] **Step 4: Commit**

```bash
git add src/language/queries/ src/language/languages.rs src/language/mod.rs
git commit -m "feat: convert all 52 languages to language!{} macro"
```

---

### Task 4: Move tests to `tests/language_test.rs`

**Files:**
- Create: `tests/language_test.rs`
- Modify: 52 old `.rs` files — will be deleted in Task 5, but tests move first

- [ ] **Step 1: Create `tests/language_test.rs` with shared test helper**

```rust
//! Language parsing tests — consolidated from per-language #[cfg(test)] blocks.

use cqs::parser::{ChunkType, Parser};
use std::io::Write;

fn write_temp_file(content: &str, ext: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(&format!(".{}", ext))
        .tempfile()
        .unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}
```

- [ ] **Step 2: Move tests in batches**

For each old language `.rs` file, copy the test functions (without the `mod tests` wrapper and `use` imports) into `tests/language_test.rs`. Prefix test function names with the language to avoid collisions (e.g., `parse_bash_function` → `test_bash_parse_function`). Most tests already have language-prefixed names.

The tests use `Parser::new()` and `parser.parse_file()` — these are public API, so they work from integration tests without change.

Tests that use `super::*` (accessing private module items like `detect_script_language`) need adjustment — either test through the public API or make the function `pub(crate)`.

- [ ] **Step 3: Run the new test file**

```bash
cargo test --features gpu-index --test language_test 2>&1 | grep "test result"
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add tests/language_test.rs
git commit -m "test: move language tests to consolidated tests/language_test.rs"
```

---

### Task 5: Delete old language files and clean up mod.rs

**Files:**
- Delete: 52 `src/language/*.rs` files (bash.rs, rust.rs, python.rs, ...)
- Modify: `src/language/mod.rs` — remove old module imports from `define_languages!`

- [ ] **Step 1: Remove old module references from `define_languages!`**

Change the `define_languages!` macro invocation to no longer import per-language modules. The feature-gated `mod $module;` lines in the macro expansion need to be removed or changed to only import `languages`.

Update the `define_languages!` macro to only generate the enum, Display, FromStr, and all_variants — not module imports or registry population. Move registry population to a separate block that calls `languages::definition_*()`.

- [ ] **Step 2: Delete old language files**

```bash
# Delete all per-language .rs files (not mod.rs, not languages.rs)
ls src/language/*.rs | grep -v mod.rs | grep -v languages.rs | xargs rm
```

- [ ] **Step 3: Build and test**

```bash
cargo test --features gpu-index 2>&1 | grep "test result"
```

Expected: all tests pass. The old per-file tests are gone (now in `tests/language_test.rs`), and unit tests in `mod.rs` still work.

- [ ] **Step 4: Commit**

```bash
git add -A src/language/
git commit -m "refactor: delete 52 per-language .rs files, consolidate into languages.rs"
```

---

### Task 6: Final verification and cleanup

**Files:**
- Modify: `CONTRIBUTING.md` — update Architecture Overview
- Modify: `CHANGELOG.md` — add entry

- [ ] **Step 1: Verify line counts**

```bash
wc -l src/language/*.rs src/language/queries/*.scm tests/language_test.rs | tail -5
```

Expected: significant reduction from the original 18,927 lines.

- [ ] **Step 2: Run full test suite**

```bash
cargo test --features gpu-index 2>&1 | grep "^test result:" | awk '{p+=$4; f+=$6} END {printf "%d pass, %d fail\n", p, f}'
```

Expected: same test count as before (2351), 0 failures.

- [ ] **Step 3: Run clippy**

```bash
cargo clippy --features gpu-index -- -D warnings 2>&1 | tail -5
```

Expected: no warnings.

- [ ] **Step 4: Update CONTRIBUTING.md Architecture Overview**

Update the `src/language/` section to reflect the new structure:
```
  language/     - Tree-sitter language support
    mod.rs      - Language enum, LanguageRegistry, LanguageDef, ChunkType
    languages.rs - All 52 language definitions (language!{} macro) + custom functions
    queries/    - Tree-sitter queries (.scm files, one per query per language)
```

- [ ] **Step 5: Commit**

```bash
git add CONTRIBUTING.md
git commit -m "docs: update architecture overview for language macro v2"
```

---

## Notes for implementer

- **Feature gates:** Each `language!{}` definition is gated by its feature flag. The `define_languages!` macro generates the enum variants unconditionally, but registry registration is feature-gated.
- **Custom parsers stay separate:** `aspx.rs` (in `src/parser/aspx.rs`), `l5x.rs` (in `src/parser/l5x.rs`), and `markdown/` (in `src/parser/markdown/`) are NOT language definition files — they're custom parsers. Don't touch them.
- **`language/aspx.rs`** and **`language/markdown.rs`** ARE language definition files that happen to have `grammar: None` because they use custom parsers. These DO get converted to `language_no_grammar!{}`.
- **The `define_languages!` macro in `mod.rs` stays.** It generates the `Language` enum, Display/FromStr impls, and all_variants(). Only the module-import and registry-population parts change.
- **Injection rules reference Rust functions** (`detect_script_language`, etc.). These functions move to `languages.rs` alongside the language that uses them.
- **CUDA and GLSL** reuse C++ and C grammars respectively. They have their own `LanguageDef` but reference another language's grammar. This is fine — `grammar: tree_sitter_cpp::LANGUAGE` works in the macro.
- **Run `cargo fmt` before every commit.** Pre-commit hook enforces this.
- **Use `--features gpu-index`** for all cargo commands. This is the project's default.
