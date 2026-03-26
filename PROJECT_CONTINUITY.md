# Project Continuity

## Right Now

**v9-mini training 87% (~10 min). BGE-large plan reviewed 3x, ready to execute. (2026-03-26)**

### Active
- **v9-mini training**: Step 6 at 1539/1773 on A6000. Eval (step 7) runs automatically.
  - 59,708 balanced pairs (9 langs), GIST+Matryoshka, LoRA r=16
  - Success bar: hard eval R@1 ≥ 92.7% AND CSN ≥ 0.627
- **BGE-large plan**: 4 revisions, 3 fresh-eyes reviews. Ready to execute.
  - Spec: `docs/superpowers/specs/2026-03-26-configurable-embedding-models-design.md`
  - Plan: `docs/superpowers/plans/2026-03-26-configurable-embedding-models.md`
  - Key complexity: EMBEDDING_DIM threading through 30+ sites, Store dim field
- **CLAUDE.md restructured**: Task-triggered cqs commands + ownership framing. Check telemetry next session.

### Key Discoveries (2026-03-26)
- **Enrichment = 43.6pp** (raw 49.1% → enriched 92.7%). More than any model or LoRA.
- **8 models tested**: BGE-large best raw (61.8%), E5-mistral worst (9.1%). E5-base is the right default.
- **Instruction models don't help**: GTE-Qwen2 47.3%, adversarial instructions worse (18-47%).
- **Agent adoption**: 87% of cqs usage is `search`. 0% for advanced commands (gather, scout, task, impact).
- **Fresh-eyes reviews are essential**: 3 rounds found 10→15→3 issues in the BGE-large plan.

### Pending (after training completes)
1. Check v9-mini eval results
2. Execute BGE-large configurable models plan (8 tasks)
3. v1.7.0 release with model selection
4. Check cqs telemetry after CLAUDE.md restructure

## Parked
- v9-full (curriculum + test-derived queries) — depends on v9-mini results
- Paper submission — needs v9-mini + BGE-large results
- transformers 5.3.0 testing — updated but GTE-Qwen2 still broken (rope_theta)

## Open Issues
- #389 (CAGRA memory — blocked on upstream cuVS)
- #255 (pre-built reference packages — feature request)
- #106 (ort pre-release RC — waiting on stable)
- #63 (paste unmaintained — waiting on upstream)

## Architecture
- Version: 1.6.0
- Schema: v16
- Model: E5-base-v2 (768-dim), BGE-large planned (1024-dim)
- Enrichment: 43.6pp contribution (raw 49.1% → enriched 92.7%)
- Languages: 51 (28 with FieldStyle field extraction)
- Tests: ~1993 (with gpu-index)
- CLI: definitions.rs + dispatch.rs + mod.rs
- Store: mod.rs + metadata.rs + search.rs
- LLM: BatchProvider trait (Anthropic impl)
- Embedding: runtime dim detection, OnceLock, ModelConfig planned
- CallGraph: Arc<str> interning
- Training: ~/training-data (github.com/jamie8johnson/cqs-training)
- Paper: ~/training-data/paper/draft.md (v0.5)
- Conda env: cqs-train (transformers 5.3.0, torch 2.11.0, faiss-gpu 1.13.2)
