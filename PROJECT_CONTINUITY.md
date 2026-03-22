# Project Continuity

## Right Now

**Training data pipeline running (2026-03-22 02:00 CDT).**

### Running
- Hard negative mining: 1.7M CSN pairs, 81.8%+ done, still in memory (32GB RSS). Writing output when complete.

### Complete this session
- v1.3.0 released (PRs #640-644, tag pushed, binaries built)
- HuggingFace: v5 model (166k/1ep) uploaded, model card updated
- Full 14-category audit: 87 findings, 75 fixed
- Full 10-task CoIR eval: 48.67 avg (#8)
- Stack extraction: Rust 56k, TypeScript 58k, C++ 63k pairs from The Stack v1
- Haiku vs Sonnet comparison: model doesn't matter for summaries
- Usage telemetry implemented (CQS_TELEMETRY=1, .cqs/telemetry.jsonl)
- GitHub issues created for unfixed audit items (#645-650)

### Uncommitted local changes
- `src/cli/telemetry.rs` — new telemetry module
- `src/cli/mod.rs` — telemetry hook in run_with
- `src/cli/pipeline.rs` — deferred type edge insertion (FK fix)
- `docs/research-log.md` — Stack extraction results, language balance
- `docs/notes.toml` — groomed (7 removed, 16 updated)
- `.github/workflows/ci.yml` — eval job summary step

### After mining finishes
1. Consistency-filter the Stack pairs with v5 model
2. Mine hard negatives for Rust/TS/C++ (same script, new data)
3. Build balanced training set: subsample per language (equal or weighted)
4. Train v7 on combined hard-neg data (9 languages)
5. Eval on hard eval + full 10-task CoIR + Rust/TS/C++ eval set
6. Reindex + re-run --improve-all (EX-13 fix: is_source_file uses registry now)

### Key decisions this session
- **v5 > v3**: shipped to HF (+1.2pp CSN, +1.4pp CosQA)
- **LoRA is specialization trade-off**: full CoIR 48.67 (#8) vs base 50.90 (#7)
- **Hard negatives may fix trade-off**: CoRNStack +9.4pp without degrading generalist tasks
- **9-language training**: Stack v1 streaming works, balanced extraction
- **Subsample per language**: prevents PHP/Java/Python from dominating
- **Agents use navigation, not search**: 0 semantic searches in audit session, mostly callers/read/context
- **Summaries are gap-fillers**: hurt well-documented code (-5.3pp), help undocumented (+1.8pp)
- **Telemetry over speculation**: measure real agent usage instead of optimizing proxy metrics

## Parked
- Paper draft (2-3 weeks from submittable — need hard neg results + controlled ablations)
- Re-run --improve-all (blocked by ORT contention, do after mining)

## Architecture
- Version: 1.3.0
- Schema: v16
- Embeddings: 768-dim E5-base-v2 LoRA v5 (166k/1ep)
- Metrics: 92.7% R@1, 0.965 NDCG@10 (hard eval, DocFirst)
- CoIR: 48.67 avg (9 tasks), CSN 0.683, CosQA 0.348
- Tests: 1290 lib pass (with gpu-index)
- Telemetry: CQS_TELEMETRY=1 in ~/.bashrc, logs to .cqs/telemetry.jsonl
