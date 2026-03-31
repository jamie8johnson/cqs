# Project Continuity

## Right Now

**FTS5 synonym expansion fix + expanded eval (296 queries, 7 languages). (2026-03-31 07:35 CDT)**

### What's done this session
- Fixed FTS5 syntax error: OR groups require explicit AND between terms (`(a OR b) AND c`, not `(a OR b) c`)
- Hardened pipeline_eval to treat search errors as misses instead of panicking
- Removed unused test imports (crud.rs Chunk, staleness.rs PathBuf)
- Clean build, zero warnings

### Expanded eval results (BGE-large, 296 queries, 7 langs)
| Config | R@1 | MRR |
|--------|-----|-----|
| A: Cosine-only | **90.9%** | 0.9493 |
| B: RRF | 74.7% | 0.8618 |
| C: RRF + name_boost | 75.3% | 0.8656 |
| D: HNSW + name_boost | 90.5% | 0.9448 |
| E: Cosine + demotion | 90.9% | 0.9493 |
| F: HNSW + boost + demote | 90.5% | 0.9448 |

Key: RRF hurts at this scale (74% vs 91%). Cosine-only is best. HNSW close to brute-force.

### Pending changes (uncommitted)
- `src/search/synonyms.rs` — FTS5 explicit AND fix + new test
- `src/search/query.rs` — removed debug eprintln
- `tests/pipeline_eval.rs` — search error resilience (4 configs)
- `tests/eval_common.rs` — 296 queries (77 hard + 219 holdout), 7 languages
- `tests/fixtures/eval_*_java.java`, `eval_*_php.php` — new fixtures
- `src/store/chunks/crud.rs`, `staleness.rs` — removed unused imports
- `Cargo.toml`, `Cargo.lock`, `ROADMAP.md`, `docs/notes.toml`, `docs/openclaw-contributions.md` — various prior-session updates

### Next
1. Commit + PR the FTS5 fix and expanded eval
2. Run full eval matrix (9 models × 296 queries) once committed
3. Update paper with expanded eval results
4. Ship v9-200k as LoRA preset in cqs

### Training basin (7 data points, 55-query eval)
| Variant | Pipeline R@1 |
|---------|-------------|
| v9-200k | **94.5%** |
| v9-175k, v9-500k, v9-200k-hn, v9-200k-1.5ep, contrastive-B, v9-200k-testq | 89.1% |

### OpenClaw — 7 PRs, 6 issues
Tracker: `docs/openclaw-contributions.md`. Consolidated from 12→7 PRs.

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1 (P&ID → PLC function block mapping)
- IEC 61131-3 language support
- Openclaw variant for PLC process control (long horizon)
- Blackwell GPU upgrade
- Publish 500K/1M datasets to HF
- Type-aware negative mining (7 basin points suggest diminishing returns)
- Imbalanced 200K experiment (lower priority post per-query analysis)

## Open Issues (cqs)
- #717 RM-40 (HNSW fully in RAM, no mmap)
- #389 (upstream cuVS CAGRA memory)
- #255, #106, #63 (upstream deps)

## Architecture
- Version: 1.12.0
- v9-200k LoRA: 94.5% pipeline, 70.9% raw — published to HF
- Narrow peak at 22K/lang. Gap = 3 TypeScript queries.
- Expanded eval: 296 queries, 7 languages (Rust, Python, TS, JS, Go, Java, PHP)
- Commands: 50+
- Tests: ~1540
