# SQ-8: `--improve-docs` Design Spec

## Purpose

Generate and write back doc comments for undocumented or poorly-documented functions using the Claude Batches API. Extends the existing `--llm-summaries` pipeline with a second batch pass and a per-language source rewriter.

## Command

```bash
cqs index --llm-summaries --improve-docs           # summaries + doc generation + write back
cqs index --llm-summaries --improve-docs --dry-run  # preview diff, no file writes, no API calls if cache warm
cqs index --llm-summaries --improve-docs --max-docs 50  # cap doc generation at N functions
cqs index --improve-docs                            # error: requires --llm-summaries
```

## Pipeline

Extends `llm_summary_pass` with a second phase:

**Phase 1 (existing):** Summary batch — one-sentence summaries, MAX_TOKENS=100, `purpose = "summary"`.

**Phase 2 (new, only if `--improve-docs`):**
1. Collect functions that need doc comments (quality heuristic, see below)
2. Sort by priority: no doc first, then thin doc, by function size descending
3. Apply `--max-docs` cap if set
4. Submit as separate Batches API request — `purpose = "doc-comment"`, max_tokens=800 (passed as override to `submit_batch`, not from `LlmConfig.max_tokens` which stays at 100 for summaries)
5. Cache results in `llm_summaries` with `purpose = "doc-comment"`
6. Write back to source files (unless `--dry-run`)

Phase 2 runs after Phase 1 completes. Separate batch IDs: `pending_llm_batch` (summaries) and `pending_doc_batch` (doc comments).

## Schema v16

**Migration v15→v16:** Recreate `llm_summaries` table with composite PK.

```sql
-- 1. Create new table
CREATE TABLE llm_summaries_v2 (
    content_hash TEXT NOT NULL,
    purpose TEXT NOT NULL DEFAULT 'summary',
    summary TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (content_hash, purpose)
);

-- 2. Copy existing data
INSERT INTO llm_summaries_v2 (content_hash, purpose, summary, model, created_at)
SELECT content_hash, 'summary', summary, model, created_at FROM llm_summaries;

-- 3. Drop old, rename new
DROP TABLE llm_summaries;
ALTER TABLE llm_summaries_v2 RENAME TO llm_summaries;

-- 4. Update schema_version
UPDATE metadata SET value = '16' WHERE key = 'schema_version';
```

**Also update `schema.sql`** for fresh indexes.

**Store method updates:** All methods in `store/chunks.rs` that touch `llm_summaries` need a `purpose` parameter:
- `get_summaries_by_hashes(&self, hashes, purpose)` — filter by purpose
- `upsert_summaries_batch(&self, summaries, purpose)` — include purpose in INSERT
- `get_all_summaries(&self, purpose)` — filter by purpose
- `prune_orphan_summaries(&self)` — prune both purposes

## Doc Quality Heuristic

Generate a doc comment for a function if ALL of:
- `is_callable()` — Function, Method, or Macro (not containers like Struct/Enum/Trait)
- `window_idx` is None or 0 (skip non-primary windowed chunks, same as summary pass)
- No doc, OR doc is "thin"
- **Thin** = `doc.len() < 30` AND doc does NOT contain (case-insensitive): `SAFETY`, `UNSAFE`, `INVARIANT`, `TODO`, `FIXME`, `HACK`, `NOTE`, `XXX`, `BUG`, `DEPRECATED`, `SECURITY`, `WARN`
- Not already in doc-comment cache (`content_hash` + `purpose = "doc-comment"`)

**Priority for `--max-docs`:** Two-pass approach — first collect all candidates, sort (no doc > thin doc, then by content length descending), take top N.

## Write-Back: Per-Language Source Rewriter

The write-back pass does NOT use stored line numbers. It re-parses each affected file at write-back time to get current function positions and existing doc locations.

### Process per file:

1. Read file content
2. Parse with `Parser::parse_source()` — get chunks with line ranges
3. For each function that has a generated doc in the cache:
   - Find the chunk in the parse results (match by name + content_hash)
   - Detect existing doc comment (walk tree-sitter siblings, same as `extract_doc_comment`)
   - Determine insertion point (language-specific, see below)
   - Detect indentation from the function's first line
   - Format doc comment with correct indent + language syntax
4. Apply all edits **bottom-up** (highest line number first, so earlier edits don't shift later line numbers)
5. Write modified content to temp file, rename atomically

### Insertion points by language:

| Language | Format | Position |
|----------|--------|----------|
| Rust, F# | `/// {line}` | Before function (after attributes) |
| Python | `"""{text}"""` | Inside body (first statement after `def` line) |
| Go | `// {FuncName} {line}` | Before function |
| Java, C, C++, Scala, Kotlin, Swift, PHP | `/** ... */` | Before function |
| TypeScript, JavaScript | `/** ... */` | Before function |
| Ruby | `# {line}` | Before function |
| Elixir | `@doc """\n{text}\n"""` | Before function |
| Lua | `--- {line}` | Before function |
| Haskell | `-- | {line}` | Before function |
| OCaml | `(** {text} *)` | Before function |
| **Default (all others)** | `// {line}` | Before function |

Languages not listed use `// {line}` default. This covers all 51 languages — some will get suboptimal but syntactically valid comments.

### "Before function" means:

After any existing doc comment lines (which are being replaced), but **before** the function definition line. Specifically:
- If replacing existing doc: remove old doc lines, insert new doc at the same starting line
- If no existing doc: insert before `chunk.line_start`, but after any decorators/attributes

To handle decorators/attributes correctly: scan upward from `chunk.line_start - 1`. If the line is a decorator (`@`, `#[`, `[Attribute]`) or blank, keep scanning. Insert the doc comment above the first decorator, not between decorator and function.

### Python special case:

```python
def foo(x: int) -> str:
    """Generated doc comment goes here.

    Args:
        x: The input value.

    Returns:
        The formatted string.
    """
    return str(x)
```

Insert after the `def` line, as the first statement in the body. Indentation = function body indent (detected from the line after `def`, or function indent + 4 spaces if empty body).

### Overlapping chunks:

Only generate docs for leaf `is_callable()` chunks. If a method inside an `impl` block needs docs, the method gets docs, not the impl block. The quality heuristic's `is_callable()` filter already ensures this (Struct, Enum, Trait, Module are not callable).

## Prompt Template

```
Generate a doc comment for this {language} {chunk_type} named `{name}`.

Requirements:
- Describe what the function does (purpose, not implementation)
- Document parameters and their meaning
- Document the return value
- Note any error conditions or panics
- Use {language} doc comment conventions
- Be concise but complete — aim for 3-8 lines
- Do NOT include the function signature, only the documentation text

{language} function:
```{language}
{content}
```
```

For Python, append: "Format as a Google-style docstring (Args/Returns/Raises sections)."
For Rust, append: "Use `# Arguments`, `# Returns`, `# Errors`, `# Panics` sections as appropriate."

## Caching

- Table: `llm_summaries` with composite PK `(content_hash, purpose)`
- Summary: `purpose = "summary"`, `model = <LlmConfig.model>`
- Doc comment: `purpose = "doc-comment"`, `model = <LlmConfig.model>`
- Cached doc comments are raw text (no language-specific formatting — formatting applied at write-back)
- Re-running with `--improve-docs` skips functions already in doc-comment cache
- Content changes (new `content_hash`) → regenerate both summary and doc

## Pending Batch Recovery

Two metadata keys:
- `pending_llm_batch` — existing, for summary batch
- `pending_doc_batch` — new, for doc-comment batch

Each follows the same resume-or-fetch pattern as the existing `llm_summary_pass`.

## Crash Safety

### During API phase:
Same as existing — pending batch IDs in metadata, resume on restart.

### During write-back phase:
- Docs are cached before write-back begins (API phase complete)
- Files are written atomically (temp + rename) — no partial file writes
- If crash between files: some files written, some not
- On re-run: re-parse each file, compare against cache. If cache has doc-comment for a function but the file doesn't have it, write it. Content_hash may differ (because the file was modified by a previous write), but we match by function name + approximate content similarity, not hash.
- Simpler alternative: just re-run `--improve-docs`. Functions that already have docs (from previous successful write) pass the quality heuristic (doc.len() >= 30) and get skipped. Functions that weren't written get regenerated (new content_hash if file changed) or found in cache (if file unchanged).

## Dry-Run

**Cache warm (docs already generated):**
```
src/config.rs:parse_config (line 45) — no doc → generate
  + /// Parses a TOML configuration file at the given path.
  + ///
  + /// # Arguments
  + /// * `path` - Path to the TOML config file
  + ///
  + /// # Returns
  + /// A Config struct with the parsed settings.
  + ///
  + /// # Errors
  + /// Returns an error if the file cannot be read or contains invalid TOML.

3 functions would receive doc comments
```

**Cache cold (nothing generated yet):**
```
3 functions need doc comments (2 no-doc, 1 thin-doc)
Estimated cost: ~$0.02 (2,400 output tokens at Haiku rates)
Run without --dry-run to generate and write back.
```

No API calls in dry-run mode. Cache cold = show estimate only.

## Error Handling

| Condition | Action |
|-----------|--------|
| `--improve-docs` without `--llm-summaries` | Error with message |
| File read-only or permission denied | Warn, skip file, continue |
| Parse failure on write-back re-parse | Warn, skip file, continue |
| Function not found in re-parse (renamed/deleted) | Warn, skip, continue |
| LLM generates invalid doc (empty, too long, contains code) | Skip, warn |
| Atomic rename fails (cross-device) | Fallback copy pattern (same as note.rs) |

## Implementation Notes

**`submit_batch` needs max_tokens parameter:** Currently hardcoded from `LlmConfig.max_tokens`. Phase 2 needs to pass max_tokens=800 as an override. Add `max_tokens: u32` parameter to `submit_batch` (or `Client::submit_batch`).

**`fetch_batch_results` has 500-char ceiling:** The existing `fetch_batch_results` (llm.rs ~line 358) discards responses >= 500 chars. Phase 2 doc comments will routinely exceed this. Either parameterize the limit or remove it (let the caller filter).

## Not In Scope (V1)

- Generating docs for non-callable chunks (structs, enums, traits, modules)
- Updating existing adequate docs (only no-doc and thin-doc)
- Language-specific linting of generated docs (e.g., rustdoc warnings)
- Interactive approval per function (git diff serves this purpose)
- Writing docs to a separate file instead of source
