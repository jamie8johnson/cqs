# Adversarial Test Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add adversarial and edge-case tests across 5 weak areas identified during the v1.4.0 audit: parser malformed input, store concurrent access, LLM batch error responses, search adversarial embeddings, and contrastive neighbor edge cases. Also update the audit skill's Test Coverage category.

**Architecture:** Tests only — no production code changes. Each task adds tests to the existing test module in the relevant source file. Tests verify graceful degradation (no panics, no data corruption) on adversarial inputs.

**Tech Stack:** Rust, `#[cfg(test)]`, tempfile, proptest (where applicable)

---

## Constraints

- **Test-only changes** — do not modify production code. If a test reveals a bug (panic, corruption), document it as a finding but don't fix it in this PR.
- **No network mocking** — LLM batch tests cover the JSONL parsing logic, not HTTP requests.
- **Existing test helpers** — reuse `make_test_store()`, `make_chunk_summary()`, `test_embedding()` where available.
- **`--features gpu-index`** for all cargo commands.

---

### Task 1: Parser malformed input tests

**Files:**
- Modify: `src/parser/mod.rs` — add tests to existing `#[cfg(test)] mod tests` block (~line 772)

- [ ] **Step 1: Write empty/minimal file tests**

```rust
#[test]
fn parse_source_empty_string() {
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_source("", Language::Rust, Path::new("empty.rs")).unwrap();
    assert!(chunks.is_empty());
}

#[test]
fn parse_source_whitespace_only() {
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_source("   \n\n\t  ", Language::Rust, Path::new("ws.rs")).unwrap();
    assert!(chunks.is_empty());
}

#[test]
fn parse_source_only_comments() {
    let parser = Parser::new().unwrap();
    let chunks = parser.parse_source("// just a comment\n/* block */", Language::Rust, Path::new("c.rs")).unwrap();
    assert!(chunks.is_empty());
}
```

- [ ] **Step 2: Write binary/malformed content tests**

```rust
#[test]
fn parse_source_binary_content_no_panic() {
    let parser = Parser::new().unwrap();
    let binary = "\x00\x01\x02\xFF\xFE\x89PNG";
    // Should not panic — binary content is valid UTF-8 (it's a &str)
    // but tree-sitter should produce no meaningful chunks
    let result = parser.parse_source(binary, Language::Rust, Path::new("bin.rs"));
    assert!(result.is_ok());
}

#[test]
fn parse_source_extremely_long_line() {
    let parser = Parser::new().unwrap();
    let long_line = format!("fn f() {{ let x = \"{}\"; }}", "a".repeat(200_000));
    let result = parser.parse_source(&long_line, Language::Rust, Path::new("long.rs"));
    assert!(result.is_ok());
}

#[test]
fn parse_source_deeply_nested_braces() {
    let parser = Parser::new().unwrap();
    let nested = format!("fn f() {{ {} }}", "{".repeat(500));
    // Malformed — unclosed braces. Should not panic.
    let result = parser.parse_source(&nested, Language::Rust, Path::new("nest.rs"));
    assert!(result.is_ok());
}
```

- [ ] **Step 3: Write multi-language edge case tests**

```rust
#[test]
fn parse_source_wrong_language_no_panic() {
    let parser = Parser::new().unwrap();
    // Python code parsed as Rust — should not panic, may produce empty/wrong chunks
    let result = parser.parse_source("def foo():\n    pass", Language::Rust, Path::new("wrong.rs"));
    assert!(result.is_ok());
}

#[test]
fn parse_source_null_bytes_in_source() {
    let parser = Parser::new().unwrap();
    let source = "fn foo() {}\0fn bar() {}";
    let result = parser.parse_source(source, Language::Rust, Path::new("null.rs"));
    assert!(result.is_ok());
}
```

- [ ] **Step 4: Write parse_file_all edge cases**

```rust
#[test]
fn parse_file_all_nonexistent_file() {
    let parser = Parser::new().unwrap();
    let result = parser.parse_file_all(Path::new("/nonexistent/file.rs"));
    assert!(result.is_err());
}

#[test]
fn parse_file_all_empty_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("empty.rs");
    std::fs::write(&path, "").unwrap();
    let parser = Parser::new().unwrap();
    let (chunks, calls, type_refs) = parser.parse_file_all(&path).unwrap();
    assert!(chunks.is_empty());
    assert!(calls.is_empty());
    assert!(type_refs.is_empty());
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test --features gpu-index -p cqs --lib -- parser::tests 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git commit -m "test: adversarial parser tests — empty, binary, malformed, wrong-language input"
```

---

### Task 2: Store concurrent access tests

**Files:**
- Modify: `src/store/mod.rs` — add tests to existing `#[cfg(test)] mod tests` block

- [ ] **Step 1: Write concurrent reader tests**

```rust
#[test]
fn concurrent_readonly_opens() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();
    drop(store);

    // Multiple readonly opens should not conflict (WAL mode)
    let r1 = Store::open_readonly(&db_path).unwrap();
    let r2 = Store::open_readonly(&db_path).unwrap();
    let s1 = r1.stats().unwrap();
    let s2 = r2.stats().unwrap();
    assert_eq!(s1.total_chunks, s2.total_chunks);
}

#[test]
fn readonly_open_while_writer_holds() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");
    let writer = Store::open(&db_path).unwrap();
    writer.init(&ModelInfo::default()).unwrap();

    // Readonly open while writer is alive should succeed (WAL mode)
    let reader = Store::open_readonly(&db_path).unwrap();
    assert!(reader.stats().is_ok());
}
```

- [ ] **Step 2: Write OnceLock cache staleness test**

```rust
#[test]
fn onclock_cache_not_invalidated_by_writes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // First call caches call graph
    let g1 = store.get_call_graph().unwrap();
    assert!(g1.forward.is_empty());

    // Write a function call
    store.upsert_function_calls(
        Path::new("test.rs"),
        &[crate::parser::FunctionCalls {
            name: "caller".to_string(),
            line_start: 1,
            calls: vec![crate::parser::CallSite {
                callee_name: "callee".to_string(),
                line_number: 2,
            }],
        }],
    ).unwrap();

    // Second call returns cached (stale) graph — intentional design
    let g2 = store.get_call_graph().unwrap();
    assert!(g2.forward.is_empty(), "OnceLock should return stale cached value");
}
```

- [ ] **Step 3: Write open-on-nonexistent and double-init tests**

```rust
#[test]
fn open_creates_parent_dirs() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("sub/dir/index.db");
    // Should create parent directories
    let result = Store::open(&db_path);
    assert!(result.is_ok());
}

#[test]
fn double_init_is_idempotent() {
    let (store, _dir) = make_test_store_initialized();
    // Second init should succeed (INSERT OR REPLACE on metadata)
    let result = store.init(&ModelInfo::default());
    assert!(result.is_ok());
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --features gpu-index -p cqs --lib -- store::tests 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git commit -m "test: adversarial store tests — concurrent access, cache staleness, double init"
```

---

### Task 3: Search adversarial embedding tests

**Files:**
- Modify: `src/search/scoring/candidate.rs` — add tests to existing `#[cfg(test)] mod tests` block
- Modify: `src/math.rs` — add tests to existing `#[cfg(test)] mod tests` block

- [ ] **Step 1: Write NaN/Inf embedding tests for cosine_similarity**

```rust
// In src/math.rs tests:

#[test]
fn cosine_nan_embedding() {
    let a = vec![f32::NAN; EMBEDDING_DIM];
    let b = make_embedding(1.0);
    assert!(cosine_similarity(&a, &b).is_none());
}

#[test]
fn cosine_inf_embedding() {
    let mut a = make_embedding(1.0);
    a[0] = f32::INFINITY;
    let b = make_embedding(1.0);
    assert!(cosine_similarity(&a, &b).is_none());
}

#[test]
fn cosine_zero_norm_vector() {
    let a = vec![0.0f32; EMBEDDING_DIM];
    let b = make_embedding(1.0);
    // Zero-norm: dot product is 0, denominator is 0
    assert!(cosine_similarity(&a, &b).is_none());
}

#[test]
fn cosine_negative_inf_embedding() {
    let mut a = make_embedding(1.0);
    a[0] = f32::NEG_INFINITY;
    let b = make_embedding(1.0);
    assert!(cosine_similarity(&a, &b).is_none());
}

#[test]
fn cosine_subnormal_values() {
    let a = vec![f32::MIN_POSITIVE / 2.0; EMBEDDING_DIM];
    let b = vec![f32::MIN_POSITIVE / 2.0; EMBEDDING_DIM];
    // Subnormal: very small but valid. Should not panic.
    let result = cosine_similarity(&a, &b);
    // May return None (non-finite) or Some — either is acceptable
    if let Some(s) = result {
        assert!(s.is_finite());
    }
}
```

- [ ] **Step 2: Write BoundedScoreHeap adversarial tests**

```rust
// In src/search/scoring/candidate.rs tests:

#[test]
fn heap_all_nan_scores() {
    let mut heap = BoundedScoreHeap::new(5);
    heap.push("a".into(), f32::NAN);
    heap.push("b".into(), f32::NAN);
    heap.push("c".into(), f32::NAN);
    let result = heap.into_sorted_vec();
    assert!(result.is_empty(), "NaN scores should all be rejected");
}

#[test]
fn heap_mixed_valid_and_nan() {
    let mut heap = BoundedScoreHeap::new(5);
    heap.push("valid1".into(), 0.5);
    heap.push("nan".into(), f32::NAN);
    heap.push("valid2".into(), 0.8);
    heap.push("inf".into(), f32::INFINITY);
    heap.push("valid3".into(), 0.3);
    let result = heap.into_sorted_vec();
    assert_eq!(result.len(), 3, "Only finite scores kept");
    assert_eq!(result[0].0, "valid2"); // highest first
}

#[test]
fn heap_negative_scores() {
    let mut heap = BoundedScoreHeap::new(3);
    heap.push("neg1".into(), -0.5);
    heap.push("neg2".into(), -0.1);
    heap.push("pos".into(), 0.1);
    let result = heap.into_sorted_vec();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].0, "pos");
}

#[test]
fn heap_capacity_zero() {
    let mut heap = BoundedScoreHeap::new(0);
    heap.push("a".into(), 1.0);
    let result = heap.into_sorted_vec();
    assert!(result.is_empty());
}
```

- [ ] **Step 3: Write score_candidate adversarial tests**

```rust
#[test]
fn score_candidate_zero_embedding() {
    let query = test_embedding(1.0);
    let zero = vec![0.0f32; 768];
    let filter = SearchFilter {
        query_text: "test".into(),
        ..Default::default()
    };
    // Zero embedding → cosine returns None → score is None
    let result = score_candidate(&zero, &query, None, "src/test.rs", &filter, None, None, &NoteBoostIndex::empty(), 0.0);
    assert!(result.is_none());
}

#[test]
fn score_candidate_truncated_embedding() {
    let query = test_embedding(1.0);
    let short = vec![0.5f32; 100]; // Wrong dimension
    let filter = SearchFilter {
        query_text: "test".into(),
        ..Default::default()
    };
    let result = score_candidate(&short, &query, None, "src/test.rs", &filter, None, None, &NoteBoostIndex::empty(), 0.0);
    assert!(result.is_none());
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --features gpu-index -p cqs --lib -- search::scoring::candidate::tests math::tests 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git commit -m "test: adversarial search tests — NaN/Inf/zero embeddings, heap edge cases"
```

---

### Task 4: Contrastive neighbor edge case tests

**Files:**
- Modify: `src/llm/summary.rs` — add tests to existing `#[cfg(test)] mod tests` block

These tests can't call `find_contrastive_neighbors` directly (it requires a Store with embeddings), but we can test the matrix computation logic by extracting it or testing via Store fixtures.

- [ ] **Step 1: Write ndarray matrix edge case tests**

```rust
#[test]
fn contrastive_neighbors_require_store_with_few_chunks() {
    // Create store with only 1 callable chunk — should return empty
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");
    let store = crate::Store::open(&db_path).unwrap();
    store.init(&crate::store::ModelInfo::default()).unwrap();

    let result = find_contrastive_neighbors(&store, 3).unwrap();
    assert!(result.is_empty(), "< 2 chunks should return empty neighbors");
}

#[test]
fn contrastive_neighbors_limit_zero() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("index.db");
    let store = crate::Store::open(&db_path).unwrap();
    store.init(&crate::store::ModelInfo::default()).unwrap();

    let result = find_contrastive_neighbors(&store, 0).unwrap();
    assert!(result.is_empty());
}
```

- [ ] **Step 2: Write L2 normalization edge case tests**

Test the normalization logic in isolation using ndarray directly:

```rust
#[test]
fn l2_normalize_zero_vector_no_panic() {
    use ndarray::Array2;
    let mut matrix = Array2::<f32>::zeros((2, 4));
    // Row 0: zero vector
    // Row 1: normal vector
    matrix[[1, 0]] = 1.0;
    matrix[[1, 1]] = 0.0;
    matrix[[1, 2]] = 0.0;
    matrix[[1, 3]] = 0.0;

    for i in 0..2 {
        let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
        if norm > 0.0 {
            matrix.row_mut(i).mapv_inplace(|x| x / norm);
        }
    }

    // Zero row stays zero, normalized row has unit norm
    assert_eq!(matrix[[0, 0]], 0.0);
    assert!((matrix[[1, 0]] - 1.0).abs() < 1e-6);
}

#[test]
fn pairwise_cosine_with_zero_row() {
    use ndarray::Array2;
    let mut matrix = Array2::<f32>::zeros((3, 4));
    // Row 0: unit vector [1,0,0,0]
    matrix[[0, 0]] = 1.0;
    // Row 1: zero vector (stays zero after normalization)
    // Row 2: unit vector [0,1,0,0]
    matrix[[2, 1]] = 1.0;

    let sims = matrix.dot(&matrix.t());

    // self-sim of zero vector should be 0
    assert_eq!(sims[[1, 1]], 0.0);
    // sim between zero and unit should be 0
    assert_eq!(sims[[0, 1]], 0.0);
    // sim between orthogonal units should be 0
    assert!((sims[[0, 2]]).abs() < 1e-6);
}

#[test]
fn pairwise_cosine_identical_vectors() {
    use ndarray::Array2;
    let mut matrix = Array2::<f32>::zeros((3, 4));
    // All three rows identical
    for i in 0..3 {
        matrix[[i, 0]] = 0.5;
        matrix[[i, 1]] = 0.5;
        matrix[[i, 2]] = 0.5;
        matrix[[i, 3]] = 0.5;
    }
    // Normalize
    for i in 0..3 {
        let norm = matrix.row(i).mapv(|x| x * x).sum().sqrt();
        if norm > 0.0 {
            matrix.row_mut(i).mapv_inplace(|x| x / norm);
        }
    }

    let sims = matrix.dot(&matrix.t());

    // All pairwise sims should be ~1.0
    for i in 0..3 {
        for j in 0..3 {
            assert!((sims[[i, j]] - 1.0).abs() < 1e-5, "sim[{i}][{j}] = {}", sims[[i, j]]);
        }
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --features gpu-index -p cqs --lib -- llm::summary::tests 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git commit -m "test: adversarial contrastive neighbor tests — zero vectors, identical embeddings, limit edge cases"
```

---

### Task 5: LLM batch JSONL parsing edge cases

**Files:**
- Modify: `src/llm/mod.rs` — add tests to existing TC-21 test block

The TC-21 tests already have a `parse_batch_results_jsonl` helper. Add adversarial cases.

- [ ] **Step 1: Write malformed JSONL tests**

```rust
#[test]
fn parse_jsonl_truncated_json() {
    let body = r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"good"}]}}}
{"custom_id":"h2","result":{"type":"succeed"#;
    let results = parse_batch_results_jsonl(body);
    assert_eq!(results.len(), 1, "Truncated line should be skipped");
    assert_eq!(results.get("h1").unwrap(), "good");
}

#[test]
fn parse_jsonl_unicode_in_summary() {
    let body = r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"日本語のサマリー 🎉"}]}}}"#;
    let results = parse_batch_results_jsonl(body);
    assert_eq!(results.get("h1").unwrap(), "日本語のサマリー 🎉");
}

#[test]
fn parse_jsonl_very_long_summary() {
    let long_text = "x".repeat(100_000);
    let body = format!(
        r#"{{"custom_id":"h1","result":{{"type":"succeeded","message":{{"content":[{{"type":"text","text":"{}"}}]}}}}}}"#,
        long_text
    );
    let results = parse_batch_results_jsonl(&body);
    assert_eq!(results.get("h1").unwrap().len(), 100_000);
}

#[test]
fn parse_jsonl_duplicate_custom_ids() {
    let body = r#"{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"first"}]}}}
{"custom_id":"h1","result":{"type":"succeeded","message":{"content":[{"type":"text","text":"second"}]}}}"#;
    let results = parse_batch_results_jsonl(body);
    // Last one wins (HashMap insert)
    assert_eq!(results.len(), 1);
}

#[test]
fn parse_jsonl_null_fields() {
    let body = r#"{"custom_id":"h1","result":{"type":"succeeded","message":null}}"#;
    let results = parse_batch_results_jsonl(body);
    assert!(results.is_empty(), "Null message should produce no result");
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --features gpu-index -p cqs --lib -- llm::tests::parse_jsonl 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git commit -m "test: adversarial JSONL parsing tests — truncated, unicode, duplicates, nulls"
```

---

### Task 6: Update audit skill Test Coverage category

**Files:**
- Modify: `.claude/skills/audit/SKILL.md`

- [ ] **Step 1: Add adversarial coverage check to Test Coverage scope**

In the "Category Scopes" table, update the Test Coverage row:

```
| Test Coverage | Gaps, meaningful assertions, integration tests, **adversarial/edge-case coverage (malformed input, concurrent access, NaN/Inf values, error path exercising)** |
```

- [ ] **Step 2: Add to Mandatory First Steps**

In the "Batch 2" section under "Test Coverage", add:

```
- **Test Coverage**: Run `cqs health --json` first — includes untested hotspots. **Also check for adversarial test gaps**: look for functions that accept user input, external data, or embeddings — verify they have tests for malformed/adversarial inputs (empty, NaN, truncated, wrong-type, concurrent).
```

- [ ] **Step 3: Commit**

```bash
git commit -m "docs: update audit skill — Test Coverage checks for adversarial/edge-case gaps"
```

---

## Estimated effort

| Task | Tests | Time | Risk |
|------|-------|------|------|
| 1. Parser malformed input | ~8 | 15 min | Low — pure input variation |
| 2. Store concurrent access | ~4 | 10 min | Low — WAL mode handles it |
| 3. Search adversarial embeddings | ~10 | 15 min | Low — existing guards |
| 4. Contrastive neighbor edge cases | ~4 | 10 min | Low — ndarray math |
| 5. LLM batch JSONL parsing | ~5 | 10 min | Low — string parsing |
| 6. Audit skill update | 0 | 5 min | None |
| **Total** | **~31** | **~65 min** | |

## What this does NOT include

- **Fuzz testing (cargo-fuzz)** — property-based testing with proptest covers similar ground for less setup cost. cargo-fuzz targets could be a follow-up.
- **Network error mocking** — LLM HTTP errors require mocking reqwest. The JSONL parsing tests cover the parseable surface without network deps.
- **Concurrent write tests** — SQLite WAL handles concurrent writes at the DB level. Testing write contention would require thread spawning and is low-value since cqs uses single-writer patterns.
