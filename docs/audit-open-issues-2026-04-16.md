# Open-issue audit — 2026-04-16 (post-v1.26.1)

Audit pass on all 20 open GitHub issues after v1.26.1 shipped. Each has been code-verified; fix prompts are amended onto the issue bodies. This document is the cross-cutting ledger.

## Summary

| # | Title | Impact | Difficulty | Verdict |
|---|-------|--------|-----------|---------|
| #63 | deps: paste unmaintained (RUSTSEC-2024-0436) | LOW | EASY | **close** — audit.toml ignore already in place |
| #106 | ort 2.0-rc.12 (no stable released) | MEDIUM | EASY | keep — blocked on upstream |
| #255 | Pre-built reference packages | HIGH | HARD | keep — update model/schema specifics before spec work |
| #717 | HNSW fully loaded into RAM (RM-40) | MEDIUM | HARD | keep — doc workaround (daemon), mmap needs lib swap |
| #916 | mmap SPLADE index body (PF-11) | LOW | MEDIUM | keep — wins smaller than claimed |
| #917 | Streaming SPLADE serialize (RM-5) | **HIGH** | MEDIUM | **prioritize** — ~60-100MB peak drop, contained scope |
| #921 | SPLADE save blocks watch on WSL 9P | LOW | MEDIUM | **rescope** — "blocks watch" claim invalid; watch never calls `idx.save` |
| #951 | Re-benchmark README Performance table | MEDIUM | EASY | fix — table is v1.22.x-era, cite v1.26.1 + 14,882 chunks |
| #954 | Grammar-less parser dispatch via LanguageDef | MEDIUM | EASY | fix — cheapest refactor-lane win |
| #955 | Compile-enforced ChunkType type-hint patterns | MEDIUM | MEDIUM | fix — extend `define_chunk_types!` with `hints = [...]` |
| #956 | ExecutionProvider: CoreML/ROCm decouple | **HIGH** | HARD | keep — unlocks Apple Silicon + AMD, needs non-Linux CI |
| #957 | SPLADE/reranker preset registry | MEDIUM | MEDIUM | keep — enables SPLADE-Code A/B without env flips |
| #958 | `define_query_categories!` macro | MEDIUM | MEDIUM | fix — closes silent `_ => 1.0` fallback class |
| #959 | Collapse notes dispatch | MEDIUM | EASY | fix — body text says `unreachable!()` but code is `bail!()` (stale) |
| #960 | Per-LanguageDef structural patterns | MEDIUM | MEDIUM | fix — 4 pattern lists, 6 languages, data-driven |
| #966 | Stream-hash enrichment (blake3) | MEDIUM | EASY | fix — ~100MB allocator pressure drop on 100k-chunk reindex |
| #969 | Recency-based `last_indexed_mtime` prune | MEDIUM | EASY | fix — drop O(n) stat syscalls on WSL watch path |
| #971 | HNSW self-heal dirty-flag integration test | MEDIUM | EASY | fix — pins rebuild-loop prevention |
| #974 | onboard + where retrieval-content assertions | **HIGH** | MEDIUM | fix — guards agent-facing retrieval regression |
| #975 | Always-on recall + staleness mtime semantics | **HIGH** | MEDIUM | fix — prevents silent retrieval regressions + backup-restore corruption |

## Recommended priority order

**Tier 1 (ship next):**
1. **#917** streaming SPLADE serialize (HIGH/MEDIUM — user-visible peak-memory win)
2. **#974** onboard/where content assertions (HIGH/MEDIUM — prevents agent-facing regressions)
3. **#975** always-on recall + mtime semantics (HIGH/MEDIUM — CI recall ceiling)
4. **#954** grammar-less parser dispatch (MEDIUM/EASY — closes a silent-routing class)
5. **#959** collapse notes dispatch (MEDIUM/EASY — eliminates a bug class that already hit us once in PR #945)
6. **#966** stream-hash enrichment (MEDIUM/EASY — allocator pressure)
7. **#969** recency-based mtime prune (MEDIUM/EASY — WSL watch responsiveness)
8. **#971** HNSW self-heal test (MEDIUM/EASY — pins invariant)
9. **#951** README perf table refresh (MEDIUM/EASY — pure measurement)

**Tier 2 (bundle into audit-v1.26.0 wave):**
10. **#955** ChunkType hints compile-enforced (MEDIUM/MEDIUM)
11. **#958** `define_query_categories!` macro (MEDIUM/MEDIUM)
12. **#960** per-LanguageDef structural patterns (MEDIUM/MEDIUM)
13. **#957** SPLADE/reranker preset registry (MEDIUM/MEDIUM)

**Tier 3 (deferred / blocked):**
14. **#956** ExecutionProvider CoreML/ROCm (HIGH/HARD — needs non-Linux CI)
15. **#255** pre-built reference packages (HIGH/HARD — open design question)
16. **#717** HNSW mmap (MEDIUM/HARD — needs hnsw lib swap)

**Stale / close:**
17. **#63** paste unmaintained — close, audit.toml already ignores it
18. **#921** SPLADE save blocks watch — rescope to "streaming save perf" or close; watch-loop claim not reproducible

**Tracking-only:**
19. **#106** ort RC — keep open, no upstream stable available
20. **#916** mmap SPLADE body — keep but depriorotize; owned-parse dominates residency

## Appendix: impact × difficulty grid

|  | EASY | MEDIUM | HARD |
|---|---|---|---|
| **HIGH** | — | #917, #974, #975 | #956, #255 |
| **MEDIUM** | #106, #951, #954, #959, #966, #969, #971 | #955, #957, #958, #960 | #717 |
| **LOW** | #63 | #916, #921 | — |

Nine items land in HIGH or MEDIUM-impact × EASY difficulty — high-ROI candidates for the next sprint. The tier-3 HARD items (#956, #255, #717) are blocked on external dependencies (ORT providers, signing infrastructure, hnsw library) and should not absorb main-thread effort until unblocked.
