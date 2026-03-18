# Project Continuity

## Right Now

**A6000 machine sync + bug fixes (2026-03-18).** Synced from other machine (v0.28.3→v1.0.13), found and fixed 4 bugs.

### Done this session (2026-03-18)
- Synced 39 commits from other machine (v0.28.3→v1.0.13)
- Fixed ORT CUDA provider path resolution (dladdr returns argv[0] on glibc, ORT falls back to CWD)
- Fixed ORT 1.23.2/1.24.2 version mismatch SIGSEGV (stale cache, updated LD_LIBRARY_PATH)
- Fixed CAGRA use-after-free on shape pointers (host ndarrays dropped while device tensors referenced them)
- Fixed LLM batch resume on interrupt (persist batch_id in SQLite metadata, resume polling on restart)
- Ran LLM summaries on A6000: 2638 API + 4556 doc-comment = 7194 total, 4088 unique stored
- Added ANTHROPIC_API_KEY to ~/.bashrc
- All tests pass: 1650 pass, 0 fail

### Uncommitted changes
- `src/embedder.rs`: ORT provider symlink fix (ort_runtime_search_dir + atexit cleanup)
- `src/cagra.rs`: Shape pointer lifetime fix (host arrays same scope as device tensors)
- `src/llm.rs`: Batch resume (check_batch_status, resume_or_fetch_batch, pending batch logic)
- `src/store/mod.rs`: set_pending_batch_id / get_pending_batch_id
- `.gitignore`: libonnxruntime_providers_*.so pattern
- `.cargo/config.toml`: unchanged (reverted -rdynamic)
- `ROADMAP.md`: SQ-6 marked done with batch resume
- Plus 70+ files from merge with origin/main

## Pending Changes

Uncommitted fixes above — need branch + PR.

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-7: LoRA fine-tune E5 on A6000** — NEXT. Training data: hard eval + holdout + synthetic pairs
- **Post-index name matching** — fuzzy cross-doc references
- **ref install** — #255

## Open Issues

### External/Waiting
- #106: ort stable (rc.12)
- #63: paste dep unmaintained (RUSTSEC-2024-0436)

### Feature
- #255: Pre-built reference packages

### Audit
- #389: CAGRA CPU-side dataset retention

### Red Team (unfixed, accepted/deferred)
- RT-DATA-3: HNSW orphan accumulation in watch mode (medium — no deletion API)
- RT-DATA-5: Batch OnceLock stale cache (medium — by design, restart to refresh)

## Architecture

- Version: 1.0.13 (with local fixes, not yet released)
- MSRV: 1.93
- Schema: v14 (llm_summaries table)
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1650 pass (with gpu-index)
- ORT: 1.24.2 (ort crate 2.0.0-rc.12)
- SQ-6: LLM summaries via Claude Batches API, cached by content_hash, batch resume on interrupt
- `cqs plan` command: 11 task-type templates
- CUDA: 13 (cuVS) + 12 (ORT, at /usr/local/cuda-12/)
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- Notes: 122 indexed
- Red team: 21+ protections verified, 10 findings fixed, 2 deferred
