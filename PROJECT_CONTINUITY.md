# Project Continuity

## Right Now

**v1.0.11 released (2026-03-16).** Clean main, nothing in progress.

### Done this session
- Roadmap fresh-eyes review: 5 parallel scout agents, found stale version/language/schema refs, missing injections, untracked issue #555, red team findings not in roadmap
- Roadmap cleanup: v1.0.10 header, 51 languages, schema v12→v13, 1.0.x highlights section, red team accepted/deferred section, #555 tracked, injections (Make→Bash, Razor→JS/CSS) added to Done
- RT-DATA-4: Notes lock race fixed — separate .lock file survives atomic renames
- RT-DATA-2: Enrichment idempotency — blake3 hash of post-filtered call context, skip on match
- RT-DATA-6: HNSW crash desync — dirty flag in SQLite metadata, set before writes, cleared after save
- #555: where_to_add coverage for 43 languages — 10 family groups + explicit skip list
- Schema v13 migration: enrichment_hash column + hnsw_dirty metadata key
- v1.0.11 released with binaries (Linux, macOS, Windows)

### Key decisions made
- Dirty flag over generation counter for RT-DATA-6 — simpler, no HNSW format change, no coordination protocol
- Enrichment hash computed post-filtering (IDF + ambiguity) not from raw call graph
- Fresh-eyes review of own plan caught 3 bugs: wrong generation protocol, pre-filter hash, #555 scope mixing
- CAGRA SIGSEGV on small datasets is cuVS upstream bug, not ours (graph degree > dataset size)

## Pending Changes

None.

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-6: LLM-generated summaries** — breaks local-only
- **SQ-7: LoRA fine-tune E5 on A6000** — the real fix for code-vs-doc ranking. Training data: hard eval + holdout + synthetic. Upload merged ONNX to HuggingFace.
- **`cqs plan` templates** — 11 templates
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

- Version: 1.0.12
- MSRV: 1.93
- Schema: v13
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1080 lib pass
- SQ-4: Two-pass enrichment with idempotency hash (RT-DATA-2)
- SQ-5: Filename stems in NL (generic stems filtered)
- HNSW dirty flag for crash detection (RT-DATA-6)
- Notes lock file for concurrent write safety (RT-DATA-4)
- CUDA: 13 (cuVS) + 12 (ORT) symlinked into conda lib dir
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- Notes: 119 indexed
- Red team: 21+ protections verified, 10 findings fixed, 2 deferred
