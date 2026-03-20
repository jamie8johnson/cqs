# Project Continuity

## Right Now

**SQ-8: --improve-docs implementation (2026-03-19).** Spec finalized after 3 review rounds. Writing implementation plan next.

### Spec
`docs/superpowers/specs/2026-03-19-improve-docs-design.md`

Key decisions:
- `--improve-docs` is modifier to `--llm-summaries` (requires it)
- Two separate Batches API passes (summaries then doc comments)
- Schema v16: composite PK (content_hash, purpose) on llm_summaries
- Re-parse files at write-back time (no doc line ranges in DB)
- Per-language DocWriter: 11 explicit formats + `// ` default for remaining 40
- Bottom-up insertion, leaf-only, decorator-aware
- Python docstrings inside function body
- `--dry-run` and `--max-docs N` flags
- `submit_batch` needs max_tokens override param
- `fetch_batch_results` 500-char ceiling needs removing for doc comments

### Done this session
- Merged LoRA research state update (PR #626)
- 3 LoRA experiments (all regressed) → pivoted to SQ-8 + SQ-10
- SQ-8 spec: 3 design rounds + 3 review rounds, all issues addressed
- Research log at ~/training-data/RESEARCH_LOG.md

## Parked

- **SQ-7 LoRA:** revisit if we switch base model (SQ-3)
- **SQ-10:** Fine-tune code reranker (after SQ-8)
- **SQ-3:** Code-specific base model
- **Post-index name matching** — fuzzy cross-doc references

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00
- cuVS PR #1840 (CAGRA serialize): open
- Audit cuVS + ort: planned

## Architecture

- Version: 1.1.0
- Schema: v15 (v16 planned for SQ-8)
- Embeddings: 768-dim E5-base-v2 (no fine-tuning)
- Tests: ~1734
- Training env: conda cqs-train, A6000 48GB (preserved for SQ-10 reranker)
- Training data: ~/training-data/ (1.7M CodeSearchNet pairs for SQ-10)
