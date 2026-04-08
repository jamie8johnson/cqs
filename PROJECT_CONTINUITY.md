# Project Continuity

## Right Now

**SPLADE training DONE. Audit 8/14 categories, partial fixes in worktrees. (2026-04-08 CDT)**

### SPLADE Training — COMPLETE
- 24,500 steps, 3h 20m, batch_size=16 on checkpoint-7500 base
- Final model: `~/training-data/splade-code-v1/final/adapter_model.safetensors`
- Checkpoints: 22000, 23000, 23500, 24000, 24500
- **ONNX export FAILED silently** — `onnx/model.onnx` missing, only tokenizer files
- Need: manual ONNX export → integration test → v2 eval ablation
- Lesson: batch_size=32 OOMs A6000 (48/49GB). Use 16 (28GB, stable).
- Lesson: resume requires `SparseEncoder(checkpoint_path)` + re-apply LoRA. Trainer's `resume_from_checkpoint=` is broken with PEFT.

### Audit — 8 of 14 categories done
- **52 unique findings** triaged in `docs/audit-triage.md` (P1: 8, P2: 10, P3: 25, P4: 9)
- Findings: `docs/audit-findings.md`
- **7 categories NOT audited**: Documentation, API Design, Scaling, Algorithm Correctness, Extensibility, Platform Behavior, Resource Management

### Worktree Fixes (partial — session crashed before agents finished)
- **`.claude/worktrees/agent-a2ffa931/`** (cache.rs): 5 fixes done — SEC-7 (URL encoding), SEC-8 (permissions 0o700), DS-47 (busy_timeout), DS-50 (multi_thread runtime), CQ-1 (delete VerifyReport). Cherry-pick this.
- **`.claude/worktrees/agent-ab299ef5/`** (splade/mod.rs): 3 fixes done — RB-10 (poisoned mutex), RB-13 (4000 char truncation), PF-14 (zero-copy logits). Cherry-pick this.
- Other 4 worktrees: no code changes (agents didn't reach code before crash)
- **44 findings still unfixed** after cherry-picking the 8 above

### What Still Needs Doing (in order)
1. Cherry-pick worktree fixes into branch, verify, commit
2. ONNX export of trained SPLADE model
3. Integration test — point cqs SPLADE encoder at new model
4. V2 eval ablation with code-trained SPLADE
5. Fix remaining 44 audit findings (dispatch new agents or manual)
6. Run remaining 7 audit categories
7. Release v1.20.0

### Open PRs
- #840: audit tests + Elm + roadmap (check CI)

### Branch State
- On `audit/chunk-type-tests`
- Uncommitted: PROJECT_CONTINUITY.md, audit-findings.md, audit-triage.md, audit-triage-v1.19.0-pre.md, audit-findings-v1.15.1.md
- Untracked: `.claude/worktrees/`, `evals/runs/`, `docs/audit-triage-v1.19.0-pre.md`

## Parked
- Wiki system — spec revised (agent-first)
- Code-trained reranker — after SPLADE eval
- Paper v0.7

## Open Issues
- #717 (HNSW mmap), #389 (CAGRA memory), #255 (pre-built refs), #106 (ort RC), #63 (paste)

## Architecture
- Version: 1.19.0, Languages: 54, Tests: ~2365, Chunk types: 27
- BGE-large + LLM summaries = best production config (pre-SPLADE)
- Eval: v2 (265q), fixture (296q), noise (143q)
- Embedding cache: SQLite at ~/.cache/cqs/embeddings.db
- SPLADE code model trained, pending ONNX export + eval
