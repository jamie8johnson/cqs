# SQ-8: `--improve-docs` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate doc comments for undocumented/poorly-documented functions via Claude Batches API and write them back to source files.

**Architecture:** Schema v16 (composite PK on llm_summaries), Phase 2 batch pass in llm.rs, per-language DocWriter for source rewriting. Extends the existing `--llm-summaries` pipeline.

**Tech Stack:** Rust, SQLite, Anthropic Batches API, tree-sitter

**Spec:** `docs/superpowers/specs/2026-03-19-improve-docs-design.md`

**IMPORTANT:** All new LLM code and CLI wiring must be behind `#[cfg(feature = "llm-summaries")]` feature gate, matching the existing `--llm-summaries` code.

---

## File Map

| File | Responsibility |
|------|---------------|
| `src/store/migrations.rs` | v15→v16 migration (recreate llm_summaries with composite PK) |
| `src/store/chunks.rs` | Update all llm_summaries methods: add `purpose` param, change tuple from 3 to 4 elements |
| `src/schema.sql` | Update llm_summaries table definition for fresh indexes |
| `src/store/helpers.rs` | Bump CURRENT_SCHEMA_VERSION to 16 |
| `src/llm.rs` | Phase 2 doc generation: doc prompt (with language-specific appendices), `submit_batch` max_tokens override, remove 500-char cap in `fetch_batch_results`, `doc_comment_pass()`, pending_doc_batch metadata |
| `src/doc_writer/mod.rs` | Public API: `DocEdit`, `DocCommentResult`, `rewrite_file()` |
| `src/doc_writer/formats.rs` | Per-language doc comment format definitions + `format_doc_comment()` |
| `src/doc_writer/rewriter.rs` | Source file rewriter: re-parse, find insertion points (decorator-aware), detect existing docs, bottom-up edit, atomic write |
| `src/lib.rs` | Add `pub mod doc_writer;` |
| `src/cli/mod.rs` | Add `--improve-docs`, `--max-docs` flags (reuse existing `--dry-run` from index command) |
| `src/cli/commands/index.rs` | Wire flags to `doc_comment_pass` and `rewrite_file` |

---

### Task 1: Schema v16 — composite PK on llm_summaries

**Files:** `src/store/helpers.rs`, `src/store/migrations.rs`, `src/schema.sql`

- [ ] **Step 1: Write migration test**

Follow `test_migrate_v14_to_v15` pattern. Create v15 schema, insert two summaries, migrate, verify: (a) can insert same content_hash with different purpose, (b) existing rows have purpose='summary', (c) schema_version=16.

- [ ] **Step 2: Implement migration**

Recreate table with composite PK `(content_hash, purpose)`. Copy existing rows with `purpose = 'summary'`.

- [ ] **Step 3: Update schema.sql** — match migrated schema for fresh indexes

- [ ] **Step 4: Bump `CURRENT_SCHEMA_VERSION` to 16**

- [ ] **Step 5: Build, test, commit**

```
feat(store): schema v16 — composite PK (content_hash, purpose) on llm_summaries
```

---

### Task 2: Update store methods for `purpose` parameter

**Files:** `src/store/chunks.rs`, `src/llm.rs` (caller updates)

- [ ] **Step 1: Change tuple type**

`upsert_summaries_batch` currently takes `&[(String, String, String)]` = (hash, summary, model). Change to `&[(String, String, String, String)]` = (hash, summary, model, purpose). Update BATCH_SIZE: `132 * 5 params = 660 < 999` (was 166 * 4).

- [ ] **Step 2: Update `upsert_summaries_batch` SQL**

Add `purpose` to INSERT column list and push_values.

- [ ] **Step 3: Update `get_summaries_by_hashes` — add `purpose: &str` param**

Add `AND purpose = ?` to WHERE clause. Bind parameter.

- [ ] **Step 4: Update `get_all_summaries` — add `purpose: &str` param**

Same WHERE clause addition.

- [ ] **Step 5: Update `prune_orphan_summaries`**

No signature change — prune orphans regardless of purpose.

- [ ] **Step 6: Update callers in llm.rs**

Every call to these methods in `llm_summary_pass` gets `"summary"` as the purpose value. The tuple construction for `api_summaries` and `to_store` adds `"summary".to_string()` as 4th element.

- [ ] **Step 7: Add coexistence test**

Test that two entries with same content_hash but different purposes can coexist: insert (hash, "summary text", model, "summary"), insert (hash, "doc comment text", model, "doc-comment"), verify both retrievable independently.

- [ ] **Step 8: Update existing summary tests**

All existing tests pass `"summary"` for purpose. Verify all pass.

- [ ] **Step 9: Build, test, commit**

```
refactor(store): add purpose parameter to llm_summaries methods
```

---

### Task 3: DocWriter — format registry (independent of Tasks 1-2)

**Files:** `src/doc_writer/mod.rs` (new), `src/doc_writer/formats.rs` (new), `src/lib.rs`

- [ ] **Step 1: Create module structure**

`src/doc_writer/mod.rs` with `pub mod formats;` and public types:

```rust
/// Result from Phase 2 LLM doc generation
pub struct DocCommentResult {
    pub file: PathBuf,
    pub function_name: String,
    pub content_hash: String,
    pub generated_doc: String,
    pub language: Language,
    pub line_start: usize,
    pub had_existing_doc: bool,
}
```

Add `pub mod doc_writer;` to `src/lib.rs`.

- [ ] **Step 2: Implement format registry in formats.rs**

```rust
pub struct DocFormat {
    pub prefix: &'static str,
    pub line_prefix: &'static str,
    pub suffix: &'static str,
    pub position: InsertionPosition,
}

pub enum InsertionPosition { BeforeFunction, InsideBody }

pub fn doc_format_for(language: Language) -> DocFormat
```

Cover: Rust (///), Python ("""), Go (// FuncName), Java/C/C++/Scala/Kotlin/Swift/PHP/TS/JS (/** */), Ruby (#), Elixir (@doc """), Lua (---), Haskell (-- |), OCaml ((** *)), F# (///). Default: `//` for others.

- [ ] **Step 3: Implement format_doc_comment**

```rust
pub fn format_doc_comment(text: &str, language: Language, indent: &str, func_name: &str) -> String
```

Wraps raw LLM text in language-specific format with correct indentation per line.

- [ ] **Step 4: Tests**

Test all major formats: Rust, Python, Go, Java, TypeScript. Verify indentation, prefix/suffix, newlines. Test default format for unsupported language.

- [ ] **Step 5: Build, test, commit**

```
feat(doc-writer): per-language doc comment format registry
```

---

### Task 4: LLM Phase 2 — doc comment batch generation

**Files:** `src/llm.rs`, `src/store/mod.rs`

**Depends on:** Tasks 1-2 (schema + store methods), Task 3 (DocCommentResult type)

- [ ] **Step 1: Add `build_doc_prompt`**

Separate from `build_prompt`. Language-specific appendices:
- Rust: "Use `# Arguments`, `# Returns`, `# Errors`, `# Panics` sections as appropriate."
- Python: "Format as a Google-style docstring (Args/Returns/Raises sections)."
- Go: "Start with the function name per Go conventions."
- Default: no appendix.

- [ ] **Step 2: Add max_tokens parameter to submit_batch**

```rust
fn submit_batch(&self, items: &[...], max_tokens: u32) -> Result<String, LlmError>
```

Update existing caller in summary pass to pass `self.llm_config.max_tokens`.

- [ ] **Step 3: Remove 500-char ceiling from fetch_batch_results**

Change `if !trimmed.is_empty() && trimmed.len() < 500` to `if !trimmed.is_empty()`.

- [ ] **Step 4: Add `pending_doc_batch` metadata**

`set_pending_doc_batch_id` / `get_pending_doc_batch_id` in `src/store/mod.rs` — same pattern as existing `pending_llm_batch`.

- [ ] **Step 5: Implement quality heuristic**

```rust
fn needs_doc_comment(chunk: &ChunkSummary) -> bool
```

- `is_callable()`
- `window_idx` is None or 0
- No doc OR thin doc (< 30 chars AND no signal words)
- Signal words (case-insensitive): SAFETY, UNSAFE, INVARIANT, TODO, FIXME, HACK, NOTE, XXX, BUG, DEPRECATED, SECURITY, WARN

Test: verify signal words are preserved, thin docs below threshold trigger, adequate docs skip.

- [ ] **Step 6: Implement doc_comment_pass**

```rust
pub fn doc_comment_pass(
    store: &Store,
    config: &Config,
    max_docs: usize,
) -> Result<Vec<DocCommentResult>, LlmError>
```

Collects candidates, sorts by priority, applies cap, checks cache, submits batch (max_tokens=800), caches results (purpose="doc-comment"), returns results.

On re-run: functions whose docs were already written back will have `doc.len() >= 30` and skip the heuristic. Functions where write-back failed will get a new content_hash (file unchanged), hit cache, and return the cached doc.

- [ ] **Step 7: Tests**

Test quality heuristic with signal words. Test doc_comment_pass with mock (or small integration test if feasible).

- [ ] **Step 8: Build, test, commit**

```
feat(llm): Phase 2 doc comment generation with batch API
```

---

### Task 5: DocWriter — source file rewriter

**Files:** `src/doc_writer/rewriter.rs`, `src/doc_writer/mod.rs`

**Depends on:** Task 3 (formats), Task 4 (DocCommentResult)

- [ ] **Step 1: Implement find_insertion_point**

```rust
pub fn find_insertion_point(
    line_start: usize,
    file_lines: &[&str],
    language: Language,
) -> usize
```

For `BeforeFunction`: scan upward from `line_start - 1`. Skip blank lines and decorator/attribute lines (`@`, `#[`, `#![`, `[`, leading whitespace + `@`). Return the line above the first decorator.

For `InsideBody` (Python): return `line_start + 1` (line after `def`). Detect indentation from body.

Test: Rust function with `#[derive]` + `#[cfg]` → insert above `#[derive]`. Python `def` → insert at body line. Plain function → insert at `line_start`.

- [ ] **Step 2: Implement detect_existing_doc_range**

```rust
pub fn detect_existing_doc_range(
    line_start: usize,
    file_lines: &[&str],
    language: Language,
) -> Option<Range<usize>>
```

For `BeforeFunction`: scan upward from insertion point. If consecutive comment lines matching the language's doc prefix are found, return their range.

For `InsideBody` (Python): check if line after `def` starts with `"""` or `'''`. Find the closing delimiter. Return the range.

- [ ] **Step 3: Implement rewrite_file**

```rust
pub fn rewrite_file(
    path: &Path,
    edits: &[DocCommentResult],
    parser: &Parser,
) -> Result<usize, DocWriterError>  // returns count of functions modified
```

1. Read file content
2. Re-parse with `parser.parse_source()` to get current chunks
3. For each edit, match to a chunk (by name + approximate content)
4. Compute insertion point, detect existing doc range
5. Format doc comment with correct indent + language format
6. Collect all line-level edits
7. Apply bottom-up (highest line first)
8. Atomic write (temp + rename, with cross-device fallback)

- [ ] **Step 4: Tests**

Multi-function Rust file: undocumented function, thin-doc function, well-documented function. Verify:
- Undocumented gets new doc at correct position
- Thin doc gets replaced (old lines removed, new inserted)
- Well-documented is untouched
- Indentation correct for nested method in impl block

Python file: verify docstring inserted inside body after `def`.

File with decorators: verify doc goes above `#[derive(...)]`, not between decorator and function.

Test `--max-docs` cap: 5 candidates, cap=2, verify only top 2 by priority get docs.

- [ ] **Step 5: Build, test, commit**

```
feat(doc-writer): source file rewriter with bottom-up insertion
```

---

### Task 6: CLI wiring

**Files:** `src/cli/mod.rs`, `src/cli/commands/index.rs`

**Depends on:** Tasks 4, 5

- [ ] **Step 1: Add CLI flags**

Add to the index command's argument struct (behind `#[cfg(feature = "llm-summaries")]`):
- `--improve-docs` (bool)
- `--max-docs` (Option<usize>)

Reuse the existing `--dry-run` flag (already on the index command — check if it's passed through to the llm code path).

- [ ] **Step 2: Validation**

If `improve_docs` without `llm_summaries`: print error "—improve-docs requires --llm-summaries" and exit.

- [ ] **Step 3: Wire orchestration**

After `llm_summary_pass` in the index command:

```rust
#[cfg(feature = "llm-summaries")]
if improve_docs {
    let results = cqs::llm::doc_comment_pass(&store, &config, max_docs.unwrap_or(0))?;
    if dry_run {
        // Print preview from cache
        for r in &results {
            let action = if r.had_existing_doc { "thin → replace" } else { "no doc → generate" };
            println!("{}:{} ({}) — {}", r.file.display(), r.function_name, r.language, action);
            let formatted = cqs::doc_writer::formats::format_doc_comment(&r.generated_doc, r.language, "  ", &r.function_name);
            for line in formatted.lines() { println!("  + {}", line); }
            println!();
        }
        println!("{} functions would receive doc comments", results.len());
    } else {
        let parser = cqs::parser::Parser::new();
        let by_file: HashMap<PathBuf, Vec<_>> = /* group results by file */;
        let mut total = 0;
        for (path, edits) in &by_file {
            match cqs::doc_writer::rewrite_file(path, edits, &parser) {
                Ok(n) => total += n,
                Err(e) => tracing::warn!(file = %path.display(), error = %e, "Doc write-back failed"),
            }
        }
        println!("Wrote doc comments to {} functions across {} files", total, by_file.len());
    }
}
```

- [ ] **Step 4: Verify `cqs index --help` shows new flags**

- [ ] **Step 5: Build, test, commit**

```
feat(cli): add --improve-docs and --max-docs flags to index command
```

---

### Task 7: Integration + end-to-end tests

**Depends on:** Task 6

- [ ] **Step 1: Dry-run test on cqs repo**

```bash
cargo run --features gpu-index -- index --llm-summaries --improve-docs --max-docs 5 --dry-run
```

Verify: shows 5 candidates, correct language formats, reasonable output.

- [ ] **Step 2: Write-back test on temp file**

Create a temp Rust file with undocumented functions. Run `--improve-docs` (requires ANTHROPIC_API_KEY). Verify file modified, `cargo check` passes on modified file, `git diff` shows only doc comment additions.

- [ ] **Step 3: Re-run test (idempotency)**

Run `--improve-docs` again on the same file. Verify: functions that received docs are now skipped (doc.len() >= 30), 0 new docs generated.

- [ ] **Step 4: Commit**

```
test: improve-docs end-to-end and idempotency tests
```

---

## Task Parallelism

| Phase | Tasks | Notes |
|-------|-------|-------|
| 1 (parallel) | 1, 3 | Schema migration + DocWriter formats (independent files) |
| 2 (sequential) | 2 | Store methods (depends on Task 1 schema) |
| 3 (sequential) | 4 | LLM batch (depends on Task 2 store + Task 3 types) |
| 4 (sequential) | 5 | Rewriter (depends on Task 3 formats + Task 4 types) |
| 5 (sequential) | 6 | CLI wiring (depends on Tasks 4 + 5) |
| 6 (sequential) | 7 | Integration tests (depends on Task 6) |
