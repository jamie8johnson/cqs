# Project Continuity

## Right Now

**7th audit complete. P1+P2 fixed (all 32 items). P3 next. (2026-03-27)**

### Active
- **Audit v1.7.0**: 95 findings, P1 (12) + P2 (18) + deferred P2 (4) = 34 fixed. P3 (48) + P4 (10) remain.
  - `docs/audit-findings.md` — all findings
  - `docs/audit-triage.md` — P1-P4 classification (needs status update)
  - ~100 files modified, uncommitted on main
- **Key fixes this session**:
  - Multi-model support: `--model` flag threaded through all 20 commands, HNSW uses `store.dim()`, Store skips model validation on open
  - dim=0 validation at ModelConfig, Store, HNSW
  - Batch data safety: `resume()` returns `valid_results`, hash failure skips storage
  - `nl.rs` split into `nl/{mod,fts,fields,markdown}.rs`
  - `BatchSubmitItem` struct replaces opaque 4-tuple
  - `create_client()` factory + `LlmProvider` enum
  - `skip_line_prefixes` data-driven on all 51 languages
  - 14 new tests (TC-31 dim threading, TC-32 batch mock)
  - `Store::dim` → private field + getter
  - `DEFAULT_MODEL_REPO` single source of truth
- **Gap-filling pipeline**: Status unknown (was running 2026-03-26)
  - Processing manifest added: `~/training-data/processing_manifest_retroactive.jsonl` (2,305 repos)

### Pending
1. **Commit P1+P2 fixes** — ~100 files, needs branch + PR
2. **Fix P3** (48 easy items) — docs, observability, robustness, performance, tests
3. Check gap-filling pipeline → assemble 200K → publish HF
4. Train v9-200k
5. Paper v0.6

## Parked
- Dart language support (guide written)
- Curriculum scheduling (v9-full)
- Ship v9-mini as default (matches base enriched, better raw+CSN)
- BGE-large eval (multi-model P1 fixes make it possible now)

## Open Issues
- #389, #255, #106, #63 (all blocked on upstream)

## Architecture
- Version: 1.7.0
- Models: E5-base default, BGE-large preset, custom ONNX (multi-model now functional)
- ModelConfig: CLI > env > config > default, resolved once in dispatch
- LlmProvider: Anthropic (extensible to OpenAI via CQS_LLM_PROVIDER)
- EMBEDDING_DIM: runtime via `store.dim()` getter (private field)
- DEFAULT_MODEL_REPO: single source of truth in `embedder/models.rs`
- Languages: 51 (all with `skip_line_prefixes` for data-driven field extraction)
- nl.rs → nl/{mod,fts,fields,markdown}.rs (4-file split)
- Tests: 1480
