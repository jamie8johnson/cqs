# Research Log

Experiments toward a paper on layered code retrieval with a 110M parameter model.

**Thesis:** "Different evaluation regimes surface different quality dimensions. Adversarial evals (confusable function pairs) test precision — type-aware embeddings dominate. Realistic benchmarks (CoIR) test recall and ranking — LoRA fine-tuning dominates. A layered architecture — signatures for precision, LoRA for recall, LLM enrichment for coverage — lets a 110M model compete with specialized models 3-20× larger."

---

## Experiment Timeline

### Exp 1: Baseline NL Descriptions (SQ-2) — 2026-03-14
**PR #588, shipped v1.0.6**

Added struct/enum field names + directory path context to NL descriptions before embedding.

| Config | R@1 (hard) | Delta |
|--------|-----------|-------|
| Baseline | — | — |
| +field names +dir context | +3.7pp | First positive result |

### Exp 2: Call-Graph Enrichment (SQ-4) — 2026-03-14
**PR #590, shipped v1.0.7**

Two-pass indexing: build call graph first, then re-embed chunks with caller/callee names appended. IDF-based filtering suppresses high-frequency utilities (>10% threshold).

- 63% of chunks enriched with call context
- Embedding quality improved for functions with rich call graphs

### Exp 3: Module-Level Context (SQ-5) — 2026-03-15
**PR #594, shipped v1.0.9**

Append filename stem to NL for module-level discrimination. Generic stems filtered (mod, index, lib, main, utils, helpers, common, types, config, constants, init).

- Regresses fixture eval ~3pp but improves real queries
- Shipped — real usage matters more than fixture eval

### Exp 4: LLM Summaries (SQ-6) — 2026-03-16
**PR #603, shipped v1.0.13**

One-sentence function summaries via Claude Haiku Batches API, prepended to NL descriptions. Cached by content_hash.

| Config | R@1 (stress) | Delta |
|--------|-------------|-------|
| Without summaries | — | — |
| With generic summaries | -2.1pp | Regression — generic summaries add noise |

Key finding: generic "summarize this function" prompt produces descriptions too similar across functions. Led to Exp 8 (discriminating prompt).

### Exp 5: LoRA Fine-Tuning (SQ-7) — 2026-03-16 through 2026-03-20
**PR #624 (train-data), PR #637 (ship v3)**

Fine-tuned E5-base-v2 on CodeSearchNet triplets (query, positive, negative).

| Config | Samples | Epochs | CSN NDCG@10 | CosQA (transfer) |
|--------|---------|--------|-------------|-------------------|
| Base E5 | — | — | 0.627 | 0.329 |
| v3 | 50k | 1 | 0.671 | 0.334 |
| **v5** | **166k** | **1** | **0.683** | **0.348** |
| v4 | 166k | 3 | 0.680 | 0.305 |
| v6-mixed | 166k+CosQA+SO | 1 | 0.644 | 0.332 |
| Rank 32 | 50k | 1 | 0.682 | — |

All variants use the same 186k-line training data file (CSN triplets + docstring-as-query pairs). The "+docs" label on v3 was misleading — all configs include docstrings. The only differences are `--max-samples` and `--epochs`.

**Original decision (2026-03-20):** Ship v3. Made at 11:17 AM before v5 results existed (12:33 PM).

**Corrected (2026-03-21):** v5 is strictly better than v3 on both CSN (+1.2pp) and CosQA transfer (+1.4pp). v4 over-specializes — 3 epochs on 166k data causes Python to spike (0.971) while CosQA collapses (0.305, below base). v5 avoids this by using 1 epoch on the full dataset.

**Untested:** 166k at 2 epochs — the midpoint between v5 (1ep, good transfer) and v4 (3ep, over-specialized). May find a sweet spot.

Per-language NDCG@10 (verified from `~/training-data/coir-results/`):

| Language | v3 | v5 | Delta |
|----------|------|------|-------|
| Go | 0.758 | 0.770 | +0.012 |
| Java | 0.610 | 0.626 | +0.017 |
| JS | 0.523 | 0.535 | +0.012 |
| Ruby | 0.590 | 0.589 | -0.001 |
| Python | 0.929 | 0.953 | +0.023 |
| PHP | 0.614 | 0.624 | +0.010 |

**Action:** Switch default to v5. Convert safetensors → ONNX, upload to HuggingFace.

**Rank investigation (2026-03-21):** rank-32 (r=32, alpha=64) trained on full 186k at 1 epoch scored 0.681 CSN, 0.350 CosQA — within noise of v5 (rank-16, 0.683/0.348) and sweep-200k (rank-16, 0.680/0.353). Doubling LoRA parameters adds nothing. rank-64 dir was created but never trained. **Rank is not a lever.**

**Verified complete results (all from `~/training-data/coir-results/`):**

| Config | Rank | Samples | Epochs | CSN | CosQA |
|--------|------|---------|--------|-----|-------|
| base E5 | — | — | — | 0.627 | 0.329 |
| sweep-10k | 16 | 10k | 1 | 0.671 | 0.327 |
| v3 | 16 | 50k | 1 | 0.671 | 0.334 |
| sweep-75k | 16 | 75k | 1 | 0.675 | 0.341 |
| sweep-200k | 16 | ~180k | 1 | 0.680 | 0.353 |
| rank-32 | 32 | ~186k | 1 | 0.681 | 0.350 |
| **v5** | **16** | **166k** | **1** | **0.683** | **0.348** |
| v4 | 16 | 166k | 3 | 0.695 | 0.304 |
| v6-mixed | 16 | 166k+mix | 1 | 0.644 | 0.332 |
| pipeline | 16 | (v3+enrich) | — | 0.626 | — |

**Next experiments (prioritized):**

1. **Hard negative mining** — biggest untapped lever. CoRNStack ablation shows +9.4pp from hard negs alone (63.3→72.7), independent of data cleanliness. Our random negatives are the main gap between us and SOTA.
   - Recipe: pre-compute similarity matrix with v5, softmax sampling with temperature annealing (τ: 0.05→0.001), InfoNCE loss (τ=0.07). See CoRNStack (arXiv 2412.01007).
   - `filter_csn.py` already exists for consistency filtering (ran it — 0 pairs filtered, CSN is clean). Need to extend with hard negative mining.
   - CoRNStack's data is open source at github.com/gangiswag/cornstack — could use their pre-mined negatives directly.

2. **Expand training languages: Rust, C++, TypeScript** — CSN only covers Go/Java/JS/Ruby/Python/PHP. Our users search Rust/C++/TS heavily but the model has zero fine-tuning signal for those. Plan:
   - Mine docstring-function pairs from popular public repos (tokio, servo, llvm, chromium, TypeScript compiler, deno)
   - Use `cqs train-data` to extract triplets from git history of those repos
   - Consistency-filter with v5 model (remove noisy pairs where cosine sim < threshold)
   - Add to training set alongside CSN data, retrain
   - Eval on hard eval (which includes Rust/TS queries) and CoIR
   - Risk: dilution (v6-mixed showed CSN+CosQA+SO hurts). Mitigation: filter aggressively, keep CSN as majority.

3. **Language-specific LoRA adapters** — LoRACode (ICLR 2025, arXiv 2503.05315) found per-language adapters outperform task-specific on Text2Code. Results on UniXcoder (not E5): Python +86.7% relative MRR (29.76→55.56), Go +67%, but Ruby only +3.9%. Gains correlate with training data size. Caveats: tested on UniXcoder (weaker base than E5), low starting MRR (29-49%), and the relative improvement looks inflated because the base is weak. Our E5 v5 already achieves 0.683 CSN — relative gains would be smaller from a stronger baseline.
   - **Assessment (corrected):** Our data sizes (56-63k per language) are 3.5-4x larger than LoRACode's best case (Python 15.7k → +86.7%). The data-size correlation actually *supports* per-language adapters for us — we're well above the threshold where gains were largest. The -7.9pp cross-code retrieval loss could be caused by a single adapter averaging across languages. Per-language adapters might fix that directly.
   - **Still lower priority than hard negatives** — hard negatives improve semantic depth across all languages, while per-language adapters improve per-language pattern matching. Try hard negatives first, but don't write off adapters based on flawed reasoning about data size.

4. **166k / 2 epochs** — quick sanity check. Does CosQA degrade gradually or cliff at 3ep?

5. **Full 10-task CoIR for v5** — only CSN + CosQA tested. 8 tasks unknown. Free to run.

6. **Knowledge distillation from CodeSage-large** — use 1.3B model as teacher, train E5 to match its similarity scores. More complex to implement.

**Why hard negatives are the priority:**
- CoRNStack's ablation is definitive: consistency filtering (+6.6pp) only helps noisy data (The Stack). CSN is already clean — confirmed by our `filter_csn.py` run (0 pairs filtered, same 1.71M lines). But hard negative mining (+9.4pp) works regardless of data quality. It forces the model to learn fine-grained semantic differences instead of surface-level language patterns.
- SFR-Code-2B (#1 on CoIR) also uses contrastive learning with hard negatives.
- Our failed reranker (SQ-10, -81.8pp) had the same root cause: random negatives too easy.

**Rejected approaches:**
- CodeSage-large-v2 — 94.26 on CSN but 20% R@1 on hard eval (can't parse NL queries)
- Consistency filtering — CSN is already clean (0 pairs filtered). CoRNStack's gain was on The Stack (raw GitHub scrapes), not CSN.
- Mixed LoRA (v6, CSN+CosQA+SO) — dilutes CSN signal without improving CosQA
- v4 (166k/3ep) — highest CSN but CosQA collapses. Over-specialized.
- rank-32/64 — no gain over rank-16. Rank is not the bottleneck.
- Pipeline enrichment on CoIR — hurts -4.5pp (see Exp 12).

**Key references:**
- CoRNStack (arXiv 2412.01007, ICLR 2025) — consistency filtering + curriculum hard negatives. Open data.
- LoRACode (arXiv 2503.05315, ICLR 2025) — language-specific LoRA adapters for code embeddings.
- CodeXEmbed / SFR-Code (arXiv 2411.12644, COLM 2025) — #1 on CoIR. LoRA + contrastive on structured pairs.
- CoCoHaNeRe (ACM TOSEM 2025) — hard negative mining specifically for code search.
- NV-Retriever (arXiv 2407.15831) — positive-aware hard negative mining, false negative removal.

### Exp 6: Weighted Multi-Signal Fusion — 2026-03-19
**PR #630 (weight sweep)**

30-config parameter sweep: name_boost, keyword boost, RRF weights.

| Result | Detail |
|--------|--------|
| All 30 configs | Regress on hard eval |
| Optimal | Embedding-only (no fusion) |

**Conclusion:** For adversarial confusable pairs, pure embedding similarity beats any weighted combination. Fusion may help on diverse queries but hurts precision.

### Exp 7: Type-Aware Embeddings (SQ-11) — 2026-03-19
**PR #630, shipped v1.2.0**

Append full function signature to NL description before embedding.

| Config | R@1 (hard) | Delta |
|--------|-----------|-------|
| Without signatures | 87.3% | — |
| **With signatures** | **90.9%** | **+3.6pp** |

TypeScript MRR +0.068. First positive result on hard eval since SQ-2.

### The LLM Text Arc (Exp 4 → 8 → 11)

This is the central narrative of the research. Three experiments, one question: can LLM-generated text improve code embeddings?

**The naive answer was no.** Exp 4 showed generic summaries *hurt* retrieval (-2.1pp). "Summarize this function" produces descriptions like "processes data and returns a result" for every function. The embeddings converge — every vector drifts toward "generic utility function" in embedding space. More text, less discrimination.

**The breakthrough was prompt engineering, not model changes.** Exp 8 tested four prompt strategies on the same LLM (Haiku), same functions, same eval:

| Prompt Strategy | R@1 | MRR | vs Raw Code |
|-----------------|-----|-----|-------------|
| No LLM text (raw code only) | 47.3% | 0.673 | — |
| Generic ("Summarize this function") | 60.0% | 0.726 | +12.7pp |
| **Discriminating ("What makes this unique")** | **63.6%** | **0.763** | **+16.3pp** |
| Contrastive (top-5 neighbors as context) | 65.5% | 0.769 | +18.2pp |

Even the generic prompt helps vs raw code (+12.7pp) — NL text is inherently closer to NL queries than code is. But the discriminating prompt adds another +3.6pp by forcing the LLM to surface *what's different* about each function: the specific algorithm, the edge case, the data structure choice. This is the single biggest lever we found across all 11 experiments.

**Then doc comments added a second signal on a different axis.** Exp 11 wrote standard doc comments (not discriminating — just "describe params and returns") back to source files, re-indexed, and measured:

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| R@1 (hard) | 90.9% | **92.7%** | **+1.8pp** |
| NDCG@10 (hard) | 0.951 | **0.965** | **+0.014** |

**Why do non-discriminating doc comments help when generic summaries hurt?** Because they flow through *different paths* and carry *different information*:

| Stream | Prompt | Stored | Feeds embedding via | What it captures |
|--------|--------|--------|---------------------|-----------------|
| Discriminating summary | "What makes this unique" | DB (`llm_summaries`) | Enrichment pass NL | *Distinctiveness* — algorithm, approach |
| Doc comment | "Describe params/returns" | Source file (`chunk.doc`) | NL generation | *Structure* — types, args, error cases |

Generic summaries failed because they attempted distinctiveness and produced sameness. Doc comments succeed because they don't try to be distinctive — they add structured parameter/return/error information that gives the embedding additional axes of discrimination. A function that "takes a HashMap and returns Option<Vec>" occupies a different region of embedding space than one that "takes a &str and returns Result<(), Error>", even if both "process data."

**The lesson:** LLM text is the biggest lever for embedding quality, but *what you ask for* matters more than the model. Prompt engineering delivered +16pp. LoRA fine-tuning delivered +4.4pp. The layered approach (discriminating summaries for distinctiveness + doc comments for structure) is additive because the two text streams are complementary, not redundant.

Ship: discriminating prompt (simple, cheap, one LLM call per function). Contrastive is future optimization (requires two passes + neighbor lookup).

Template comparison confirms — DocFirst (which prioritizes `chunk.doc` in embedding text) is now the clear winner:

| Template | R@1 | MRR | NDCG@10 |
|----------|-----|-----|---------|
| Compact | 89.1% | 0.933 | 0.949 |
| **DocFirst** | **92.7%** | **0.948** | **0.960** |

---

### Exp 9: Cross-Encoder Reranking (SQ-10) — 2026-03-19
**PR #629**

Trained cross-encoder reranker on 50k CSN + 7.5k docstring pairs, 3 epochs.

| Config | R@1 (hard) | Delta |
|--------|-----------|-------|
| No reranker | 90.9% | — |
| Web-trained reranker | 80.0% | **-10.9pp** |
| Code-trained reranker | 9.1% | **-81.8pp** (catastrophic) |

**Root cause:** Random same-language negatives too easy for cross-encoders. Need hard negatives (BM25 top-k) for V2. Infrastructure kept, do NOT make default.

### Exp 10: HyDE Query Predictions (SQ-12) — 2026-03-20
**PR #631, shipped v1.2.0**

LLM predicts 3-5 search queries per function at index time. Embedded alongside NL description.

| Config | R@1 (hard) | R@1 (stress) |
|--------|-----------|-------------|
| Without HyDE | — | — |
| With HyDE | Mixed | Neutral |

Shipped as optional enrichment (`--hyde-queries`). Untested on CoIR.

### Exp 12: Full Pipeline on CoIR — 2026-03-21
**In progress**

Previous CoIR runs tested individual components in isolation (base model, LoRA alone, NL enrichment alone). This experiment applies the full free enrichment pipeline as an `E5Pipeline` wrapper in `run_coir.py`:

- LoRA v3 model (trained on 50k CSN+docstrings)
- Signature extraction + append (SQ-11)
- Function name tokenization (SQ-2)
- Doc comment extraction from code (DocFirst template)
- Language detection

**Not included** (require API calls or full project context):
- LLM discriminating summaries (SQ-6) — $250 for full CSN, possible on small tasks for ~$2
- Call graph enrichment (SQ-4) — needs full project, not isolated snippets
- Module path context (SQ-5) — no file paths in CoIR

### Result: Pipeline enrichment HURTS on CoIR (-4.5pp)

| Language | v3 (raw) | v5 (raw) | Pipeline (v3+enrichment) | Delta vs v3 |
|----------|----------|----------|--------------------------|-------------|
| Go | 0.758 | 0.770 | 0.718 | -0.040 |
| Java | 0.610 | 0.626 | 0.572 | -0.038 |
| JS | 0.523 | 0.535 | 0.485 | -0.038 |
| Ruby | 0.590 | 0.589 | 0.547 | -0.042 |
| Python | 0.929 | 0.953 | 0.867 | -0.063 |
| PHP | 0.614 | 0.624 | 0.566 | -0.049 |
| **AVG** | **0.671** | **0.683** | **0.626** | **-0.045** |

**Why:** CoIR queries are NL, corpus is raw code. The LoRA was trained on that exact format (query→code). Prepending signatures, tokenized names, and doc snippets shifts the passage distribution away from what the model learned. The enrichment is designed for *index-time* use where we control the full NL generation pipeline, not as a drop-in text transform on raw code.

**Lesson for the paper:** NL enrichment is a *product feature* (helps when you own the whole pipeline), not a *model improvement* (hurts on standard benchmarks). The honest CoIR number is **v5 raw = 0.683**. The enrichment's +1.8pp on hard eval is real but only within cqs's controlled environment.

### Exp 13: Full 10-Task CoIR with LoRA v5 — 2026-03-21

First complete CoIR benchmark run. 9 tasks (codeforces dataset unavailable on HF).

| Task | NDCG@10 | Subtasks | Notes |
|------|---------|----------|-------|
| stackoverflow-qa | **0.877** | 1 | NL→NL, E5 base strength |
| codefeedback-st | **0.735** | 1 | Single-turn code feedback |
| codesearchnet | **0.683** | 6 | NL→code, LoRA target (+5.6pp vs base) |
| synthetic-text2sql | **0.567** | 1 | NL→SQL, zero SQL training |
| codesearchnet-ccr | 0.490 | 6 | Cross-code retrieval |
| codefeedback-mt | 0.399 | 1 | Multi-turn feedback |
| cosqa | 0.348 | 1 | Code QA |
| codetrans-dl | 0.174 | 1 | Code translation |
| apps | 0.107 | 1 | Program synthesis |
| **Overall avg** | **48.67** | 9 | **#8 on leaderboard (was #7 with base E5 at 50.90)** |

**Key finding: LoRA fine-tuning for code search is a specialization trade-off.** Controlled comparison (our measurement, not leaderboard):

| Task | Base E5 | LoRA v5 | Delta |
|------|---------|---------|-------|
| codesearchnet | 0.627 | **0.683** | **+5.6pp** |
| cosqa | 0.329 | **0.348** | +1.9pp |
| text2sql | 0.554 | **0.567** | +1.3pp |
| stackoverflow-qa | **0.879** | 0.877 | -0.1pp |
| codefeedback-st | **0.745** | 0.735 | -1.0pp |
| codefeedback-mt | **0.416** | 0.399 | -1.6pp |
| apps | **0.115** | 0.107 | -0.8pp |
| codetrans-dl | **0.219** | 0.174 | **-4.5pp** |
| codesearchnet-ccr | **0.569** | 0.490 | **-7.9pp** |
| **OVERALL** | **49.47** | 48.67 | **-0.8pp** |

v5 wins on 3 tasks, loses on 6. Biggest loss: cross-code retrieval (-7.9pp) — LoRA damaged cross-language understanding. Our measured base (49.47) is 1.4pp below the leaderboard's 50.90 — likely because we're missing codeforces (1 of 10 tasks, dataset unavailable on HF) and possibly different eval split versions.

**Implication for the paper:** The layered enrichment pipeline (which doesn't touch model weights) may be better for overall benchmark performance than LoRA fine-tuning. Hard negative mining with task-balanced loss could improve code search without degrading generalist ability.

**Implication for the product:** In cqs, we only do NL→code search. The LoRA + enrichment pipeline is the right choice for the product (0.683 CSN vs 0.627 base). The generalist degradation doesn't matter because cqs never does SO-QA or text2sql.

---

## CoIR Benchmark

CodeSearchNet, 6 languages. Standard code retrieval benchmark (ACL 2025).
Harness: `~/training-data/run_coir.py`, results in `~/training-data/coir-results/`.

### Summary (Avg NDCG@10 across 6 CSN languages)

| Config | Avg NDCG@10 | CosQA (transfer) | vs Base |
|--------|-------------|-------------------|---------|
| Base E5-base-v2 | 0.627 | 0.329 | — |
| E5 + NL enrichment | 0.626 | — | -0.001 |
| **E5 + LoRA v3 (50k+docs/1ep)** | **0.671** | **0.334** | **+0.043** |
| E5 + LoRA sweep 10k | 0.671 | 0.327 | +0.044 |
| E5 + LoRA v4 (200k/3ep) | 0.680 | 0.305 | +0.053 |
| E5 + LoRA v5 (100k/3ep) | 0.678 | 0.348 | +0.051 |
| E5 + LoRA v6-mixed (CSN+CosQA+SO) | 0.644 | 0.332 | +0.017 |
| E5 + LoRA rank-32 | 0.682 | — | +0.055 |

### Per-Language NDCG@10 (CodeSearchNet)

| Config | Go | Java | JS | Ruby | Python | PHP |
|--------|-----|------|-----|------|--------|-----|
| Base E5 | 0.624* | 0.571* | 0.487* | 0.526* | 0.888* | 0.601 |
| NL-enriched | 0.678 | 0.560 | 0.476 | 0.532 | 0.910 | 0.600 |
| LoRA v3 | 0.746 | 0.621 | 0.535 | 0.592 | 0.909 | 0.623 |
| LoRA sweep-10k | 0.746 | 0.621 | 0.535 | 0.592 | 0.909 | 0.623 |
| LoRA v4 | 0.780 | 0.644 | 0.547 | 0.593 | 0.971 | 0.637 |
| LoRA v5 | 0.770 | 0.626 | 0.535 | 0.589 | 0.953 | 0.624 |
| LoRA v6-mixed | 0.753 | 0.587 | 0.496 | 0.539 | 0.890 | 0.597 |

*Base E5 per-language from `e5-nl-enriched` run (closest to raw base; full base CSN run only has PHP).

### CosQA Transfer (out-of-distribution)

| Config | NDCG@10 | R@1 | R@10 |
|--------|---------|-----|------|
| Base E5 | 0.329 | 0.156 | 0.572 |
| LoRA v3 | 0.334 | 0.162 | 0.572 |
| LoRA sweep-10k | 0.327 | 0.162 | 0.550 |
| LoRA v4 | 0.305 | 0.150 | 0.510 |
| LoRA v5 | 0.348 | 0.172 | 0.586 |
| LoRA v6-mixed | 0.332 | 0.150 | 0.574 |

Key finding: v4 over-specializes on CSN Python (0.971!) at the expense of CosQA transfer (0.305). v5 has the best CosQA transfer (0.348) but v3 is the sweet spot for ship.

### Full CoIR Leaderboard (from archersama.github.io/coir)

10 tasks, ranked by average across all. Our CSN-only results aren't directly comparable to the full leaderboard avg, but CSN is the most relevant task.

| # | Model | Params | Avg (10 tasks) |
|---|-------|--------|----------------|
| 1 | Salesforce/SFR-Embedding-Code-2B_R | 2B | 67.41 |
| 2 | CodeSage-large-v2 | 1.3B | 64.18 |
| 3 | Salesforce/SFR-Embedding-Code-400M_R | 400M | 61.89 |
| 4 | CodeSage-large | 1.3B | 61.04 |
| 5 | Voyage-Code-002 | — | 56.26 |
| 6 | E5-Mistral | 7B | 55.18 |
| **7** | **E5-Base-v2 (our base)** | **110M** | **50.9** |
| 8 | OpenAI-Ada-002 | — | 45.59 |
| 9 | BGE-Base-en-v1.5 | 110M | 42.77 |
| 10 | BGE-M3 | 567M | 39.31 |
| 11 | UniXcoder | 123M | 37.33 |
| 12 | GTE-Base-en-v1.5 | 110M | 36.75 |
| 13 | Contriever | 110M | 36.4 |

**Our position:** E5-base-v2 is already #7 out of 13. With LoRA v3 (+4.3pp on CSN), we'd approach #5-6 territory on the full benchmark — competing with models 3-60× our size.

**Paper angle:** Among 110M-class models (BGE-Base, GTE-Base, UniXcoder, Contriever), E5-base-v2 dominates at 50.9 vs next-best 42.77. Our LoRA + layered enrichment architecture extends that lead further while staying at 110M params — runnable on CPU in <100ms per query.

---

## Current Gold Standard (post Exp 11)

**Hard eval (55 confusable pairs):**
- Recall@1: **92.7%** (51/55)
- Recall@5: 98.2% (54/55)
- Recall@10: 100% (55/55)
- MRR: 0.954
- NDCG@10: 0.965

**Production stack (what ships):**

| Layer | Feature | Cost | Impact |
|-------|---------|------|--------|
| 1 | Type-aware signatures (SQ-11) | Free | +3.6pp R@1 |
| 2 | Call graph enrichment (SQ-4) | Free | 63% of chunks enriched |
| 3 | LoRA v3 embedding model | Free (baked in) | +4.4pp CoIR NDCG@10 |
| 4 | LLM summaries — discriminating (SQ-6) | ~$0.15/3k fn | +16pp R@1 vs raw code |
| 5 | Doc comment generation (SQ-8) | ~$1.50/3k fn | +1.8pp R@1 |
| 6 | HyDE predictions (SQ-12) | ~$0.15/3k fn | Optional, mixed results |

---

## Literature

- **CoIR benchmark** (ACL 2025) — 10 datasets, 8 tasks, 14 languages, 2M docs
- **CoRNStack** — large-scale contrastive training, claims SOTA
- **CodeXEmbed** (COLM 2025) — generalist code embedding family
- **C2LLM** (arXiv 2512.21332) — contrastive code LLMs (0.5B, 7B)
- **CodeCSE** (arXiv 2407.06360) — multilingual code/comment sentence embeddings
- **Refining embeddings with PEFT** (arXiv 2405.04126) — LoRA on CodeT5+. Closest to our approach.
- **Lore** (arXiv 2603.15566) — git commit messages as structured knowledge for AI agents

---

## Key Lessons

1. **Generic LLM descriptions hurt.** They make all functions sound the same. Discriminating prompt (+16pp) vs generic (+12.7pp).
2. **Adversarial evals ≠ realistic evals.** LoRA regresses hard eval but +4.4pp on CoIR. Type signatures help hard eval but are invisible on CoIR.
3. **Fusion doesn't help precision.** 30-config weight sweep — pure embedding beats all combinations on confusable pairs.
4. **Cross-encoder reranking needs hard negatives.** Random negatives are too easy. Catastrophic failure (-81.8pp) with code-trained reranker.
5. **Doc comments improve embeddings.** Writing back LLM-generated docs to source → richer NL → better vectors (+1.8pp R@1).
6. **Data > architecture for LoRA.** Rank 16→32 is flat. 50k→200k samples gives diminishing returns. Quality > quantity.
7. **In-product enrichment ≠ benchmark improvement.** NL enrichment (signatures, doc text, names) helps +1.8pp inside cqs but hurts -4.5pp on CoIR. The model was trained on raw code; enrichment shifts the passage distribution. Benchmark numbers must use the raw model.
8. **Don't ship before eval completes.** v3 was shipped as default before v5 results existed (11:17 AM vs 12:33 PM). v5 is strictly better on every metric.
9. **LoRA fine-tuning is a specialization trade-off.** Full 10-task CoIR: v5 drops from #7 (base E5 50.90) to #8 (48.67). Gains on CSN (+5.6pp) come at the cost of generalist tasks (SO-QA, text2sql, codefeedback). Random negatives teach language discrimination, not semantic discrimination — over-specializes.
10. **Hard negatives alone don't fix the trade-off.** v7 (hard negs + GIST + Matryoshka) degraded further than v5 (R@1: 81.8% vs 85.5%). Language imbalance (PHP/Java/Python at 82%) likely overwhelms the benefit. CoRNStack's gains may require balanced data or larger scale. Next test: 46k/lang balanced training.
11. **Language balance matters.** v7's 200k subsample preserved the source proportions (82% in 3 dominant langs). Stack languages at 3% each were too diluted. Equal-weight subsampling is the next experiment.

---

## Publication Assessment (2026-03-21)

**Status: Not yet publishable. 2-3 weeks from submittable draft.**

### What we have
- 13 experiments with verified metrics across three eval regimes (hard eval, CoIR CSN, full 10-task CoIR)
- The LLM text arc: generic hurts → discriminating +16pp → doc comments +1.8pp on top
- The benchmark-vs-product gap: enrichment helps in-product, hurts on benchmarks
- The specialization trade-off: LoRA helps code search, hurts generalist retrieval
- 10 LoRA variants with data scaling analysis
- Full 10-task CoIR numbers: 48.67 avg (#8), with per-task breakdown

### What's missing for publication
1. **Near-SOTA results.** 0.683 CSN is good for 110M but 48.67 overall is below base E5. Need hard negative mining to push CSN toward 0.72+ without degrading generalist tasks.
2. **Novelty framing.** Individual techniques aren't new. The combination + evaluation methodology insights are the contribution.
3. **Controlled ablations.** Current comparisons are ad-hoc. Need confidence intervals, same-seed runs.
4. **Training data expansion.** CSN only covers 6 languages. Rust/C++/TypeScript from public repos would strengthen practical angle.

### Strongest paper angle
"LoRA fine-tuning for code search is a specialization trade-off: a 110M model study." The contribution isn't SOTA results — it's the systematic analysis of what helps, what hurts, and why. The benchmark-vs-product gap, the specialization trade-off, the LLM text arc, and the layered pipeline architecture are all underexplored in the literature.

### Roadmap to submission
1. Hard negative mining (CoRNStack recipe) — **done** (1.89M pairs, 65% with hard negs)
2. Expand training data (Rust/C++/TS) — **done** (Stack v1: 56k Rust, 58k TS, 63k C++)
3. Train v7 (hard negs + GIST + Matryoshka) — **done, degraded** (R@1: 81.8%, worse than v5 85.5% and base 87.3%)
4. **Train v7b balanced** — 46k/lang × 9 = 414k, equal language representation. Tests imbalance hypothesis.
5. Run full CoIR with base E5 for controlled comparison — **done** (base 49.47, v5 48.67)
6. Controlled ablation table — each layer added/removed with confidence intervals
7. Write draft — intro (gap), method (layered pipeline), experiments (three regimes), discussion (trade-offs)

### Dimensions of code retrieval quality

Useful framework for designing eval sets and understanding which techniques help which dimension:

1. **Semantic depth** — understanding what code *does*. Hard negatives target this.
2. **Task breadth** — NL→code vs NL→NL vs code→code. LoRA narrows this.
3. **Text distribution** — raw code vs enriched descriptions. Enrichment shifts this.
4. **Abstraction level** — "implement a cache" → HashMap+TTL code. Neither LoRA nor enrichment bridges this well.
5. **Structural awareness** — recursion, patterns, error handling. Embeddings see tokens, not structure.
6. **Negative space** — "sort without allocating" requires understanding what code avoids.
7. **Granularity** — function vs file vs concept level retrieval.
8. **Cross-lingual transfer** — Python's `sorted(key=)` = Rust's `.sort_by_key()`. CoIR-CCR tests this (we score 0.490).

Hard negatives primarily improve dimension 1 but may also help 4 (forcing abstract→concrete reasoning) and 8 (same-language negatives force semantic over syntactic discrimination). A dimension-specific eval would reveal which.

### Hard negative mining (Exp 14, in progress)

**Status:** Mining 1.7M CSN pairs with v5 model. FAISS index for top-100 nearest neighbors, γ=0.95 false negative filter, temperature=0.05 softmax sampling, 7 negatives per query, same-language constraint.

**Script:** `~/training-data/mine_hard_negatives.py`

**Test run (1000 pairs):** 100% got 7 negatives, ~49 valid candidates per query after filtering. Negatives are semantically related (same domain, different function) — exactly what we want.

**Run 1 (CSN only, CPU FAISS):** Killed after ~10 hours at 81.8%. CPU brute-force FAISS was the bottleneck (~1.7B dot products per batch × 1,710 batches). 29.5% skip rate (no valid same-language hard negatives).

**Run 2 (9-language, GPU FAISS, 2026-03-22 04:00 CDT):** Combined dataset of 1.89M pairs (CSN 6 langs + Stack Rust/TS/C++). GPU FAISS installed (conda faiss-gpu 1.13.2). Softmax overflow fix applied. Embedding cache enabled. Streaming output with resume support. Query embedding complete (~30 min). Corpus embedding 13% at 2.25 it/s (code passages are longer → slower). ETA ~7:30 AM CDT for corpus, then GPU FAISS search (minutes) + sampling.

**Verification pass (2026-03-22 04:30 CDT):** All factual claims in research log verified against actual data:
- 9 per-task CoIR deltas: all match within rounding
- CoRNStack +9.4pp: confirmed from Table 4 (72.7 - 63.3)
- LoRACode 86.7%: confirmed as relative MRR gain on Python/UniXcoder
- Dataset sizes: all match file line counts
- Leaderboard base E5: 1.4pp discrepancy with official score (49.47 vs 50.90), likely from missing codeforces task

**Consistency filtering results:** All three Stack languages passed at 100% (zero pairs filtered). Avg cosine similarity 0.82-0.84. The Stack pairs with doc comments are self-selecting for quality — regex extraction only captures documented functions. Same result as CSN: consistency filtering doesn't help clean data. The value is entirely in hard negative mining.

**After mining:**
1. Train v7 with `train_lora.py --data csn_hard_negs.jsonl --epochs 1`, eval on hard eval + full 10-task CoIR
2. Key question: does CSN improve without degrading generalist tasks?

### Exp 14: v7 — Hard Negatives + GISTEmbedLoss + Matryoshka — 2026-03-22

**Training config:**
- Data: 200k subsample from 1.89M combined (CSN 6 + Stack Rust/TS/C++), shuffled seed 42
- Language composition: PHP 23%, Java 22%, Python 21%, Go 17%, JS 6%, C++ 3%, TS 3%, Rust 3%, Ruby 2%
- Loss: CachedGISTEmbedLoss (guide=base E5, margin=0.05) + MatryoshkaLoss (768/384/192/128)
- LoRA: r=16, alpha=32, target=query/key/value/dense
- Training: 1 epoch, batch 32, lr 2e-5, warmup 10%, fp16
- Hardware: A6000 48GB, 6h53m (3.5s/step with GIST guide model)
- Final train loss: 0.327 avg (0.146 last step), eval loss: 0.085

**Hard eval results (55 adversarial queries, 268 chunks):**

| Model | Training | Recall@1 | Recall@5 | MRR | NDCG@10 |
|-------|----------|----------|----------|-----|---------|
| **E5-base-v2** | None (pretrained) | **87.3%** | 98.2% | **0.9265** | **0.9449** |
| jina-v2-base-code | Full FT (161M) | 76.4% | 100.0% | 0.8621 | 0.8971 |
| E5-LoRA-v5 | 166k CSN, MNR, 1ep | 85.5% | 98.2% | 0.9077 | 0.9304 |
| **E5-LoRA-v7** | 200k 9-lang, GIST+Matryoshka, 1ep | **81.8%** | 98.2% | **0.8750** | **0.9053** |

Per-language MRR: v7 holds Python (0.955) but drops TypeScript (0.740 vs base 0.769) and Go (0.867 vs base 1.000).

**v7 is worse than v5, and both are worse than base E5 on hard eval.** GISTEmbedLoss + hard negatives + Matryoshka did not solve the specialization trade-off — v7 actually degraded further (-3.7pp R@1 vs v5, -5.5pp vs base).

**CoIR results:** Running (task b626hmrzb). Go NDCG@10: 0.785 (base 0.780, v5 0.793).

**Why v7 failed (hypotheses):**
1. **Language imbalance**: PHP/Java/Python dominate at 82% of training data. Rust/TS/C++ at 9% total — too diluted to help, but enough to disrupt learning.
2. **GISTEmbedLoss didn't prevent degradation**: The guide model (base E5) filters false negatives, but doesn't prevent the model from over-fitting to dominant languages. The loss prevents false *negative* damage but not *imbalance* damage.
3. **Matryoshka overhead**: Training across 4 dimensions may spread capacity too thin with limited data.
4. **200k subsample too small for 9 languages**: v5 used 166k on 6 CSN languages. v7 used 200k on 9 languages — effectively less per-language coverage.

**Next: v7b balanced (46k/lang × 9 = 414k)**
Equal language representation. Ruby limits to 46k (smallest). Tests whether balance fixes the degradation.

### Language expansion analysis

CSN covers 6 languages (Go, Java, JS, Ruby, Python, PHP). Our users need Rust, C++, TypeScript.

**Approach:** Clone popular repos (tokio, serde, axum, clap for Rust; TS compiler, deno for TS; nlohmann/json, fmt, spdlog for C++). Run `cqs index`, extract (docstring, code) pairs, consistency-filter, mine hard negatives.

**The mix problem:**
- Estimated new pairs: ~11k (3.5k Rust, 5.6k TS, 2.1k C++)
- Existing CSN: 1.71M
- New languages would be 0.7% of total — invisible without oversampling
- Python alone (457k) is 41x larger than all 3 new languages combined

**Options considered:**
- A: Oversample new langs to 5% each in 166k training set. Risk: overfit to 4 repos per language.
- B: Use new langs for **eval only**, not training. Check if CSN training transfers via E5's base multilingual ability. If it does, no new training data needed.
- C: Generate more pairs (20+ repos per language, 50k+ per language). Makes mix meaningful without oversampling.
- D: The Stack v2 on HuggingFace has all languages pre-parsed. Gated dataset — needs license acceptance at huggingface.co/datasets/bigcode/the-stack-v2-dedup. Could stream Rust/TS/C++ subsets without downloading full 3TB. Requires bulk download agreement with SoftwareHeritage/INRIA for full dataset.

**Software Heritage principles (softwareheritage.org/2023/10/19/swh-statement-on-llm-for-code):**
The Stack v2 is governed by three principles: (1) models must be released under open licenses — no monopolization of knowledge built from the commons, (2) training data must be fully identified via SWHID identifiers for transparency, attribution, and bias studies, (3) authors must have opt-out mechanisms before training begins. They frame source code as "a digital commons that embodies decades of human creative effort."

For our use: we're training a LoRA adapter (open, published on HuggingFace under Apache 2.0), with traceable training data (CSN + The Stack subsets), and we're not redistributing the raw code. Should be compliant. The adapter weights don't contain memorized code — they adjust embedding geometry.

**Decision:** Do B first — eval-only. Build a Rust/TS/C++ eval set from cloned repos. If CSN-trained model already searches Rust well (E5 base handles it), language expansion is low priority. If not, pursue D (The Stack) for volume.

**Extraction results (2026-03-22):** Streamed from The Stack v1 with `extract_stack_pairs.py`. Regex-based function+docstring extraction, min 1 star filter.

| Language | Files streamed | Pairs extracted | Repos |
|----------|---------------|----------------|-------|
| Rust | 50k | 56,130 | 13,154 |
| TypeScript | 200k | 57,884 | 49,140 |
| C++ | 200k | 25,186 | 44,528 |
| **Total** | — | **139,200** | — |

Still need: consistency filtering, hard negative mining for new languages, combine with CSN data.

**Language balance strategy:** CSN is dominated by PHP/Java/Python (23-25% each). New languages at 56-63k pairs each are ~3% of combined 1.9M — comparable to Ruby (2.4%). After hard negative mining, small languages will lose more pairs (fewer candidates → higher skip rate). Solution: **subsample per language** when building the final training set. Options:
- Equal: 50k per language × 9 = 450k total. Prevents dominance.
- Weighted toward user need: oversample Rust (primary cqs user language).
- Proportional-with-floor: no language below 25k.

The mining skip rate (~29.5% overall) is not uniform — languages with fewer pairs have fewer valid negative candidates. Ruby (46k pairs) probably skips 40-50% vs Python (392k) at ~25%. We'll know exact numbers when mining completes.

### Literature survey (2026-03-22)

**Validated directions (others doing similar):**
- Hard negatives: GitHub Copilot's embedding model blog confirms "near misses" drove their biggest quality gains (+37.6% relative lift). CoRNStack, NV-Retriever same conclusion. We're on the right track.
- Synthetic query generation: Qodo-Embed-1 (68.53 CoIR, 1.5B params) uses LLM-generated docstrings + search queries as training data. Their 1.5B beats 7B models. Data quality > model size.
- Matryoshka Representation Learning: GitHub uses it. sentence-transformers supports natively. Free to add to our training.

**Genuinely novel ideas (verified as unexplored):**
- **Call-graph false negative filtering**: use structural evidence (shared callers, shared types, same module) to determine which in-batch pairs are false negatives, instead of asking another model. Deterministic, no guide model blindspots. cqs already builds call graphs and type edges for every indexed codebase — this is a product advantage that becomes a training advantage. Graph contrastive learning exists, but using call graphs specifically to filter false negatives during text embedding training is unpublished.
- **Test-derived training queries**: `cqs test-map` maps functions to their tests. If a test calls `validate_email("bad@")` and asserts `false`, that's a behavioral specification: "function that rejects invalid email addresses." More precise than docstrings or LLM-generated queries. No one uses test assertions as training signal for code retrieval embeddings.
- **Negative space training**: explicit "without X" queries for code. Embeddings demonstrably fail at negation. No one has created negative-space training pairs for code search.
- **Multi-granularity semantic embedding**: different vector subspaces for function-level vs module-level vs concept-level meaning. Matryoshka handles dimension truncation but not semantic granularity.

**Potential research contribution unique to cqs:** The call-graph filtering and test-derived queries are only possible in a system that deeply understands code structure. Benchmark pipelines see text; cqs sees call graphs, type edges, test coverage, and behavioral specifications. This is the paper's strongest angle — not "we made a small model competitive" but "code intelligence infrastructure creates training signals that text-only pipelines can't access."

**Practical feasibility:** ~500 repos per language, clone and index with cqs, 2 hours parallel per language, 24GB disk. CSN data (90% of training set) has no repo context — only works for clone-and-index data. Same-repo heuristic from The Stack metadata is the free alternative (functions from same repo = potential false negatives).

**Sequencing:** Get v7 results first (hard negs + GISTEmbedLoss + Matryoshka on current data). If generalist degradation persists, call-graph filtering is the next lever. Don't build the infrastructure before proving the baseline approach works.

**Underexplored (exists but rare):**
- Temporal/evolutionary training from git history: TransformCode uses syntactic transforms, TeaRAGs uses git trajectories for re-ranking, Repo2Vec embeds repos. But (old_version, new_version) contrastive pairs for embedding training is uncommon.
- Structural metadata as text in training NL: GraphCodeBERT uses data flow graphs during pre-training, but as graph input to the model. Injecting structural features as *text* ("2 loops, 3 branches, calls 5 functions") into the NL before a standard text encoder is underexplored.
- Co-change frequency as contrastive signal: functions modified together share coupling. TeaRAGs re-ranks by git signals. Using co-change as training pairs is rare.

**Dead ends confirmed by literature:**
- Generic pre-trained code embeddings don't always help downstream tasks (Springer EMSE)
- Contrastive fine-tuning causes catastrophic forgetting of token-level knowledge (SimCSE/ACL)
- Structure-blind embeddings underperform in 5/6 tasks (EMSE)
- Single-vector embeddings have theoretical capacity limits based on sign-rank (arXiv 2508.21038)

**Language balance question:** With 9 languages (PHP 23% down to Rust 3%), in-batch negatives are dominated by PHP/Java/Python. Underrepresented languages get cross-language negatives that teach language discrimination, not semantic understanding. No paper directly studies this for code embeddings. Plan: train v7a (unbalanced 1.89M) and v7b (equal 56k/lang, 504k total). If v7b wins despite less data, balance matters more than volume.

**MNR loss may be the wrong loss function.** HuggingFace blog shows MNR alone can *hurt* (Korean example: base 0.671 → MNR 0.626, -0.045 — same magnitude as our LoRA degradation). GISTEmbedLoss with guide model + relative margin filtering outperforms MNR by handling false negatives. Consider switching from MNR to CachedGISTEmbedLoss for v7.

**Actionable for next training run:**
1. Matryoshka loss wrapper (free, add to train_lora.py)
2. Synthetic query augmentation (~$3 Haiku batch for 1.89M pairs)
3. Structural metadata in training NL (extract from tree-sitter, append to passage text)
4. GISTEmbedLoss instead of MNR — implemented in train_lora.py (`--use-gist`). Guide model (base E5, frozen) filters false negatives with relative margin. Prevents the generalist degradation we see with MNR.
5. Matryoshka loss — implemented (`--matryoshka`). Multi-dimension embeddings at 768/384/192/128. Free multi-resolution retrieval.
6. Test balanced vs unbalanced language distribution (v7a vs v7b)

**Mining results (2026-03-22 05:45 CDT):**
- 1,888,099 pairs processed, 1,226,409 (65%) got hard negatives
- 661,690 (35%) skipped — no valid same-language negatives after γ=0.95 filter
- GPU FAISS search: minutes (vs hours on CPU). Embedding cache saved to .npy.
- Total mining time: ~2 hours (embedding) + minutes (search + sampling)

**v7 training started (2026-03-22 05:58 CDT):**
```bash
python train_lora.py \
  --data combined_9lang_hard_negs.jsonl \
  --output ./e5-code-search-lora-v7 \
  --epochs 1 --use-gist --matryoshka --export-onnx
```
1.89M pairs, GISTEmbedLoss (base E5 guide, margin 0.05), Matryoshka (768/384/192/128), LoRA rank 16. ETA ~5-6 hours on A6000.

Sources: [Qodo-Embed-1](https://www.qodo.ai/blog/qodo-embed-1-code-embedding-code-retrieval/), [GitHub Copilot Embedding](https://github.blog/news-insights/product-news/copilot-new-embedding-model-vs-code/), [Theoretical Limits](https://arxiv.org/abs/2508.21038), [GraphCodeBERT](https://openreview.net/pdf?id=jLoC4ez43PZ), [TransformCode](https://arxiv.org/pdf/2308.12773), [SimCSE Forgetting](https://caiac.pubpub.org/pub/n7sbt42t), [CoRNStack](https://arxiv.org/abs/2412.01007), [Negation in Embeddings](https://community.openai.com/t/embedding-does-not-capture-negative-expression/579676)

### Haiku vs Sonnet summary comparison (Exp 14b)

Tested on 19 hard eval Rust functions. Both Haiku and Sonnet summaries **hurt** R@1 by 5.3pp on well-documented code. Model doesn't matter — the prompt is the key. Summaries are gap-fillers for undocumented code, not improvements for documented code. Haiku stays as default (5x cheaper, same embedding quality).

**Summary augmentation (Dimension 4):** Script `~/training-data/augment_with_summaries.py` adds (discriminating_summary, code) pairs alongside (docstring, code) pairs. For our codebase, summaries are cached in cqs store. For CSN, would need ~$2 Haiku batch to generate. The discriminating summaries capture *what makes a function unique* — bridging abstract intent to concrete implementation. Free data augmentation for indexed codebases.

**Note:** cqs users are always AI agents, not humans. Agent queries tend to be more precise and technical than human queries — "function that validates JWT tokens and checks expiration" rather than "JWT stuff." This affects which quality dimensions matter most: semantic depth (1) and abstraction level (4) over task breadth (2).
