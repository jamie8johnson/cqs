# Project Continuity

## Right Now

**v1.11.0 released. 8th audit (80/88 fixed). 6 new commands. Contrastive experiment confirmed 89.1% floor. (2026-03-29)**

### What shipped today
- v1.11.0: 8th audit (80 findings fixed) + 6 new commands + query expansion
- PR #715 merged (audit), PR #720 merged (commands), PR #721 open (CONTRIBUTING)
- crates.io published, GitHub release building, binary installed
- 19 OpenClaw contributions (9 PRs, 9 issues, 1 comment)
- Paper v0.6 (thesis: training signal quality > model capacity)
- Pre-Edit hook live (`.claude/hooks/pre-edit-context.py`)
- Audit skill improved (prompt gen + review steps 8-9)

### Training — 89.1% basin confirmed (5 data points)
| Variant | Change | Pipeline R@1 |
|---------|--------|-------------|
| v9-200k | baseline | **94.5%** |
| v9-500k | 2.5× more data | 89.1% |
| v9-200k-hn | + FAISS hard negatives | 89.1% |
| v9-200k-1.5ep | 1.5× more epochs | 89.1% |
| contrastive-B | 25% contrastive queries | 89.1% |

Query format changes don't help. Need fundamentally different training signals (test-derived queries, type-aware negatives).

### In Progress (2026-03-29 ~16:00 CDT)
- Issue-fixer agent running: #711 (diff impact cap), #695 (export-model auto-dim), #694 (batch ID trait), #697 (cargo audit)
- Markdown split done (waiting to commit together)
- Pipeline batch=64 restored with debug logging (#716 investigation)
- All on branch `docs/contributing-checklist` (or agents may have created `fix/open-issues-batch`)

### Uncommitted changes across branches
- `src/cli/pipeline.rs` — EMBED_BATCH_SIZE 32→64 + debug logging
- `src/parser/markdown/` — split from markdown.rs (4 files, 3 context structs)
- Issue fixer agent modifying: impact/diff.rs, export_model.rs, llm/provider.rs, Cargo.toml

### Pending
1. Issue-fixer agent completes → commit all + PR
2. Merge PR #721 (CONTRIBUTING checklist)
3. Re-run full eval matrix on final code state
4. Close resolved issues (#711, #695, #694, #697, #716, #718)
5. Next training: test-derived queries or type-aware negatives

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1
- Blackwell GPU upgrade
- Publish 500K/1M datasets to HF (waiting for training experiments to settle)

## Open Issues
- #389, #255, #106, #63 (upstream)
- #694, #695 (audit P4)
- #711 RT-RES-9
- #716 PERF-45 (EMBED_BATCH_SIZE diagnosis)
- #717 RM-40 (HNSW mmap)
- #718 CQ-38 (markdown.rs split)

## Architecture
- Version: 1.11.0, BGE-large default (1024-dim)
- v9-200k LoRA: 94.5% pipeline, 70.9% raw (110M = 335M on pipeline)
- 6 new commands: brief, affected, neighbors, doctor --fix, train-pairs, query expansion
- HF dataset: https://huggingface.co/datasets/jamie8johnson/cqs-code-search-200k
- OpenClaw: https://github.com/openclaw/openclaw (19 contributions, all open)
- Tests: ~1540
