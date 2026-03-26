# Project Continuity

## Right Now

**6th full audit complete. All 82 findings fixed. v9-mini pipeline indexing. (2026-03-26)**

### Active Work
- **Branch**: `feat/runtime-embedding-dim` — PR #687 (CI pending)
  - #681 BatchProvider trait, #682 runtime embedding dim, #683 batch size docs, #684 P4 remainders
  - Two agents running: #665 (lazy enrichment) and #666 (GC transaction)
- **Gap indexing**: 293/554 repos for underrepresented languages (PHP/Ruby/Rust/Python/C++)
  - Output: `~/training-data/stack_indexed_pairs_gaps.jsonl`
  - Already have 1.25M pairs from first 265 repos; filling per-language gaps to 15K each

### v9-mini Pipeline (Exp 18)
- **Pivot**: training data from CSN → The Stack (full repos with call graphs)
- **Novel signals**: call-graph false-negative filtering + synthetic multi-style queries
- **Status**: 1,900 repos selected (1,350 original + 554 gap), 1,904 cloned, ~560 indexed
- **Scripts**: `select_and_clone_repos.py`, `index_stack_repos.py`, `gen_synthetic_queries.py`, `assemble_v9_mini.py`
- **Next steps after indexing**: mine hard negs (with --call-graph), gen synthetic queries (~$2), assemble 100K balanced set, train
- **Success bar**: hard eval ≥ 92.7% AND CSN ≥ 0.627

### Session Accomplishments (2026-03-26)
1. v1.5.0 released (PR #678 — base E5 default, CI env var fix)
2. 6th full audit: 82 findings across 14 categories, ALL fixed
   - PR #685: 74 findings (P1-P3) — merged
   - PR #686: #680 FieldStyle field extraction for 28 languages — merged
   - PR #687: #681-684 — pending CI
3. v9-mini pipeline: 5 scripts written, 1,900 repos cloned, indexing in progress
4. Exp 18 research log updated (Stack + call-graph filter + synthetic queries)
5. #680 plan written, fresh-eyes reviewed, executed (FieldStyle enum)
6. 133GB debug build artifacts cleaned
7. Tests: 1395 → 1434+ (39+ new tests)

### Parked
- v9 training (waiting on indexing to complete)
- Paper v0.5 (waiting on v9 results)

## Open Issues
- #389 (CAGRA memory — blocked on upstream cuVS)
- #255 (pre-built reference packages — feature request)
- #106 (ort pre-release RC — waiting on stable)
- #63 (paste unmaintained — waiting on upstream)
- #665, #666 (being fixed now in PR #687)
- #680-684 (fixed, close after PR #687 merge)

## Architecture
- Version: 1.5.0
- Schema: v16 (llm_summaries table)
- Current shipping model: base E5 (intfloat/e5-base-v2, 92.7% hard eval, 0.627 CSN)
- Full-pipeline: 96.3% R@1 (HyDE + contrastive summaries)
- Languages: 51 (28 with field extraction via FieldStyle)
- Tests: ~1434 (with gpu-index)
- CLI split: definitions.rs + dispatch.rs + mod.rs
- LLM: BatchProvider trait (Anthropic impl)
- Embedding: runtime dimension detection via OnceLock
- CallGraph: Arc<str> interning
- Paper: ~/training-data/paper/draft.md (v0.4)
- Training repo: github.com/jamie8johnson/cqs-training
