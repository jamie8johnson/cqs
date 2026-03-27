# Project Continuity

## Right Now

**7th audit: 85/95 fixed. PR pending. Gap-filling 90% complete. (2026-03-27)**

### Active
- **Audit v1.7.0**: 85 of 95 findings fixed (P1-P4). 5 open issues, 2 wontfix closed.
  - 3 commits on main awaiting branch + PR + merge
  - Issues created: #694-#700, anthropics/claude-plugins-official#1071
- **Gap-filling pipeline**: 1899/2119 repos (90%), actively indexing. 2.3M pairs extracted.
  - Script: `~/training-data/fill_gaps.sh` step 4
  - Processing manifest: `~/training-data/processing_manifest_retroactive.jsonl` (2,305 repos)

### Pending
1. Branch + PR + merge the 3 audit commits
2. Gap-filling finishes → step 5 (merge) → step 6 (balance check) → step 7 (HF dataset)
3. Delete cloned repos after verification (~39GB)
4. Mine hard negatives on 200K
5. Train v9-200k
6. Paper v0.6

## Parked
- Dart language support (guide written)
- Curriculum scheduling (v9-full)
- Ship v9-mini as default (matches base enriched, better raw+CSN)
- BGE-large eval (multi-model now functional)

## Open Issues
- #389, #255, #106, #63 (blocked on upstream)
- #694 EX-30, #695 EX-32, #696 SEC-20, #697 SEC-22, #700 EX-33 (audit P4)

## Architecture
- Version: 1.7.0
- Models: E5-base default, BGE-large preset, custom ONNX (multi-model functional)
- ModelConfig: CLI > env > config > default, resolved once in dispatch
- LlmProvider: Anthropic (extensible via CQS_LLM_PROVIDER)
- Store::dim(): private field + getter, validated at open (dim=0 rejected)
- DEFAULT_MODEL_REPO: single source of truth in embedder/models.rs
- Languages: 51 (all with skip_line_prefixes)
- nl/ directory: mod.rs, fts.rs, fields.rs, markdown.rs
- Tests: 1490
