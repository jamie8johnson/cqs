# Project Continuity

## Right Now

**v1.25.0 shipped. Classifier is the next bottleneck. (2026-04-14 ~13:00 CDT)**

### Where we landed today

Three merged PRs on top of v1.24.0:

1. **#943** — eval output moved to `~/.cache/cqs/evals/` (fixes watch-reindex contamination that corrupted every prior alpha measurement).
2. **Clean 21-point alpha re-sweep** — first truly deterministic sweep; back-to-back runs are bit-exact.
3. **#944 / v1.25.0** — new per-category defaults (`identifier 0.90, structural 0.60, conceptual 0.85, behavioral 0.05, rest 1.0`) + dropped the over-broad `"how does"` pattern from `is_behavioral_query` that was catching 100% of multi_step queries and routing them to α=0.05. Transitive `rand 0.9.2 → 0.9.4` bump (GHSA-cq8v-f236-94qc alert dismissed as `not_used`).

### Release status

- Tag `v1.25.0` pushed
- Crates.io: **published** as `cqs 1.25.0` (default features; `[patch.crates-io]` cuvs git dep prevents publishing with `--features gpu-index`)
- GitHub Release workflow: running (background task `bsyim32pp`, watched via `gh run watch`). Will auto-create the GitHub Release with Linux/macOS/Windows binaries on success.
- Local binary + daemon: on v1.25.0, daemon active.

### Numbers

- Best uniform α from clean sweep: **α=0.95 → 44.9%** R@1
- Per-category oracle ceiling: **49.4%** (131/265)
- Deployed per-category routing (v1.25.0): **44.9%** — ties uniform α=0.95
- The 4.5pp oracle gap is **entirely classifier accuracy**, not alpha choice

### The classifier is the bottleneck

Confusion matrix (eval label vs `classify_query()` output) from today's check:

| eval_label | N | correctly classified | dominant misroute |
|---|---|---|---|
| negation | 29 | 100% | — |
| identifier | 50 | 84% | 5 → Unknown |
| structural | 27 | 19% | 18 → Unknown |
| type_filtered | 24 | 4% | 11 → Structural (starts with "struct "/"enum "/"trait ") |
| behavioral | 44 | 5% | 24 → Unknown |
| conceptual | 36 | 3% | 24 → Unknown |
| cross_language | 21 | 0% | 11 → Unknown, 5 → Structural |
| multi_step | 34 | 0% → fixed | was 100% → Behavioral; now split MultiStep/Unknown (both α=1.0) |

Structural/conceptual/behavioral detectors use narrow phrase/word lists that miss natural-language queries. Those fall to Unknown → α=1.0. Cross-language detection requires explicit language names, which the eval queries don't use.

### Next session priorities

1. **Classifier accuracy investigation** — expand rule set with phrasings mined from eval queries, or a small learned classifier, or LLM-first-query-cached. Worth +4.5pp if done well. Full ROADMAP entry exists.
2. **Eval expansion** — grow small categories (N=21 cross_language, N=24 type_filtered) to N≥40 so per-category decisions aren't dominated by single-query noise.
3. **Rename `evals/queries/v2_300q.json`** to its actual count (265 queries).

### Residual puzzles

- Identifier dropped 1 query (98% → 96%) and structural dropped 1 query between v1 and v2 router-fix evals with only `is_behavioral_query` changed between them. Likely SPLADE ONNX GPU non-determinism on the sparse vector output — a known residual drift source worth verifying once.

## Parked

- **CAGRA filtering regression on enriched index** (v1.24.0 investigation) — conceptual −5.5pp, structural −3.8pp, identifier −2pp vs pre-release baseline when CAGRA bitset filtering is on with the enriched graph. Options: HNSW-for-enriched + CAGRA-for-base, or bumped itopk_size, or per-filter CAGRA graphs. Blocks further R@1 gains but orthogonal to the classifier work.
- **Query-time HyDE for structural queries** — old data shows +14pp structural / +12pp type_filtered / −22pp conceptual. Needs a fresh eval with SPLADE active.
- **Reranker V2** (code-trained cross-encoder; ms-marco was catastrophic).

## PR status
- All recent PRs merged: #939, #940, #941, #942, #943, #944.
- No open PRs from this session.

## Architecture
- Version: **1.25.0**, Schema: v20
- Deterministic search path (PR #942) + deterministic eval pipeline (PR #943)
- SPLADE always-on, α controls fusion weight only
- Per-category SPLADE defaults: identifier 0.90, structural 0.60, conceptual 0.85, type_filtered 1.0, behavioral 0.05, rest 1.0
- HNSW dirty flag self-heals via checksum verification
- cuVS 26.4 + patched with `search_with_filter` (upstream rapidsai/cuvs#2019 pending)
- Eval results write to `~/.cache/cqs/evals/` (outside watched project dir)
- Daemon: `cqs watch --serve`, systemd unit `cqs-watch`

## Open Issues
- #909, #912-#925, #856, #717, #389, #255, #106, #63
