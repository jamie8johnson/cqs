# Project Continuity

## Right Now

**v1.1.0 released. Starting `cqs train-data` implementation (SQ-7 prep) (2026-03-19).**

### Done this session
- Full 14-category audit: 88 findings, P1-P4 all addressed
- 10 PRs merged (#614-#623)
- v1.1.0 released (crates.io + GitHub + 3 platform binaries)
- SQ-9 complete: notes removed from search, 769→768-dim, schema v15
- `cqs train-data` spec written and reviewed (3 rounds)
- LoRA training env installed (conda cqs-train, PyTorch 2.10 + A6000)

### In Progress
- `cqs train-data` command — spec at `docs/superpowers/specs/2026-03-19-train-data-design.md`
- Need implementation plan then execution

### Key Decisions
- BM25 hard negatives (with IDF), not same-file negatives
- Use `git show {commit}:{path}` for commit-time content, not HEAD
- Query normalization: strip conventional commit prefixes + action verbs
- Content hash guard on negatives (BLAKE3)
- Stream JSONL output, checkpoint after each commit
- New `Parser::parse_source()` API required

## Pending Changes

Uncommitted: spec doc + ROADMAP on main. Push in next PR.

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-8: LLM doc comment generation** — write back to source
- **Post-index name matching** — fuzzy cross-doc references
- **ref install** — #255

## Upstream Tracking

- cuVS PR #1839 (search &self): merged, expected v26.04.00 (April)
- cuVS PR #1840 (CAGRA serialize): open, may land v26.04.00
- cuVS #1277 (CUDA 13 Rust): stalled
- Audit cuVS + ort bindings: planned post-release

## Open Issues

- #106: ort stable (rc.12)
- #63: paste dep unmaintained
- #255: Pre-built reference packages
- #389: CAGRA CPU-side dataset retention

## Architecture

- Version: 1.1.0 (released)
- MSRV: 1.93
- Schema: v15 (768-dim)
- Embeddings: 768-dim E5-base-v2
- 51 languages, 16 ChunkType variants
- Tests: ~1694
- Search: project-only by default, --include-refs
- File structure: search/ (3), embedder/ (2), cli/enrichment.rs, cli/args.rs, test_helpers.rs
- Training env: conda cqs-train (PyTorch 2.10, sentence-transformers 5.3, peft 0.18, A6000 48GB)
