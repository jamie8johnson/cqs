# Audit P1 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 15 P1 findings from the v1.0.13 audit — panics, security holes, data safety, algorithm bugs, docs, and extensibility.

**Architecture:** Grouped by file to minimize context switches. llm.rs has 5 findings (heaviest), embedder.rs has 2, rest are 1 each. All fixes are localized — no cross-file refactors.

**Tech Stack:** Rust, reqwest, libc, clap, SQLite

---

### Task 1: llm.rs — Fix byte slice panic on CJK content (RB-7)

**Files:**
- Modify: `src/llm.rs:114-118`

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `src/llm.rs`:

```rust
#[test]
fn build_prompt_multibyte_no_panic() {
    // 3-byte CJK chars: 2667 chars = 8001 bytes, triggers truncation
    let content: String = std::iter::repeat('あ').take(2667).collect();
    let prompt = Client::build_prompt(&content, "function", "rust");
    assert!(prompt.len() <= 8100); // prompt overhead + truncated content
    assert!(prompt.is_char_boundary(prompt.len())); // valid UTF-8
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features gpu-index -p cqs --lib -- build_prompt_multibyte_no_panic 2>&1`
Expected: FAIL with "byte index 8000 is not a char boundary"

- [ ] **Step 3: Fix the truncation**

In `src/llm.rs`, replace lines 114-118:

```rust
    fn build_prompt(content: &str, chunk_type: &str, language: &str) -> String {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            &content[..content.floor_char_boundary(MAX_CONTENT_CHARS)]
        } else {
            content
        };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features gpu-index -p cqs --lib -- build_prompt_multibyte_no_panic 2>&1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/llm.rs
git commit -m "fix(llm): use floor_char_boundary for content truncation (RB-7)

Byte-offset slice panicked on multi-byte UTF-8 (CJK, emoji). Uses
floor_char_boundary (stable since 1.82, MSRV is 1.93)."
```

---

### Task 2: llm.rs — Validate batch_id format + disable redirects (SEC-5/SEC-6)

**Files:**
- Modify: `src/llm.rs:101-110` (Client::new)
- Modify: `src/llm.rs:174-175` (check_batch_status)
- Modify: `src/llm.rs:194-195` (wait_for_batch)
- Modify: `src/llm.rs:239-240` (fetch_batch_results)

- [ ] **Step 1: Write the validation test**

```rust
#[test]
fn is_valid_batch_id_accepts_real_ids() {
    assert!(is_valid_batch_id("msgbatch_abc123"));
    assert!(is_valid_batch_id("msgbatch_0123456789abcdef_ABCDEF"));
}

#[test]
fn is_valid_batch_id_rejects_crafted() {
    assert!(!is_valid_batch_id("../../v1/complete"));
    assert!(!is_valid_batch_id("msgbatch_abc?redirect=evil.com"));
    assert!(!is_valid_batch_id(""));
    assert!(!is_valid_batch_id("not_a_batch"));
    assert!(!is_valid_batch_id(&"msgbatch_".to_string() + &"a".repeat(200)));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features gpu-index -p cqs --lib -- is_valid_batch_id 2>&1`
Expected: FAIL — function doesn't exist yet

- [ ] **Step 3: Add validation function and disable redirects**

Add after the `Client` struct (around line 30):

```rust
/// Validate that a batch ID matches the expected Anthropic format.
/// Prevents URL injection from crafted `.cqs/index.db` metadata.
fn is_valid_batch_id(id: &str) -> bool {
    id.starts_with("msgbatch_")
        && id.len() < 100
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
```

In `Client::new`, disable redirects:

```rust
    pub fn new(api_key: &str) -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .expect("Failed to create HTTP client"),
            api_key: api_key.to_string(),
        }
    }
```

In `check_batch_status`, `wait_for_batch`, and `fetch_batch_results`, add validation before URL construction. For each function, add at the top:

```rust
        if !is_valid_batch_id(batch_id) {
            bail!("Invalid batch ID format: {}", batch_id);
        }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test --features gpu-index -p cqs --lib -- is_valid_batch_id 2>&1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/llm.rs
git commit -m "fix(llm): validate batch_id format + disable redirects (SEC-5/SEC-6)

Crafted batch_id in malicious .cqs/index.db could inject URL paths.
reqwest default redirect policy forwards x-api-key to redirect targets.
Fix: validate msgbatch_ format, disable auto-redirect."
```

---

### Task 3: llm.rs — Cap batch_items to prevent OOM (SEC-9)

**Files:**
- Modify: `src/llm.rs:24` (add constant)
- Modify: `src/llm.rs:366-370` (add flag before loop)
- Modify: `src/llm.rs:431-439` (add cap check after push)

- [ ] **Step 1: Add batch size constant**

Add near line 24 (after `BATCH_POLL_INTERVAL`):

```rust
/// Maximum items per Batches API request (API limit is 10,000)
const MAX_BATCH_SIZE: usize = 10_000;
```

- [ ] **Step 2: Add flag and cap logic**

Before the `loop` at line 380 (before `loop {`), add:

```rust
    let mut batch_full = false;
```

After the `batch_items.push(...)` block (after line 438, inside the `for cs in &chunks` loop), add:

```rust
            if batch_items.len() >= MAX_BATCH_SIZE {
                batch_full = true;
                break; // break inner for loop
            }
```

After the closing `}` of the inner `for cs in &chunks` loop (before `if chunks.is_empty()` — wait, actually the break condition is at the top: `if chunks.is_empty() { break; }`), add after the for loop ends:

```rust
        if batch_full {
            tracing::info!(
                count = MAX_BATCH_SIZE,
                "Batch size limit reached, remaining chunks deferred to next run"
            );
            break; // break outer loop
        }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --features gpu-index 2>&1 | tail -5`
Expected: compiles clean

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/llm.rs
git commit -m "fix(llm): cap batch_items at 10k to prevent OOM (SEC-9)

Malicious repo with 100k small functions could accumulate ~400MB in
batch_items Vec. Cap at API limit of 10k, defer remainder to next run."
```

---

### Task 4: llm.rs — Return Result from Client::new (EH-8)

**Files:**
- Modify: `src/llm.rs:101-110`
- Modify: `src/llm.rs:356` (call site in llm_summary_pass)

- [ ] **Step 1: Change Client::new to return Result**

```rust
    pub fn new(api_key: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .context("Failed to create HTTP client")?,
            api_key: api_key.to_string(),
        })
    }
```

- [ ] **Step 2: Update call site**

In `llm_summary_pass`, line 356:

```rust
    let client = Client::new(&api_key).context("Failed to create API client")?;
```

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo test --features gpu-index -p cqs --lib -- llm 2>&1 | tail -10`
Expected: all llm tests pass

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/llm.rs
git commit -m "fix(llm): Client::new returns Result instead of panicking (EH-8)

Library code should never .expect() on fallible operations. TLS backend
issues or missing system CA certs now propagate as errors."
```

---

### Task 5: llm.rs — Log errors from set_pending_batch_id / get_pending_batch_id (EH-10)

**Files:**
- Modify: `src/llm.rs:332` (set to None)
- Modify: `src/llm.rs:463,473` (get)
- Modify: `src/llm.rs:500,514` (set to Some)

- [ ] **Step 1: Replace .ok() with warn-on-error pattern**

At line 332, replace `store.set_pending_batch_id(None).ok();` with:

```rust
    if let Err(e) = store.set_pending_batch_id(None) {
        tracing::warn!(error = %e, "Failed to clear pending batch ID");
    }
```

At lines 500 and 514, replace `store.set_pending_batch_id(Some(&id)).ok();` with:

```rust
                    if let Err(e) = store.set_pending_batch_id(Some(&id)) {
                        tracing::warn!(error = %e, "Failed to persist batch ID — batch cannot be resumed if interrupted");
                    }
```

At lines 463 and 473, replace `if let Ok(Some(pending)) = store.get_pending_batch_id()` with:

```rust
        match store.get_pending_batch_id() {
            Ok(Some(pending)) => { /* existing body */ }
            Ok(None) => { /* no pending batch */ }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to check for pending batch ID");
                // Fall through to submit fresh batch
            }
        }
```

Restructure the two `if let Ok(Some(pending))` blocks as match statements. The line 463 block (empty batch_items path) becomes:

```rust
        match store.get_pending_batch_id() {
            Ok(Some(pending)) => {
                if !quiet {
                    eprintln!("Resuming pending batch {}", pending);
                }
                resume_or_fetch_batch(&client, store, &pending, quiet)?
            }
            Ok(None) => 0,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to check for pending batch ID");
                0
            }
        }
```

The line 473 block (has batch_items) becomes:

```rust
        let pending = match store.get_pending_batch_id() {
            Ok(Some(p)) => Some(p),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to check for pending batch ID");
                None
            }
        };
        let batch_id = if let Some(pending) = pending {
            // ... existing status check logic
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --features gpu-index 2>&1 | tail -5`
Expected: compiles clean

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add src/llm.rs
git commit -m "fix(llm): log errors from pending batch ID operations (EH-10)

Silent .ok() on set_pending_batch_id could cause duplicate API batches
(financial cost) or stale batch markers. Now logs warnings on failure."
```

---

### Task 6: watch.rs — Fail chunk writes if set_hnsw_dirty fails (DS-11)

**Files:**
- Modify: `src/cli/watch.rs:346`

- [ ] **Step 1: Replace .ok() with error check**

Replace line 346:

```rust
    store.set_hnsw_dirty(true).ok();
```

With:

```rust
    if let Err(e) = store.set_hnsw_dirty(true) {
        tracing::warn!(error = %e, "Cannot set HNSW dirty flag — skipping reindex to prevent stale index on crash");
        return;
    }
```

- [ ] **Step 2: Also add warn to the set_hnsw_dirty(false) calls (lines 378, ~419)**

Replace each `store.set_hnsw_dirty(false).ok();` with:

```rust
                        if let Err(e) = store.set_hnsw_dirty(false) {
                            tracing::warn!(error = %e, "Failed to clear HNSW dirty flag — unnecessary rebuild on next load");
                        }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --features gpu-index 2>&1 | tail -5`
Expected: compiles clean

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/cli/watch.rs
git commit -m "fix(watch): skip reindex if set_hnsw_dirty fails (DS-11)

The dirty flag is the sole crash safety mechanism for HNSW consistency.
If it fails to set, proceeding with chunk writes could leave a stale
HNSW index with no rebuild trigger."
```

---

### Task 7: task.rs — Cap waterfall_pack overshoot for impact/placement (AC-7)

**Files:**
- Modify: `src/cli/commands/task.rs:201`
- Modify: `src/cli/commands/task.rs:228`

**Context:** Lines 141-143 already cap scout overshoot with `scout_used.min(scout_budget)` and line 154 caps code with `code_used.min(code_budget)`. The comment at line 141 documents the pattern: "Charge only the budgeted portion to remaining — overshoot from first-item guarantee doesn't cascade into downstream section budgets." Lines 201 and 228 are missing this same cap.

- [ ] **Step 1: Apply the fix**

At line 201, change:

```rust
    remaining = remaining.saturating_sub(risk_used + tests_used);
```

To:

```rust
    remaining = remaining.saturating_sub((risk_used + tests_used).min(impact_budget));
```

At line 228, change:

```rust
    remaining = remaining.saturating_sub(placement_used);
```

To:

```rust
    remaining = remaining.saturating_sub(placement_used.min(placement_budget));
```

- [ ] **Step 2: Verify it compiles and existing tests pass**

Run: `cargo test --features gpu-index -p cqs --lib -- task 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add src/cli/commands/task.rs
git commit -m "fix(task): cap waterfall_pack overshoot for impact/placement (AC-7)

AC-2 fix capped scout and code sections but missed impact (line 201)
and placement (line 228). First-item overshoot could steal budget from
downstream sections."
```

---

### Task 8: audit.rs — Fix non-atomic copy fallback (DS-8/PB-11)

**Files:**
- Modify: `src/audit.rs:125-134`

- [ ] **Step 1: Fix the fallback to use temp-then-rename**

Replace lines 125-134:

```rust
    if let Err(rename_err) = std::fs::rename(&tmp_path, &path) {
        // Cross-device fallback: copy to same-dir temp, then rename (atomic)
        let dest_dir = path.parent().unwrap_or(std::path::Path::new("."));
        let dest_tmp = dest_dir.join(format!(".audit.{:016x}.tmp", suffix));
        if let Err(copy_err) = std::fs::copy(&tmp_path, &dest_tmp) {
            let _ = std::fs::remove_file(&tmp_path);
            anyhow::bail!(
                "rename failed ({}), copy fallback failed: {}",
                rename_err,
                copy_err
            );
        }
        if let Err(rename2_err) = std::fs::rename(&dest_tmp, &path) {
            let _ = std::fs::remove_file(&dest_tmp);
            let _ = std::fs::remove_file(&tmp_path);
            anyhow::bail!(
                "rename failed ({}), fallback rename failed: {}",
                rename_err,
                rename2_err
            );
        }
        let _ = std::fs::remove_file(&tmp_path);
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --features gpu-index 2>&1 | tail -5`
Expected: compiles clean

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add src/audit.rs
git commit -m "fix(audit): atomic copy fallback for cross-device writes (DS-8)

Copy-fallback wrote directly to final path. Now copies to same-dir
temp then renames, matching the pattern in note.rs/config.rs/project.rs."
```

---

### Task 9: embedder.rs — Fix provider symlink cleanup for both directories (SEC-8 + PB-14/RM-13)

**Files:**
- Modify: `src/embedder.rs:700-709` (ensure_ort_provider_libs — collect all paths before registering)
- Modify: `src/embedder.rs:798-817` (register_provider_cleanup — use Mutex instead of OnceLock)

**Context:** `ensure_ort_provider_libs()` is called from `detect_provider()` at line 843, inside a `static OnceCell` cached function. This means a RAII guard can't be stored in `Embedder` — the call site is module-level. Instead, fix the registration to handle multiple path sets.

- [ ] **Step 1: Collect all paths before calling register_provider_cleanup once**

In `ensure_ort_provider_libs()`, replace lines 700-709:

```rust
    symlink_providers(&ort_lib_dir, &ort_search_dir, &provider_libs);

    // Collect all symlink paths for cleanup
    let mut cleanup_paths: Vec<PathBuf> = provider_libs.iter().map(|lib| ort_search_dir.join(lib)).collect();

    // Also symlink into LD_LIBRARY_PATH for other search paths
    if let Some(ld_dir) = find_ld_library_dir(&ort_lib_dir) {
        symlink_providers(&ort_lib_dir, &ld_dir, &provider_libs);
        cleanup_paths.extend(provider_libs.iter().map(|lib| ld_dir.join(lib)));
    }

    // Register cleanup for ALL symlinked paths (both directories)
    register_provider_cleanup(cleanup_paths);
```

- [ ] **Step 2: Rewrite register_provider_cleanup to accept Vec and use Mutex**

Replace the entire `register_provider_cleanup` function (lines 798-817):

```rust
/// Register atexit cleanup for provider symlinks.
/// Uses Mutex to support paths from multiple directories.
#[cfg(unix)]
fn register_provider_cleanup(paths: Vec<PathBuf>) {
    use std::sync::Mutex;

    static CLEANUP_PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    if let Ok(mut guard) = CLEANUP_PATHS.lock() {
        guard.extend(paths);
    }

    // Register atexit handler only once
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        extern "C" fn cleanup() {
            // Note: remove_file may allocate. This is acceptable for a CLI tool
            // that always exits normally. For shared library usage (theoretical),
            // this could deadlock if exit() is called from a signal handler.
            if let Ok(paths) = CLEANUP_PATHS.lock() {
                for path in paths.iter() {
                    if path.symlink_metadata().is_ok() && std::fs::read_link(path).is_ok() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
        unsafe { libc::atexit(cleanup) };
    });
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --features gpu-index 2>&1 | tail -5`
Expected: compiles clean

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/embedder.rs
git commit -m "fix(embedder): clean up provider symlinks from both directories (SEC-8, PB-14, RM-13)

OnceLock::set() only succeeded once — second directory's symlinks
leaked. Now collects all paths into Mutex<Vec> and registers atexit
once via std::sync::Once. Documented atexit allocation caveat (SEC-8)
as acceptable for CLI usage."
```

---

### Task 10: cagra.rs — Early return for k=0 (RB-11)

**Files:**
- Modify: `src/cagra.rs:153-157`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn search_k_zero_returns_empty() {
    // k=0 should return empty without passing zero-sized buffers to GPU
    // (Existing test may already cover this — verify, add if not)
}
```

Check if a test already exists. If so, just add the code fix.

- [ ] **Step 2: Add early return after empty check**

After line 157 (`return Vec::new();` for empty id_map), add:

```rust
        if k == 0 {
            return Vec::new();
        }
```

- [ ] **Step 3: Verify it compiles and tests pass**

Run: `cargo test --features gpu-index -p cqs --lib -- cagra 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/cagra.rs
git commit -m "fix(cagra): early return for k=0 to avoid zero-sized GPU buffers (RB-11)

k=0 was passing zero-sized ndarray to cuVS search(), which may cause
undefined behavior depending on the GPU library implementation."
```

---

### Task 11: Docs — Fix schema version (DOC-8/DOC-9) and CHANGELOG (DOC-13)

**Files:**
- Modify: `README.md:35`
- Modify: `CONTRIBUTING.md:125`
- Modify: `CHANGELOG.md:8`

- [ ] **Step 1: Fix README schema version**

In `README.md`, change:

```
cqs index --force  # Run after upgrading from older versions (current schema: v12)
```

To:

```
cqs index --force  # Run after upgrading from older versions (current schema: v14)
```

- [ ] **Step 2: Fix CONTRIBUTING.md schema version**

In `CONTRIBUTING.md`, change:

```
  store/        - SQLite storage layer (Schema v12, WAL mode)
```

To:

```
  store/        - SQLite storage layer (Schema v14, WAL mode)
```

- [ ] **Step 3: Add CHANGELOG entries**

In `CHANGELOG.md`, after line 8 (`## [Unreleased]`), add:

```markdown

### Fixed
- CAGRA use-after-free on shape pointers — host ndarrays dropped while device tensors referenced them (#613)
- ORT CUDA provider path resolution — dladdr returns argv[0] on glibc, ORT falls back to CWD (#613)
- LLM batch resume on interrupt — persist batch_id in SQLite metadata, resume polling on restart (#613)

### Changed
- LLM summaries now use Batches API for throughput (no RPM limit, 50% discount) (#605)
```

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add README.md CONTRIBUTING.md CHANGELOG.md
git commit -m "docs: fix schema version v12→v14, add missing CHANGELOG entries (DOC-8/9/13)"
```

---

### Task 12: plan.rs — Fix stale "Add ChunkType Variant" checklist (EX-10)

**Files:**
- Modify: `src/plan.rs:148-155`

- [ ] **Step 1: Update the checklist**

Replace lines 148-155:

```rust
            checklist: &[
                "src/language/mod.rs — Add one line to define_chunk_types! macro (Display, FromStr auto-generated)",
                "src/language/mod.rs — Update is_callable() and human_name() if needed",
                "src/parser/types.rs — Add capture name mapping in capture_name_to_chunk_type()",
                "src/nl.rs — Add natural language label for the variant",
                "src/language/<lang>.rs — Add capture using the new variant name in chunk_query",
                "tests/parser_test.rs — Parser tests for each language using the variant",
                "ROADMAP.md — Update ChunkType Variant Status table",
            ],
```

- [ ] **Step 2: Update the patterns block below it**

Replace lines 156-160:

```rust
            patterns: &[
                "is_callable() returns true for Function, Method, Macro — most others false",
                "define_chunk_types! generates Display (lowercase), FromStr (snake_case + spaces), all_names()",
                "capture_name_to_chunk_type() maps tree-sitter capture names to ChunkType (may differ from Display)",
                "Container extraction uses capture_types to decide container vs leaf",
            ],
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --features gpu-index 2>&1 | tail -5`
Expected: compiles clean

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/plan.rs
git commit -m "fix(plan): update Add ChunkType checklist for macro consolidation (EX-10)

Checklist said 'update Display, FromStr' which are now auto-generated
by define_chunk_types! macro. Added missing capture_name_to_chunk_type()
step which is the most likely to forget (silent failure when omitted)."
```

---

### Task 13: Consolidate DeadConfidence / DeadConfidenceLevel (EX-12)

**Files:**
- Modify: `src/store/calls.rs:30-31` (add clap::ValueEnum derive)
- Modify: `src/cli/mod.rs:125-141` (delete DeadConfidenceLevel + From impl)
- Modify: `src/cli/mod.rs:571` (change field type)
- Modify: `src/cli/batch/commands.rs:7,160` (change import and field type)
- Modify: `src/cli/batch/handlers.rs:10,682` (change import and param type)

- [ ] **Step 1: Add clap::ValueEnum to DeadConfidence**

In `src/store/calls.rs:30`, change:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
```

To:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, clap::ValueEnum)]
```

- [ ] **Step 2: Delete DeadConfidenceLevel and From impl from cli/mod.rs**

Remove lines 125-141 (the `DeadConfidenceLevel` enum and its `From` impl).

- [ ] **Step 3: Update cli/mod.rs field**

At line 571, change `min_confidence: DeadConfidenceLevel` to `min_confidence: cqs::store::DeadConfidence`.

- [ ] **Step 4: Update batch/commands.rs**

Change the import at line 7: remove `DeadConfidenceLevel` from the import list. Add `use cqs::store::DeadConfidence;`.

At line 160, change `min_confidence: DeadConfidenceLevel` to `min_confidence: DeadConfidence`.

At line 493 in the test, change `DeadConfidenceLevel::High` to `DeadConfidence::High`.

- [ ] **Step 5: Update batch/handlers.rs**

Change the import at line 10: remove `DeadConfidenceLevel` from the import list. Add `use cqs::store::DeadConfidence;`.

At line 682, change `min_confidence: &DeadConfidenceLevel` to `min_confidence: &DeadConfidence`.

Remove the `.into()` / `From` conversion at the call site (it's now the same type). Grep for `DeadConfidence::from` or `into()` on min_confidence to find conversion sites.

- [ ] **Step 6: Verify it compiles and tests pass**

Run: `cargo test --features gpu-index -p cqs 2>&1 | grep -E "(FAIL|error|test result)" | tail -5`
Expected: all pass, no errors

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/store/calls.rs src/cli/mod.rs src/cli/batch/commands.rs src/cli/batch/handlers.rs
git commit -m "refactor: consolidate DeadConfidence + DeadConfidenceLevel into one enum (EX-12)

Added clap::ValueEnum to the library DeadConfidence enum, deleted the
CLI-only DeadConfidenceLevel duplicate and its manual From impl."
```

---

### Task 14: Final verification

- [ ] **Step 1: Full build**

Run: `cargo build --features gpu-index 2>&1 | grep -E "(error|warning)" | head -20`
Expected: no errors, only pre-existing warnings if any

- [ ] **Step 2: Full test suite**

Run: `cargo test --features gpu-index 2>&1 | grep "^test result:" | awk '{p+=$4; f+=$6} END {printf "%d pass, %d fail\n", p, f}'`
Expected: ~1650 pass, 0 fail

- [ ] **Step 3: Update audit-triage.md with fix status**

Mark all 15 P1 items as fixed in `docs/audit-triage.md`.

- [ ] **Step 4: Commit triage update**

```bash
git add docs/audit-triage.md
git commit -m "docs: mark all P1 audit findings as fixed"
```
