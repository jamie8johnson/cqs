# Audit P2 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 17 P2 findings from the v1.0.13 audit — security, documentation, robustness, error handling, code quality, API design, platform behavior, data safety, algorithm, observability, and resource management.

**Architecture:** Grouped by independence. Tasks 1-3 (docs) can parallelize. Tasks 4-5 (parser robustness) can parallelize. Tasks 6-7 (error handling) both touch llm.rs so must be sequential with task 8 (observability). Tasks 9-11 are independent files. Tasks 12-17 are independent.

**Tech Stack:** Rust, thiserror, serde, regex, tree-sitter, SQLite

---

### Task 1: SECURITY.md + PRIVACY.md — document --llm-summaries network activity (DOC-11)

**Files:**
- Modify: `SECURITY.md:33,47`
- Modify: `PRIVACY.md:5`

- [ ] **Step 1: Update SECURITY.md**

At line 33, change:
```
cqs runs entirely locally. No telemetry, no external API calls during operation.
```
To:
```
cqs runs locally by default. No telemetry. The optional `--llm-summaries` flag sends function code to the Anthropic API (see below).
```

In the Network Requests table (around line 40), add a row:
```
| `--llm-summaries` | `api.anthropic.com` | Function bodies (up to 8000 chars), chunk type, language | Requires `ANTHROPIC_API_KEY`. Opt-in via `cqs index --llm-summaries` |
```

At line 47, change:
```
No other network requests are made. Search, indexing, and all other operations are offline.
```
To:
```
No other network requests are made. Without `--llm-summaries`, all operations are offline.
```

- [ ] **Step 2: Update PRIVACY.md**

At line 5, change:
```
cqs processes your code entirely on your machine. Nothing is transmitted externally.
```
To:
```
cqs processes your code locally by default. With `--llm-summaries`, function code is sent to Anthropic's API for one-sentence summary generation. See [Anthropic's privacy policy](https://www.anthropic.com/privacy). Without this flag, nothing is transmitted externally.
```

- [ ] **Step 3: Commit**

```
docs: document --llm-summaries network activity in SECURITY/PRIVACY (DOC-11)
```

---

### Task 2: README — fix --json vs --format json documentation (DOC-10)

**Files:**
- Modify: `README.md` (multiple locations)

- [ ] **Step 1: Audit which commands use --format json**

Commands with `format: OutputFormat`: `impact`, `review`, `ci`, `trace`. All others use `--json` boolean flag.

- [ ] **Step 2: Fix README examples**

Find all instances where `impact`, `review`, `ci`, or `trace` are shown with `--json` and change to `--format json`. Also fix the "all support `--json`" claim.

Fix `cqs callers --format mermaid` — callers has no `--format` flag. Change to show `cqs impact <name> --format mermaid` instead.

- [ ] **Step 3: Also fix CLAUDE.md if the same patterns appear**

Check CLAUDE.md for the same `--json` claims.

- [ ] **Step 4: Commit**

```
docs: fix --json vs --format json in README/CLAUDE.md (DOC-10)
```

---

### Task 3: HNSW deserialization — add pre-load file size validation (SEC-7)

**Files:**
- Modify: `src/hnsw/persist.rs` (around load function, lines 340-420)

**Context:** The load path already has:
- Checksum verification (line 345)
- ID map file size limit 500MB (lines 348-364)
- Graph file limit 500MB, data file limit 1GB (lines 367-391)
- Post-load id_map count validation (lines 419-427)

The remaining gap: bincode inside `hnsw_rs` can still OOM via crafted length prefixes within the size limits. The best additional defense is to validate file sizes against `id_map.len()` (already loaded by line 403).

- [ ] **Step 1: Add size validation after id_map load, before HnswIo::load_hnsw**

After the id_map is parsed (line 403) and before `HnswIo::load_hnsw` (line 415), add:

```rust
// Validate file sizes against id_map — catches crafted files that claim
// more vectors than the id_map supports (RUSTSEC-2025-0141 mitigation)
let expected_data_size = id_map.len() * EMBEDDING_DIM * std::mem::size_of::<f32>();
let data_path = dir.join(format!("{}.hnsw.data", basename));
if let Ok(meta) = std::fs::metadata(&data_path) {
    if meta.len() as usize > expected_data_size * 2 {
        return Err(HnswError::Validation(format!(
            "HNSW data file ({} bytes) too large for {} vectors",
            meta.len(), id_map.len()
        )));
    }
}
```

- [ ] **Step 2: Add test for oversized data file rejection**

- [ ] **Step 3: Commit**

```
fix(hnsw): validate data file size against id_map before deserialization (SEC-7)
```

---

### Task 4: Parser — fix to_uppercase byte offset mismatch (RB-8/RB-9)

**Files:**
- Modify: `src/parser/chunk.rs:125-147` (extract_signature)
- Modify: `src/parser/chunk.rs:200-219` (extract_name_fallback)

**Context:** Both functions use `content.to_uppercase().find(keyword)` and apply the byte offset to the original string. This fails on non-ASCII content because `to_uppercase()` can change byte lengths.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn extract_signature_until_as_with_unicode() {
    // ü is 2 bytes in UTF-8, Ü is also 2 bytes, so this case works
    // But ß→SS changes byte length. Test with content before AS keyword.
    let content = "CREATE VIEW straße AS SELECT 1";
    let sig = extract_signature(content, &SignatureStyle::UntilAs);
    assert_eq!(sig, "CREATE VIEW straße");
}

#[test]
fn extract_name_fallback_with_unicode() {
    let content = "CREATE FUNCTION straße_func() RETURNS void";
    let name = extract_name_fallback(content);
    assert_eq!(name, Some("straße_func".to_string()));
}
```

- [ ] **Step 2: Fix extract_signature for UntilAs**

Replace the `to_uppercase().find()` pattern with case-insensitive ASCII search. Since SQL keywords are ASCII, use `make_ascii_uppercase()` on a byte copy:

```rust
SignatureStyle::UntilAs => {
    // Case-insensitive search for SQL AS keyword.
    // Uses byte-level ASCII uppercasing to preserve byte offsets
    // (to_uppercase() can change byte lengths for non-ASCII chars like ß→SS).
    let bytes = content.as_bytes();
    let mut pos = None;
    for i in 0..bytes.len().saturating_sub(3) {
        if (bytes[i] == b' ' || bytes[i] == b'\n')
            && bytes[i + 1].to_ascii_uppercase() == b'A'
            && bytes[i + 2].to_ascii_uppercase() == b'S'
            && (i + 3 >= bytes.len() || bytes[i + 3] == b' ' || bytes[i + 3] == b'\n')
        {
            pos = Some(i);
            break;
        }
    }
    pos.unwrap_or(content.len())
}
```

- [ ] **Step 3: Fix extract_name_fallback**

Same approach — use `to_ascii_uppercase()` byte-level comparison instead of `to_uppercase()`:

```rust
fn extract_name_fallback(content: &str) -> Option<String> {
    let upper_bytes: Vec<u8> = content.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let upper = String::from_utf8_lossy(&upper_bytes);
    for keyword in &["PROCEDURE", "FUNCTION", "VIEW", "TRIGGER"] {
        if let Some(pos) = upper.find(keyword) {
            // pos is valid for original content since ASCII uppercase preserves byte positions
            let after_keyword = pos + keyword.len();
            if after_keyword >= content.len() {
                continue;
            }
            // ... rest of logic using content[after_keyword..]
```

- [ ] **Step 4: Run tests, commit**

```
fix(parser): use ASCII case folding to preserve byte offsets (RB-8/RB-9)
```

---

### Task 5: Parser — bounds-check byte_offset_to_point (RB-10)

**Files:**
- Modify: `src/parser/injection.rs:146-151`
- Modify: `src/parser/aspx.rs:95-100`

- [ ] **Step 1: Fix injection.rs**

```rust
fn byte_offset_to_point(source: &str, byte: usize) -> tree_sitter::Point {
    let byte = byte.min(source.len());
    let byte = source.floor_char_boundary(byte);
    let before = &source[..byte];
```

- [ ] **Step 2: Fix aspx.rs**

```rust
fn byte_to_point(source: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(source.len());
    let byte = source.floor_char_boundary(byte);
    let before = &source[..byte];
```

- [ ] **Step 3: Commit**

```
fix(parser): bounds-check byte offsets in injection/aspx parsers (RB-10)
```

---

### Task 6: Define ProjectError + LlmError typed error enums (EH-13/EH-14)

**Files:**
- Modify: `src/project.rs` (define ProjectError, convert 5 pub fns)
- Modify: `src/llm.rs` (define LlmError, convert 2 pub fns + Client methods)

**Context:** Both files use `anyhow::Result` in library code. Need `thiserror` enums.

- [ ] **Step 1: Define ProjectError in project.rs**

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Config directory not found")]
    ConfigDirNotFound,
    #[error("Duplicate project name: {0}")]
    DuplicateName(String),
    #[error("Project not found: {0}")]
    NotFound(String),
    #[error("File too large: {path} ({size} bytes, max {max} bytes)")]
    FileTooLarge { path: String, size: u64, max: u64 },
    #[error("Store error: {0}")]
    Store(#[from] crate::store::StoreError),
}
```

Convert all pub functions from `anyhow::Result<T>` to `Result<T, ProjectError>`. Replace `bail!` with `return Err(ProjectError::...)`. Replace `.context()` with `.map_err()`.

- [ ] **Step 2: Define LlmError in llm.rs**

```rust
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("API key missing: {0}")]
    ApiKeyMissing(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("Batch failed with status: {0}")]
    BatchFailed(String),
    #[error("Invalid batch ID format: {0}")]
    InvalidBatchId(String),
    #[error("JSON parse error: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("Store error: {0}")]
    Store(#[from] crate::store::StoreError),
}
```

Convert `llm_summary_pass`, `Client::new`, and internal methods.

- [ ] **Step 3: Update CLI callers to convert errors**

CLI callers (in `src/cli/commands/`) that call these functions need `.map_err(|e| anyhow::anyhow!(e))` or `anyhow::Error::from(e)` at the boundary.

- [ ] **Step 4: Commit**

```
fix(errors): define ProjectError + LlmError, replace anyhow in library code (EH-13/EH-14)
```

---

### Task 7: Define ConfigError for config write functions (EH-15)

**Files:**
- Modify: `src/config.rs:285,372`

- [ ] **Step 1: Define ConfigError**

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("Duplicate reference: {0}")]
    DuplicateReference(String),
    #[error("Invalid config format: {0}")]
    InvalidFormat(String),
}
```

- [ ] **Step 2: Convert add_reference_to_config and remove_reference_from_config**

- [ ] **Step 3: Commit**

```
fix(config): define ConfigError for write functions (EH-15)
```

---

### Task 8: Replace eprint/eprintln in llm.rs with tracing (OB-8)

**Files:**
- Modify: `src/llm.rs` (~15 eprint/eprintln sites)

- [ ] **Step 1: Replace all eprintln! with tracing::info!**

Convert progress messages to structured tracing:
- `eprintln!("Scanning {} chunks...", n)` → `tracing::info!(chunks = n, "Scanning for LLM summaries")`
- `eprintln!("  {} cached, {} doc, {} skipped, {} API", ...)` → `tracing::info!(cached, doc_extracted, skipped, api_needed = batch_items.len(), "Summary scan complete")`
- `eprintln!("Resuming pending batch {}", id)` → `tracing::info!(batch_id = %id, "Resuming pending batch")`
- etc.

- [ ] **Step 2: Handle progress dots specially**

The `eprint!(".")` in `wait_for_batch` can stay as-is (gated by `!quiet`) since tracing has no native progress indicator. Add a comment explaining the exception.

- [ ] **Step 3: Remove the `quiet` parameter where possible**

If all user-facing output is now tracing, the `quiet` flag can control tracing subscriber level instead. However, this is a larger refactor — for now, keep `quiet` but move all messages to tracing.

- [ ] **Step 4: Commit**

```
fix(llm): replace eprint/eprintln with tracing in library code (OB-8)
```

---

### Task 9: Add serialize_path_normalized to 13 PathBuf fields (AD-16)

**Files:**
- Modify: `src/impact/types.rs` (CallerDetail, TestInfo, TransitiveCaller, TypeImpacted, ChangedFunction, DiffTestInfo — 6 fields)
- Modify: `src/drift.rs` (DriftEntry — 1 field)
- Modify: `src/review.rs` (ReviewedFunction — 1 field)
- Modify: `src/ci.rs` (DeadInDiff — 1 field)
- Modify: `src/scout.rs` (FileGroup — 1 field)
- Modify: `src/where_to_add.rs` (FileSuggestion — 1 field)
- Modify: `src/related.rs` (RelatedFunction — 1 field)
- Modify: `src/diff_parse.rs` (DiffHunk — 1 field)

- [ ] **Step 1: Add serde annotation to each pub file: PathBuf field**

For each field, add:
```rust
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
```

- [ ] **Step 2: Build and test**

- [ ] **Step 3: Commit**

```
fix(api): add serialize_path_normalized to 13 PathBuf fields (AD-16)
```

---

### Task 10: Change ORT provider #[cfg(unix)] to #[cfg(target_os = "linux")] (PB-8/PB-9/PB-15)

**Files:**
- Modify: `src/embedder.rs` (5 functions around lines 679-830)

- [ ] **Step 1: Change cfg gates**

Change `#[cfg(unix)]` to `#[cfg(target_os = "linux")]` on:
- `fn ensure_ort_provider_libs()` (line 679)
- `fn ort_runtime_search_dir()` (line 723)
- `fn find_ort_provider_dir()` (line 742)
- `fn find_ld_library_dir()` (line 768)
- `fn symlink_providers()` (line 779)
- `fn register_provider_cleanup()` (line 805)

Change the corresponding `#[cfg(not(unix))]` no-op to `#[cfg(not(target_os = "linux"))]`.

- [ ] **Step 2: Build and test**

- [ ] **Step 3: Commit**

```
fix(embedder): gate ORT provider functions on target_os = "linux" (PB-8/PB-9/PB-15)
```

---

### Task 11: Normalize note paths with normalize_path (PB-13/DS-12)

**Files:**
- Modify: `src/store/notes.rs:105,195,249`

- [ ] **Step 1: Replace to_string_lossy with normalize_path**

At all three locations, replace:
```rust
let source_str = source_file.to_string_lossy().into_owned();
```
With:
```rust
let source_str = crate::normalize_path(source_file);
```

And at line 249:
```rust
.bind(source_file.to_string_lossy().into_owned())
```
With:
```rust
.bind(crate::normalize_path(source_file))
```

- [ ] **Step 2: Build and test**

- [ ] **Step 3: Commit**

```
fix(notes): use normalize_path for consistent path storage (PB-13/DS-12)
```

---

### Task 12: LLM batch resume — re-scan after fetching pending results (DS-7)

**Files:**
- Modify: `src/llm.rs` (empty batch_items path, around line 492)

- [ ] **Step 1: After resume_or_fetch_batch on empty path, log and continue**

The current code returns immediately after fetching the pending batch results. Instead, log the count and let the function return — the next `cqs index --llm-summaries` run will pick up any new uncached chunks. The fix is to document this behavior rather than add re-scanning logic (which would require re-paginating all chunks).

Add a tracing::info after the resume:
```rust
tracing::info!(count, "Fetched pending batch results — new chunks will be processed on next run");
```

- [ ] **Step 2: Commit**

```
fix(llm): document pending batch resume behavior for newly-added chunks (DS-7)
```

---

### Task 13: LLM batch — handle "created" status explicitly (DS-10)

**Files:**
- Modify: `src/llm.rs` (check_batch_status match around line 515)

- [ ] **Step 1: Add "created" arm to the status match**

```rust
Ok(status) if status == "created" => {
    // Batch was just submitted but hasn't started processing yet — wait for it
    if !quiet {
        tracing::info!(batch_id = %pending, "Pending batch still queued, waiting");
    }
    pending
}
```

- [ ] **Step 2: Log when overwriting a pending batch**

In the catch-all `_` arm, add:
```rust
tracing::warn!(old_batch = %pending, "Pending batch status unknown, submitting fresh — old batch results lost");
```

- [ ] **Step 3: Commit**

```
fix(llm): handle "created" batch status, warn on duplicate submission (DS-10)
```

---

### Task 14: CAGRA — document distance conversion assumption (AC-8)

**Files:**
- Modify: `src/cagra.rs:328-330`

**Context:** The formula `score = 1.0 - dist / 2.0` is mathematically correct for L2 distance on unit-norm vectors. The scout confirmed the math checks out. The divergence for non-zero sentiment is real but bounded (sentiment values are in [-1, 1], so the offset is at most 1.0). The practical impact is small since sentiment=0 is the common case.

- [ ] **Step 1: Add documentation comment**

```rust
// CAGRA uses squared L2 distance. For unit-norm vectors: d = 2 - 2*cos_sim, so cos_sim = 1 - d/2.
// Note: cqs embeddings are 769-dim (768 + sentiment). The sentiment dimension breaks the unit-norm
// assumption, introducing a small scoring bias proportional to sentiment magnitude. This diverges
// from HNSW (DistCosine, normalizes internally) and brute-force (dot product). For sentiment=0
// (the common case), all three backends agree. See audit finding AC-8.
let score = 1.0 - dist / 2.0;
```

- [ ] **Step 2: Commit**

```
docs(cagra): document distance conversion assumption for non-unit-norm vectors (AC-8)
```

---

### Task 15: Reduce PRAGMA quick_check to integrity_check(1) (RM-15)

**Files:**
- Modify: `src/store/mod.rs:289-298` (Store::open)
- Modify: `src/store/mod.rs:367-376` (Store::open_readonly)

- [ ] **Step 1: Replace PRAGMA quick_check with integrity_check(1)**

In both `open()` and `open_readonly()`, change:
```rust
sqlx::query_as("PRAGMA quick_check")
```
To:
```rust
sqlx::query_as("PRAGMA integrity_check(1)")
```

`integrity_check(1)` stops after the first error found, making it much faster while still catching corruption. The `(1)` parameter limits it to checking the first page and returning at most 1 error.

- [ ] **Step 2: Commit**

```
fix(store): use integrity_check(1) instead of quick_check for faster startup (RM-15)
```

---

### Task 16: Unify test detection logic (CQ-8)

**Files:**
- Modify: `src/lib.rs:210-228` (is_test_chunk — add missing patterns)
- Modify: `src/search.rs:482-511` (chunk_importance — delegate to is_test_chunk)

- [ ] **Step 1: Expand is_test_chunk with patterns from chunk_importance**

Add the missing patterns to `is_test_chunk`:
```rust
pub fn is_test_chunk(name: &str, file: &str) -> bool {
    let name_match = name.starts_with("test_")
        || name.starts_with("Test")
        || name.starts_with("spec_")       // from chunk_importance
        || name.ends_with("_test")
        || name.ends_with("_spec")         // from chunk_importance
        || name.contains("_test_")
        || name.contains(".test");
    if name_match { return true; }
    let filename = file.rsplit('/').next().unwrap_or(file);
    filename.contains("_test.") || filename.contains(".test.")
        || filename.contains(".spec.") || filename.contains("_spec.")  // from chunk_importance
        || filename.starts_with("test_")                                // from chunk_importance
        || file.contains("/tests/") || file.starts_with("tests/")
        || file.ends_with("_test.go") || file.ends_with("_test.py")
}
```

- [ ] **Step 2: Simplify chunk_importance to use is_test_chunk**

```rust
fn chunk_importance(name: &str, file_path: &str) -> f32 {
    if crate::is_test_chunk(name, file_path) {
        return IMPORTANCE_TEST;
    }
    // ... rest of importance logic (private functions, etc.)
```

- [ ] **Step 3: Update tests if any**

- [ ] **Step 4: Commit**

```
fix(search): unify test detection into is_test_chunk (CQ-8)
```

---

### Task 17: Extract shared post-scoring pipeline in search.rs (CQ-7)

**Files:**
- Modify: `src/search.rs` (extract ~130 lines into helper)

**Context:** `search_filtered` (~line 900) and `search_filtered_with_index` (~line 1099) duplicate: RRF fusion, content fetch, parent dedup, parent boost, truncation. The only difference is how scored candidates arrive.

- [ ] **Step 1: Define the shared helper signature**

```rust
async fn finalize_results(
    &self,
    scored: Vec<(String, f32)>,
    filter: &SearchFilter,
    use_rrf: bool,
    limit: usize,
) -> Result<Vec<SearchResult>, StoreError>
```

- [ ] **Step 2: Extract the common code from search_filtered into the helper**

Move: RRF fusion → fetch_chunks_by_ids_async → parent dedup → apply_parent_boost → truncate.

- [ ] **Step 3: Call the helper from both search_filtered and search_filtered_with_index**

In `search_filtered_with_index`, convert the `(CandidateRow, f32)` to `(String, f32)` before calling.

- [ ] **Step 4: Run search tests**

```
cargo test --features gpu-index -p cqs --lib -- search 2>&1
```

- [ ] **Step 5: Commit**

```
refactor(search): extract shared post-scoring pipeline (CQ-7)
```

---

### Task 18: Final verification

- [ ] **Step 1: Full build + test**
- [ ] **Step 2: Update audit-triage.md P2 status**
- [ ] **Step 3: Commit triage update**
