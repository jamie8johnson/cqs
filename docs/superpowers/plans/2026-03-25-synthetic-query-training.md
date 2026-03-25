# Synthetic Query Training Data (v9) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate synthetic training pairs using LLM-generated search queries and multi-style docstrings, following the Qodo-Embed-1 methodology that achieved 68.53 on CoIR with a 1.5B model. Stack with KeyDAC augmentation for v9 training.

**Architecture:** Three-phase data pipeline: (1) harvest existing HyDE predictions from cqs index as free training pairs, (2) generate multi-style docstrings per function via Haiku batch, (3) generate developer search queries per function. Mix all synthetic pairs with existing 200k CSN+Stack data.

**Tech Stack:** Python, Claude Batches API (Haiku), cqs index DB

**Reference:** Qodo-Embed-1 blog (2025-02-27), Tal Sheffer. Key insight: synthetic data for code embedding > model scale. 1.5B beats 7B through better training data.

---

## Background: The Qodo Method

Qodo's pipeline:
1. Scrape open-source code from GitHub, quality-filter
2. **Docstring generation** — for undocumented functions, generate multiple docstring styles (formatted docs → concise NL). Prompt: Google-style docstring with params, returns, exceptions
3. **Code query generation** — generate 10-30 word NL search queries per function. Prompt: "brief and concise natural-language search query that developers could use to find similar code solutions"
4. Train embedding model on (docstring, code) + (query, code) pairs

**What we already have:**
- `--improve-docs` generates full doc comments (one style per function)
- HyDE pass generates 3-5 search queries per function (stored in `llm_summaries` with `purpose = "hyde"`)
- Contrastive summaries generate one-sentence discriminating descriptions
- All cached in SQLite by `content_hash`

**What's new:**
- Multi-style docstrings (formal, concise, question-form) — 3 per function
- Use existing HyDE predictions as training pairs (free)
- Use contrastive summaries as training pairs (free)
- Mix into training data alongside CSN+Stack

---

## Data Sources

| Source | Pairs | Cost | Notes |
|--------|-------|------|-------|
| CSN + Stack (existing) | 200k | $0 | Subsample from 1.89M |
| KeyDAC augmentation (existing) | 243k | $0 | Already generated |
| HyDE predictions (existing) | ~8k | $0 | 2635 functions × 3 queries avg |
| Contrastive summaries (existing) | ~2.6k | $0 | Already in llm_summaries |
| Multi-style docstrings (new) | ~8k | ~$1.14 | 3 styles × 2635 functions |
| Synthetic search queries (new) | ~5k | ~$0.38 | 2 queries × 2635 functions |
| **Total** | **~467k** | **~$1.52** | |

---

### Task 1: Harvest existing HyDE + summary training pairs

**Files:**
- Create: `~/training-data/harvest_cqs_training_pairs.py`

- [ ] **Step 1: Write the harvester**

```python
"""Extract (text, code) training pairs from cqs index DB.

Sources:
- HyDE predictions (purpose='hyde'): each is 3-5 search queries
- Contrastive summaries (purpose='summary'): one-sentence descriptions
- Doc comments (from chunk.doc field): existing documentation

For each source, creates (text, code) pairs in JSONL format matching
the CSN training data schema: {"query": "...", "positive": "..."}
"""
```

Extract from SQLite:
```sql
-- HyDE: split multi-line predictions into individual queries
SELECT c.content, s.summary FROM chunks c
JOIN llm_summaries s ON c.content_hash = s.content_hash
WHERE s.purpose = 'hyde' AND c.chunk_type IN (callable types)

-- Summaries: one-sentence descriptions
SELECT c.content, s.summary FROM chunks c
JOIN llm_summaries s ON c.content_hash = s.content_hash
WHERE s.purpose = 'summary' AND s.model = 'claude-haiku-4-5'
```

For HyDE: split each prediction into individual lines (each is a search query).
For summaries: use as-is (one pair per function).

- [ ] **Step 2: Test on live cqs index**

```bash
python3 harvest_cqs_training_pairs.py --db /mnt/c/Projects/cqs/.cqs/index.db --output cqs_pairs.jsonl
wc -l cqs_pairs.jsonl  # Expected: ~10k
head -3 cqs_pairs.jsonl  # Inspect
```

- [ ] **Step 3: Commit**

---

### Task 2: Generate multi-style docstrings

**Files:**
- Create: `~/training-data/generate_multi_docstrings.py`

- [ ] **Step 1: Write the generator**

Three styles per function (following Qodo):

**Style 1: Formal docstring** (Google-style, params + returns + raises)
```
Generate a detailed docstring for this function. Include: description,
parameters with types, return value with type, exceptions. Use
Google-style format. Be specific. Provide only the docstring.
```

**Style 2: Concise NL summary** (already have this — contrastive summaries)
Skip — reuse existing summaries from Task 1.

**Style 3: Question-form** (what problem does this solve?)
```
Write a single question that this function answers. The question should
be what a developer would ask before writing this code. Example:
"How do I validate an email address with regex in Python?"
Provide only the question, nothing else.
```

So really just 1 new style (formal docstring) + 1 new style (question-form).
Contrastive summary is the 3rd style (already exists).

Batch via Claude Batches API (Haiku). ~$0.76 for 2 styles × 2635 functions.

- [ ] **Step 2: Submit batch, fetch results, write JSONL**

- [ ] **Step 3: Commit**

---

### Task 3: Generate synthetic search queries

**Files:**
- Modify: `~/training-data/generate_multi_docstrings.py` (add query generation)

- [ ] **Step 1: Add Qodo-style query generation prompt**

```
You are a query generator. Produce ONLY a brief natural-language search
query (10-30 words) that a developer would use to find this code.
Use common programming terminology. Capture the core functionality.

Examples of good queries:
- "function that retries HTTP requests with exponential backoff"
- "parse CSV file and return list of dictionaries"
- "validate JWT token and check expiration date"

{code}
```

2 queries per function (different phrasing). ~$0.38 for 2635 functions.

- [ ] **Step 2: Submit batch, fetch results, write JSONL**

- [ ] **Step 3: Commit**

---

### Task 4: Mix all sources and train v9

**Files:**
- Create: `~/training-data/mix_synthetic.py`

- [ ] **Step 1: Combine all training data**

```python
# Priority order:
# 1. CSN + Stack (200k, real pairs, highest quality)
# 2. KeyDAC augmented (243k, keyword-preserving rewrites)
# 3. HyDE predictions (~8k, model-generated queries)
# 4. Contrastive summaries (~2.6k, discriminating descriptions)
# 5. Multi-style docstrings (~5k, formal + question-form)
# 6. Synthetic queries (~5k, Qodo-style search queries)
#
# Shuffle with seed 42, output to combined_v9_synthetic.jsonl
```

- [ ] **Step 2: Add curriculum hard negative scheduling to train_lora.py**

Based on CoRNStack (ICLR 2025) and NV-Retriever findings. Prevents model collapse
on adversarial negatives — may fix the v7→v7b R@1 degradation pattern.

```python
# In train_lora.py, modify the data collator or training loop:
#
# Phase 1 (0-30% of training): in-batch random negatives only
#   - Set hard_negative_weight = 0.0
#   - Model learns coarse code vs non-code distinction
#
# Phase 2 (30-70%): introduce top-50 hard negatives
#   - Ramp hard_negative_weight from 0.0 to 0.5
#   - Model learns fine-grained code discrimination
#
# Phase 3 (70-100%): full hard negatives
#   - hard_negative_weight = 1.0
#   - Model polishes on hardest examples
#
# Implementation: custom callback that adjusts the loss weighting
# or filters the negative pool based on training progress.
```

- [ ] **Step 3: Train v9**

```bash
python3 train_lora.py \
  --data combined_v9_synthetic.jsonl \
  --output ./e5-code-search-lora-v9-synthetic \
  --epochs 1 --batch-size 32 \
  --use-gist --matryoshka --export-onnx \
  --curriculum  # new flag for progressive hard negative schedule
```

- [ ] **Step 4: Evaluate (hard eval 3x median + CoIR)**

---

## Estimated effort

| Task | Time | Cost |
|------|------|------|
| 1. Harvest existing pairs | 20 min | $0 |
| 2. Multi-style docstrings | 30 min + batch wait | ~$0.76 |
| 3. Synthetic queries | 15 min + batch wait | ~$0.38 |
| 4. Mix + train | 30 min + 14-21h training | $0 |
| **Total coding** | **~1.5h** | **~$1.14** |
| **Total wall time** | **~16-22h** | (dominated by training) |

## Expected impact

- **CosQA** — most likely to improve. Synthetic queries resemble real web searches.
- **CSN** — moderate. Multi-style docstrings provide diverse positive pairs.
- **Hard eval** — unclear. More training data has degraded precision in prior experiments (v5→v7→v7b trend). Synthetic queries might help or hurt.

## Risks

- **Over-representation of cqs code** — 10k+ synthetic pairs from our 2635 functions could bias the model toward Rust patterns. Mitigation: cap cqs-derived pairs at 5% of total.
- **Quality of LLM-generated pairs** — Haiku query quality on non-English code may be poor. Mitigation: inspect samples, filter short/generic outputs.
- **Training time** — 467k pairs with GIST is ~21h on A6000. Could subsample to 300k to match v8 training time.

## Depends on

- v8-keydac eval results. If KeyDAC already saturates improvements, synthetic queries add less value.
- A6000 eval matrix sweep (all models, authoritative numbers).
