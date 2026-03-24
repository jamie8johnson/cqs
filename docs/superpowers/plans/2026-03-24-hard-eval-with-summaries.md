# Hard Eval with LLM Summaries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Inject contrastive LLM summaries into the hard eval fixture embeddings so R@1 measures the full enrichment pipeline, not just raw embeddings. Currently hard eval = 89.1% R@1 (raw), full pipeline = 92.7% R@1 (with summaries). This gap makes it hard to measure model improvements end-to-end.

**Architecture:** Generate contrastive summaries for the ~268 fixture chunks via the Batches API (one-time, ~$0.02), store as a JSON fixture file. The eval harness loads summaries and prepends them to NL descriptions before embedding, matching production behavior. Add `--with-summaries` variant to hard eval tests.

**Tech Stack:** Rust, Claude Batches API, serde_json

---

## Constraints

- **Don't change existing tests** — add new `_with_summaries` variants alongside existing tests. The raw eval (without summaries) remains the baseline for model quality.
- **Fixture file** — pre-generated summaries stored as `tests/fixtures/eval_hard_summaries.json` (JSON map: `function_name → summary`). Not generated on the fly (would require API key in CI).
- **No call context** — fixtures don't have a call graph. Summaries only, no caller/callee enrichment. This isolates the summary contribution.
- **Contrastive summaries** — use the same contrastive prompt as production (with neighbors). Neighbors computed from fixture embeddings.

## Data Flow

```
fixture chunks → parse → generate_nl_description() → base NL
                                                       ↓
eval_hard_summaries.json → load → prepend summary → enriched NL
                                                       ↓
                                            embed_documents() → embedding
```

---

### Task 1: Generate fixture summaries

**Files:**
- Create: `tests/fixtures/eval_hard_summaries.json`

This is a one-time generation step using the existing cqs binary. Not automated in CI.

- [ ] **Step 1: Write a script to generate summaries for fixtures**

```bash
# Index the fixture files into a temp store, run summary pass, extract
cd /mnt/c/Projects/cqs

# Create temp index of just fixture files
TMPDIR=$(mktemp -d)
mkdir -p "$TMPDIR/.cqs"

# Parse and index fixtures
for f in tests/fixtures/eval_hard_*.rs tests/fixtures/eval_hard_*.py \
         tests/fixtures/eval_hard_*.ts tests/fixtures/eval_hard_*.js \
         tests/fixtures/eval_hard_*.go; do
  echo "$f"
done

# Use Python to generate the JSON by querying the live index
# (fixtures are already indexed in our main .cqs/)
python3 -c "
import sqlite3, json

db = sqlite3.connect('.cqs/index.db')

# Get all callable chunks from fixture files
rows = db.execute('''
    SELECT c.name, s.summary
    FROM chunks c
    JOIN llm_summaries s ON c.content_hash = s.content_hash
    WHERE c.origin LIKE 'tests/fixtures/eval_hard_%'
    AND s.purpose = 'summary'
    AND c.chunk_type IN ('function', 'method', 'constructor')
''').fetchall()

summaries = {}
for name, summary in rows:
    # Use name as key (eval matches by name)
    if name not in summaries:
        summaries[name] = summary

with open('tests/fixtures/eval_hard_summaries.json', 'w') as f:
    json.dump(summaries, f, indent=2, sort_keys=True)

print(f'Generated {len(summaries)} summaries')
db.close()
"
```

- [ ] **Step 2: Verify fixture file**

```bash
wc -l tests/fixtures/eval_hard_summaries.json
python3 -c "import json; d=json.load(open('tests/fixtures/eval_hard_summaries.json')); print(f'{len(d)} summaries'); [print(f'  {k}: {v[:60]}') for k,v in list(d.items())[:5]]"
```

Expected: ~80-120 summaries (callable functions across 5 language fixtures).

- [ ] **Step 3: Commit fixture file**

```bash
git add tests/fixtures/eval_hard_summaries.json
git commit -m "test: pre-generated contrastive summaries for hard eval fixtures"
```

---

### Task 2: Add summary loading to eval harness

**Files:**
- Modify: `tests/model_eval.rs` — add summary loading and enriched embedding path

- [ ] **Step 1: Add summary loading function**

After the existing `use` imports in model_eval.rs, add:

```rust
/// Load pre-generated contrastive summaries for hard eval fixtures.
/// Returns a map from function name to summary text.
fn load_fixture_summaries() -> HashMap<String, String> {
    let path = std::path::Path::new("tests/fixtures/eval_hard_summaries.json");
    if !path.exists() {
        return HashMap::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}
```

- [ ] **Step 2: Add enriched NL generation helper**

```rust
/// Generate NL with optional summary prepended (matches production enrichment).
fn generate_enriched_nl(chunk: &cqs::parser::Chunk, summary: Option<&str>) -> String {
    let base_nl = cqs::nl::generate_nl_description(chunk);
    match summary {
        Some(s) if !s.is_empty() => format!("{} {}", s, base_nl),
        _ => base_nl,
    }
}
```

- [ ] **Step 3: Run tests to verify existing tests still pass**

```bash
cargo test --features gpu-index --test model_eval -- test_hard_model_comparison --ignored --nocapture 2>&1 | grep "Recall@1"
```

- [ ] **Step 4: Commit**

```bash
git commit -m "test: add summary loading and enriched NL helpers to eval harness"
```

---

### Task 3: Add `test_hard_with_summaries` test

**Files:**
- Modify: `tests/model_eval.rs`

- [ ] **Step 1: Write the test**

Duplicate the structure of `test_hard_model_comparison` but use `generate_enriched_nl` instead of `generate_nl_description`:

```rust
#[test]
#[ignore] // Requires model files
fn test_hard_with_summaries() {
    let embedder = Embedder::new().unwrap();
    let summaries = load_fixture_summaries();
    if summaries.is_empty() {
        eprintln!("WARNING: No fixture summaries found, skipping enriched eval");
        return;
    }

    // Parse fixtures (same as test_hard_model_comparison)
    let parser = cqs::parser::Parser::new().unwrap();
    let mut chunks: Vec<ChunkDesc> = Vec::new();

    for lang in &HARD_EVAL_LANGUAGES {
        let path = hard_fixture_path(*lang);
        if !path.exists() { continue; }
        let parsed = parser.parse_file(&path).unwrap();
        for chunk in &parsed {
            let summary = summaries.get(&chunk.name);
            let nl_text = generate_enriched_nl(chunk, summary.map(|s| s.as_str()));
            chunks.push(ChunkDesc {
                name: chunk.name.clone(),
                language: *lang,
                nl_text,
            });
        }
    }

    // Also load original fixtures for base comparison
    // ... (same pattern as test_hard_model_comparison)

    // Embed and score
    let texts: Vec<&str> = chunks.iter().map(|c| c.nl_text.as_str()).collect();
    let embeddings = embedder.embed_documents(&texts).unwrap();

    // Score against HARD_EVAL_CASES
    let mut hits_at_1 = 0;
    let mut hits_at_5 = 0;
    let mut total = 0;

    for case in eval_common::HARD_EVAL_CASES {
        // ... same scoring logic as test_hard_model_comparison
        // but against enriched embeddings
    }

    let r1 = hits_at_1 as f64 / total as f64 * 100.0;
    let r5 = hits_at_5 as f64 / total as f64 * 100.0;
    eprintln!("  With summaries:");
    eprintln!("  Recall@1: {hits_at_1}/{total} ({r1:.1}%)");
    eprintln!("  Recall@5: {hits_at_5}/{total} ({r5:.1}%)");

    // Should be >= raw hard eval (89.1%)
    assert!(r1 >= 85.0, "Enriched R@1 {r1:.1}% should be >= 85%");
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test --features gpu-index --test model_eval -- test_hard_with_summaries --ignored --nocapture 2>&1 | tail -10
```

- [ ] **Step 3: Compare raw vs enriched**

```bash
cargo test --features gpu-index --test model_eval -- test_hard --ignored --nocapture 2>&1 | grep "Recall@1"
```

Expected: enriched R@1 > raw R@1 (92%+ vs 89.1%).

- [ ] **Step 4: Commit**

```bash
git commit -m "test: hard eval with contrastive summaries — measures full pipeline quality"
```

---

### Task 4: Add summary coverage reporting

**Files:**
- Modify: `tests/model_eval.rs`

- [ ] **Step 1: Add coverage stats to test output**

In `test_hard_with_summaries`, after loading summaries:

```rust
let with_summary = chunks.iter().filter(|c| summaries.contains_key(&c.name)).count();
let without = chunks.len() - with_summary;
eprintln!("  Chunks: {} total, {} with summaries, {} without", chunks.len(), with_summary, without);
```

This helps diagnose whether missing summaries are causing lower scores.

- [ ] **Step 2: Run and verify output**

```bash
cargo test --features gpu-index --test model_eval -- test_hard_with_summaries --ignored --nocapture 2>&1 | grep -E "Chunks|Recall"
```

- [ ] **Step 3: Commit**

```bash
git commit -m "test: add summary coverage reporting to enriched hard eval"
```

---

## Estimated effort

| Task | Time | Notes |
|------|------|-------|
| 1. Generate fixture summaries | 10 min | One-time Python script against live index |
| 2. Summary loading helpers | 10 min | Two small functions |
| 3. Enriched eval test | 20 min | Based on existing test structure |
| 4. Coverage reporting | 5 min | One-liner |
| **Total** | **~45 min** | |

## Expected impact

- **Raw hard eval** — unchanged (89.1% R@1). Kept as baseline for model quality.
- **Enriched hard eval** — should be ~90-93% R@1, closing the gap with full-pipeline eval (92.7%).
- **CI** — both tests are `#[ignore]` (require model files). Run manually during development.

## What this does NOT include

- **Call context enrichment** — fixtures don't have a call graph. Only summaries are injected.
- **HyDE query enrichment** — separate from summaries, would need its own fixture file.
- **Automated summary regeneration** — fixture file is committed once. Regenerate manually when adding new fixtures or changing the contrastive prompt.

---

### Task 5: Node.js 24 CI migration

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Update checkout action**

In both workflow files, change `actions/checkout@v4` to `actions/checkout@v5` (Node.js 24 compatible).

```yaml
# Before:
- uses: actions/checkout@v4
# After:
- uses: actions/checkout@v5
```

Also update `dtolnay/rust-toolchain@stable` if a newer version exists.

- [ ] **Step 2: Commit**

```bash
git commit -m "ci: upgrade actions/checkout v4→v5 for Node.js 24 (June 2026 deadline)"
```

---

### Task 6: pymupdf4llm 1.x compatibility check

**Files:**
- Modify: `scripts/pdf_to_md.py` (if API changed)
- Modify: `src/convert/pdf.rs` (if CLI interface changed)

- [ ] **Step 1: Check pymupdf4llm 1.x changelog**

```bash
pip install pymupdf4llm==1.27.2.2
python3 -c "import pymupdf4llm; print(dir(pymupdf4llm))" 
# Compare with 0.2.9 API
```

- [ ] **Step 2: Test PDF conversion**

```bash
# Find a test PDF
cqs convert some_doc.pdf --output /tmp/test_convert/
# Verify output
```

- [ ] **Step 3: Fix any API changes, commit**

```bash
git commit -m "deps: upgrade pymupdf4llm 0.2.9→1.27.2 with API compatibility fixes"
```
