# KeyDAC-Style Query Augmentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Augment training data with keyword-preserving query rewrites. For each (query, code) pair, generate additional (augmented_query, code) pairs where important keywords are preserved but surrounding words are modified. This teaches the model robustness to query phrasing variations. Free — no API costs.

**Architecture:** Python script that reads training JSONL, identifies keywords per query (via code component matching), applies augmentation operations while preserving keywords, outputs augmented JSONL. Run before `train_lora.py`.

**Tech Stack:** Python, NLTK or simple tokenization

**Reference:** KeyDAC (EACL 2023, Park et al.) — keyword-based data augmentation for contrastive code search.

---

## Background

KeyDAC's core insight: when augmenting code search queries, don't modify the words that carry search intent. "sort array by key" — "sort" and "key" are important (they match the function name and parameter). "array" and "by" are less important and can be varied.

**Keyword identification:** Match query tokens against code components:
- Function name tokens (tokenized: `merge_sort` → `merge`, `sort`)
- Parameter names (tokenized)
- Doc comment tokens (if available)
- Tokens that appear in the code but are not language stopwords

**Augmentation operations** (applied to non-keyword tokens only):
1. **Delete** — remove a non-keyword word: "sort array by key" → "sort by key"
2. **Swap** — swap two adjacent non-keyword words: "find files matching pattern" → "find matching files pattern"
3. **Synonym replace** — replace with a synonym: "check if valid" → "verify if valid"

Each original pair generates 2-3 augmented pairs. Keywords are never modified.

## Data Flow

```
combined_9lang_hard_negs.jsonl (1.89M pairs)
  ↓ subsample (200k, seed 42 — same as v7)
  ↓ augment_keydac.py (2-3x expansion)
  ↓ augmented_200k.jsonl (~500k pairs)
  ↓ train_lora.py --data augmented_200k.jsonl
```

---

### Task 1: Write keyword extraction

**Files:**
- Create: `~/training-data/augment_keydac.py`

- [ ] **Step 1: Implement `extract_keywords`**

```python
def extract_keywords(query: str, code: str) -> set[str]:
    """Identify query tokens that match code components."""
    query_tokens = set(tokenize(query.lower()))

    # Extract code components
    code_tokens = set()
    # Function/method names (camelCase and snake_case split)
    for token in re.findall(r'\b[a-zA-Z_]\w*\b', code):
        code_tokens.update(split_identifier(token))

    # Keywords = query tokens that appear in code components
    # Plus: tokens that are nouns/verbs (content words) based on simple heuristic
    keywords = query_tokens & code_tokens

    # If no overlap found (rare), treat all content words as keywords
    # to avoid destroying the query entirely
    if not keywords:
        keywords = {t for t in query_tokens if len(t) > 3}

    return keywords
```

- [ ] **Step 2: Test keyword extraction**

```python
def test_extract_keywords():
    assert "sort" in extract_keywords("sort array by key", "def sort_by_key(array, key):")
    assert "key" in extract_keywords("sort array by key", "def sort_by_key(array, key):")
    assert "array" in extract_keywords("sort array by key", "def sort_by_key(array, key):")
    # "by" should NOT be a keyword (not in code identifiers)
```

- [ ] **Step 3: Implement `split_identifier`**

```python
def split_identifier(name: str) -> list[str]:
    """Split camelCase and snake_case into tokens."""
    # snake_case
    if '_' in name:
        return [t.lower() for t in name.split('_') if t]
    # camelCase
    tokens = re.sub(r'([A-Z])', r' \1', name).split()
    return [t.lower() for t in tokens if t]
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat: keyword extraction for KeyDAC augmentation"
```

---

### Task 2: Write augmentation operations

**Files:**
- Modify: `~/training-data/augment_keydac.py`

- [ ] **Step 1: Implement `augment_delete`**

```python
def augment_delete(tokens: list[str], keywords: set[str]) -> str | None:
    """Remove one random non-keyword token."""
    non_kw_indices = [i for i, t in enumerate(tokens) if t.lower() not in keywords]
    if not non_kw_indices:
        return None  # All tokens are keywords, can't delete
    idx = random.choice(non_kw_indices)
    result = tokens[:idx] + tokens[idx+1:]
    return ' '.join(result) if result else None
```

- [ ] **Step 2: Implement `augment_swap`**

```python
def augment_swap(tokens: list[str], keywords: set[str]) -> str | None:
    """Swap two adjacent non-keyword tokens."""
    swappable = []
    for i in range(len(tokens) - 1):
        if tokens[i].lower() not in keywords and tokens[i+1].lower() not in keywords:
            swappable.append(i)
    if not swappable:
        return None
    idx = random.choice(swappable)
    result = list(tokens)
    result[idx], result[idx+1] = result[idx+1], result[idx]
    return ' '.join(result)
```

- [ ] **Step 3: Implement `augment_synonym`**

```python
# Simple synonym table — no external dependency needed
SYNONYMS = {
    "find": ["search", "locate", "look up"],
    "get": ["retrieve", "fetch", "obtain"],
    "check": ["verify", "validate", "test"],
    "create": ["make", "build", "generate"],
    "remove": ["delete", "drop", "clear"],
    "update": ["modify", "change", "set"],
    "convert": ["transform", "parse", "map"],
    "sort": ["order", "arrange", "rank"],
    "filter": ["select", "extract", "pick"],
    "count": ["tally", "enumerate", "total"],
    "list": ["enumerate", "show", "display"],
    "read": ["load", "parse", "open"],
    "write": ["save", "store", "output"],
    "split": ["divide", "separate", "partition"],
    "merge": ["combine", "join", "concatenate"],
    "compare": ["diff", "match", "contrast"],
    # Add more as needed
}

def augment_synonym(tokens: list[str], keywords: set[str]) -> str | None:
    """Replace one non-keyword token with a synonym."""
    replaceable = [(i, t) for i, t in enumerate(tokens)
                   if t.lower() not in keywords and t.lower() in SYNONYMS]
    if not replaceable:
        return None
    idx, token = random.choice(replaceable)
    syn = random.choice(SYNONYMS[token.lower()])
    result = list(tokens)
    result[idx] = syn
    return ' '.join(result)
```

- [ ] **Step 4: Test augmentation operations**

```python
def test_augment_delete():
    tokens = ["sort", "array", "by", "key"]
    keywords = {"sort", "key"}
    result = augment_delete(tokens, keywords)
    assert "sort" in result and "key" in result
    assert len(result.split()) == 3  # one token removed

def test_augment_swap():
    tokens = ["find", "all", "matching", "files"]
    keywords = {"find", "files"}
    result = augment_swap(tokens, keywords)
    assert "find" in result and "files" in result

def test_augment_all_keywords_returns_none():
    tokens = ["sort", "key"]
    keywords = {"sort", "key"}
    assert augment_delete(tokens, keywords) is None
```

- [ ] **Step 5: Commit**

```bash
git commit -m "feat: KeyDAC augmentation operations (delete, swap, synonym)"
```

---

### Task 3: Write the main augmentation pipeline

**Files:**
- Modify: `~/training-data/augment_keydac.py`

- [ ] **Step 1: Implement main pipeline**

```python
def augment_pair(query: str, code: str, num_augments: int = 2) -> list[str]:
    """Generate augmented queries for a (query, code) pair."""
    keywords = extract_keywords(query, code)
    tokens = query.split()

    if len(tokens) < 3:
        return []  # Too short to augment meaningfully

    augmented = []
    ops = [augment_delete, augment_swap, augment_synonym]

    attempts = 0
    while len(augmented) < num_augments and attempts < num_augments * 3:
        op = random.choice(ops)
        result = op(tokens, keywords)
        if result and result != query and result not in augmented:
            augmented.append(result)
        attempts += 1

    return augmented

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--max-samples", type=int, default=200000)
    parser.add_argument("--augments-per-pair", type=int, default=2)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()

    random.seed(args.seed)

    original = 0
    augmented_count = 0

    with open(args.input) as fin, open(args.output, 'w') as fout:
        for line in fin:
            d = json.loads(line)

            # Write original pair
            fout.write(line)
            original += 1

            # Generate augmented queries
            aug_queries = augment_pair(d["query"], d["positive"], args.augments_per_pair)
            for aug_q in aug_queries:
                aug_record = dict(d)
                aug_record["query"] = aug_q
                fout.write(json.dumps(aug_record) + "\n")
                augmented_count += 1

            if original >= args.max_samples:
                break

    print(f"Original: {original}, Augmented: {augmented_count}")
    print(f"Total: {original + augmented_count} pairs -> {args.output}")
```

- [ ] **Step 2: Test end-to-end**

```bash
# Quick test on 100 pairs
python augment_keydac.py --input combined_9lang_hard_negs.jsonl --output test_aug.jsonl --max-samples 100
wc -l test_aug.jsonl  # Should be ~300 (100 original + ~200 augmented)
head -5 test_aug.jsonl  # Inspect quality
```

- [ ] **Step 3: Add progress reporting and stats**

```python
# Every 10k pairs, print progress
# At end, print per-operation counts and keyword hit rate
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat: KeyDAC augmentation pipeline for training data"
```

---

### Task 4: Generate augmented training data

**Files:**
- No code changes — run the script

- [ ] **Step 1: Generate augmented dataset**

```bash
python augment_keydac.py \
  --input combined_9lang_hard_negs.jsonl \
  --output augmented_200k_keydac.jsonl \
  --max-samples 200000 \
  --augments-per-pair 2
```

Expected: ~600k pairs (200k original + ~400k augmented). Some pairs produce 0-1 augments (short queries, all keywords).

- [ ] **Step 2: Inspect quality**

```bash
# Check augmented queries look reasonable
python -c "
import json, random
lines = open('augmented_200k_keydac.jsonl').readlines()
random.seed(42)
for line in random.sample(lines, 10):
    d = json.loads(line)
    print(f'  {d[\"query\"][:80]}')
"
```

- [ ] **Step 3: Push to cqs-training repo**

```bash
git add augment_keydac.py
git commit -m "feat: KeyDAC query augmentation script"
git push
```

---

### Task 5: Train v8 with augmented data

**Files:**
- No new code — use existing `train_lora.py`

- [ ] **Step 1: Train**

```bash
python train_lora.py \
  --data augmented_200k_keydac.jsonl \
  --output ./e5-code-search-lora-v8-keydac \
  --epochs 1 --batch-size 32 \
  --use-gist --matryoshka --export-onnx
```

ETA: ~14-21 hours on A6000 (600k pairs with GIST at ~3.5s/step).

- [ ] **Step 2: Export opset-11 ONNX**

The updated `train_lora.py --export-onnx` now handles this automatically (weight injection into opset-11 template).

- [ ] **Step 3: Add v8-keydac to eval harness**

Add `lora-v8-keydac` to `run_coir.py` and `local_lora_models()` in `model_eval.rs`.

---

### Task 6: Evaluate

- [ ] **Step 1: Hard eval (raw)**

```bash
cargo test --features gpu-index --test model_eval --release -- test_hard_model_comparison --nocapture --ignored
```

Compare v8-keydac vs v7 vs base.

- [ ] **Step 2: CoIR (full 9 tasks)**

```bash
python run_coir.py --model lora-v8-keydac --all
```

Compare overall, CSN, and CosQA transfer.

- [ ] **Step 3: Update research log, roadmap, model card if improved**

---

## Estimated effort

| Task | Time | Notes |
|------|------|-------|
| 1. Keyword extraction | 20 min | Simple token matching |
| 2. Augmentation ops | 20 min | Delete/swap/synonym |
| 3. Main pipeline | 15 min | JSONL reader/writer |
| 4. Generate data | 5 min | ~200k pairs, fast |
| 5. Train v8 | 14-21 hours | GPU time, unattended |
| 6. Evaluate | 1-2 hours | Hard eval + CoIR |
| **Total coding** | **~1 hour** | |
| **Total wall time** | **~16-23 hours** | Dominated by training |

## Expected impact

- **CosQA transfer** — most likely to improve. CosQA queries are real web searches (terse, varied phrasing). Augmentation teaches robustness to phrasing variations.
- **CSN** — moderate improvement. CSN queries are commit messages/docstrings (already well-formed). Less benefit from augmentation.
- **Hard eval** — unlikely to change. Hard eval tests semantic discrimination, not query phrasing robustness.

## Risks

- **Quality degradation** — if augmentations are too aggressive (too many deletions, bad synonyms), the model learns noisy query→code mappings. Mitigation: preserve keywords, limit to 2 augments per pair, inspect sample quality.
- **Training time increase** — 3x data = 3x training time with GIST. Could subsample augmented set to 200k total (66k original + 133k augmented) to keep training time constant.
- **Synonym table gaps** — the hardcoded synonym table won't cover all code search vocabulary. Can expand iteratively. Missing synonyms just mean fewer augmentations, not wrong ones.

## What this does NOT include

- **Code-side augmentation** — only queries are augmented. Code passages stay unchanged (must match CoIR's raw-code evaluation).
- **LLM-based augmentation** — KeyDAC is purely mechanical (no API costs). LLM discriminating summaries are a separate, complementary approach ($38).
- **Contrastive neighbor context** — that's the separate contrastive summaries plan. KeyDAC and contrastive summaries are independent and can stack.
