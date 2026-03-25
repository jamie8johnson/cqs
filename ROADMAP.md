# Roadmap

## Current: v1.4.2

v1.4.2: Contrastive LLM summaries, FTS path filter fix, 34 adversarial tests, enriched hard eval, CI Node.js 24. Cross-GPU eval finding: LoRA trades hard eval precision for benchmark recall (base 90.9% → v7 83.6% R@1, but contrastive summaries recover to 92.7%). v8-keydac training in progress.

### 1.0.x Highlights

- v1.0.5: ASP.NET Web Forms (51st language), Make → Bash injection, schema v12 (`parent_type_name`)
- v1.0.6: SQ-2 richer NL descriptions (+3.7pp R@1 on hard eval)
- v1.0.7: SQ-4 call-graph-enriched embeddings (two-pass, IDF callee filtering)
- v1.0.8: 14-category audit — 14 findings fixed
- v1.0.9: SQ-5 module-level context (filename stems with generic filter)
- v1.0.10: Red team audit — 7 findings fixed (HNSW ID desync, PDF script injection, path traversal)

### Next — Commands

- [x] `cqs blame` — semantic git blame. Given a function, show who last changed it, when, and the commit message. Combines call graph with git log.
- [x] `cqs chat` — interactive REPL for chained queries. Readline, history, tab completion. Wraps batch mode.

### Next — Performance

- [x] PF-5: Lightweight HNSW candidate fetch (#510) — fetch only `(id, embedding)` for scoring, load full content only for top-k survivors.

### Next — Expansion

- [x] Pre-built release binaries (GitHub Actions) — adoption friction

### Future Languages — Priority Order

- [x] **Elixir** — Module + Macro exist. defprotocol → Interface, defimpl → Object. Clean mapping.
- [x] **Erlang** — FP + modules, behaviour → Interface, record → Struct.
- [x] **Haskell** — data → Enum, newtype → Struct, type synonym → TypeAlias, class → Trait, instance → Object.
- [x] **OCaml** — FP + modules. Uses `LANGUAGE_OCAML` export.
- [x] **Julia** — Scientific + types.
- [x] **Gleam** — FP + types.
- [x] **CSS** — Selectors + rules. Rule sets → Section.
- [x] **Perl** — Subs + packages. OOP via bless.
- [x] **HTML** — Semantic elements, script/style modules, landmark sections.
- [x] **JSON** — Top-level key-value pairs as Property.
- [x] **XML** — Top-level elements as Struct.
- [x] **INI** — Sections as Module, settings as Property.
- [x] **Nix** — Bindings with function/attrset expressions. Call graph via apply_expression.
- [x] **Make** — Targets as Function, variable assignments as Property.
- [x] **LaTeX** — Sections, commands, environments.
- [x] **Solidity** — Contracts, interfaces, libraries, call graph. Expression supertype workaround.
- [x] **CUDA** — Reuses C++ queries. Kernel-specific stopwords.
- [x] **GLSL** — Reuses C queries. Shader-specific stopwords.
- [x] **Svelte** — `tree-sitter-svelte-next`. Injection: `script_element/raw_text→JS`, `style_element/raw_text→CSS`. Reuses HTML helpers.
- [ ] **Clojure** — Blocked: `tree-sitter-clojure` 0.1.0 requires tree-sitter ^0.25, incompatible with 0.26.
- [ ] **Dart** — Blocked: old tree-sitter API (pre-0.24). Property covers properties, mixin → Trait.
- [x] **Razor/CSHTML** — `tris203/tree-sitter-razor` (git dep, forked). Monolithic grammar: C# + HTML + Razor directives. JS/CSS injection via `_inner` content mode.
- [x] **VB.NET** — `CodeAnt-AI/tree-sitter-vb-dotnet` (git dep, forked). Classes, modules, structures, interfaces, enums, methods, properties, events, delegates.
- [ ] **ArchestrA QuickScript** — No tree-sitter grammar exists. Needs custom grammar from scratch (VB-like syntax).

### ChunkType Variant Status

20 variants shipped. Recent additions (PR #662, #663):

| Variant | Used by |
|---------|---------|
| `Extension` | Swift, Objective-C (categories), F#, Scala 3 |
| `Constructor` | Python, Java, C#, Kotlin, Swift, VB.NET, Rust, Go, C++, PHP, Razor |
| `Constant` | Rust, Go, C, C++, Gleam, Ruby, PHP, GLSL, Python, JavaScript, TypeScript, Java, Erlang, Bash, R, Lua |
| `Event` | C#, VB.NET, Solidity |

Infrastructure for adding variants is cheap: enum arm + Display/FromStr + is_callable + nl.rs + capture_types in parser.

**Coverage gaps fixed (PR #662):** Python/JS/TS constants, Solidity events, Java static final → Constant, Erlang -define() → Macro, Bash readonly → Constant.

**Language improvements (PR #663):** R: S4/R6 classes + UPPER_CASE constants. Lua: UPPER_CASE constants.

### Multi-Grammar Parsing

Injection framework shipped in v0.27.0 (PRs #540, #544). `InjectionRule` on `LanguageDef`, `parse_file_all()` combined method for single-pass chunk + relationship extraction via `set_included_ranges()`.

**Done:**
- [x] HTML → JavaScript (with TypeScript detection via `lang`/`type` attrs)
- [x] HTML → CSS
- [x] PHP → HTML → JS/CSS — recursive injection (depth limit 3). Two injection rules: `program/text` (leading HTML) + `text_interpolation/text` (HTML after `?>`). `content_scoped_lines` prevents container-spans-file problem.
- [x] Svelte → JS/TS, CSS — `tree-sitter-svelte-next`. Reuses HTML's `detect_script_language` for TypeScript detection.
- [x] LaTeX → code listings — `minted_environment` + `listing_environment`. Language detection from `\begin{minted}{python}` and `[language=Rust]` options.
- [x] Nix → Bash — `indented_string_expression` in shell contexts (buildPhase, installPhase, shellHook, etc.). `detect_nix_shell_context` checks parent binding name.
- [x] HCL → Bash — `heredoc_template` with shell identifiers (EOT, BASH, SHELL, etc.). `detect_heredoc_language` checks heredoc identifier.
- [x] Make → Bash — `recipe/shell_text` injection. Extracts shell commands from recipe bodies.
- [x] Razor → JS/CSS — `_inner` content mode for grammars without named content children. `detect_razor_element_language` for script/style elements.

**Next — New grammars required:**
- [x] Vue (.vue) → JS/TS, CSS, HTML — `tree-sitter-vue-next`. Identical injection pattern to HTML/Svelte. Post-processing: headings, landmarks, setup script detection.

**Next — Medium value (narrower scope):**
- [x] Markdown → fenced code blocks — custom line scanner + per-block tree-sitter parse. `extract_fenced_blocks()` + `parse_fenced_blocks()` in parser/mod.rs.
- ~~YAML → Bash~~ — closed: bash chunk query only captures `function_definition` nodes; GHA `run:` blocks are bare commands, so injection would produce zero chunks.

**Lower priority (niche or fragile):**
- [ ] Astro (.astro) → JS/TS + HTML — needs grammar
- [ ] ERB (.erb) → Ruby in HTML — needs grammar
- [ ] EEx/HEEx (.eex, .heex) → Elixir in HTML — needs grammar
- [ ] SQL in string literals (Rust, Python, Go, Java) — fragile detection
- [ ] GraphQL in tagged templates (JS/TS) — fragile detection
- [ ] CSS-in-JS (styled-components, emotion) — template literal detection

### Next — Search Quality (large corpus)

Stress eval against real codebases (cqs 2956 chunks, Flask, Express, Chi) showed MRR drops from 0.91 (fixture-only) to 0.46 (3969 chunks). Rust MRR = 0.000. NL descriptions are too generic to discriminate in large corpora.

- [x] SQ-1: Adaptive name_boost — sweep proved ineffective at scale. Dead end.
- [x] SQ-2: Richer NL descriptions — field names, dir-only file context. +3.7pp R@1 on hard eval (v1.0.6).
- [ ] SQ-3: Code-specific embedding model — evaluate UniXcoder, CodeBERT, or fine-tuned E5 as replacement for general-purpose E5-base-v2.
- [x] SQ-4: Call-graph-enriched embeddings — two-pass index with IDF callee filtering. 63% of chunks enriched (v1.0.7).
- [x] SQ-5: Module-level context in NL — filename stems with generic filter (11 stems: mod, index, lib, main, utils, helpers, common, types, config, constants, init). Regresses fixture eval ~3pp but improves real queries — shipped in v1.0.9.
- [x] SQ-6: LLM-generated function summaries — one-sentence purpose summary per function via small LLM at index time. Cached, regenerated on content change. Breaks local-only constraint; high accuracy. Batch resume on interrupt (v1.0.14).
- [x] SQ-8: LLM doc comment generation (PR #627). `--improve-docs` flag, per-language DocWriter, bottom-up source rewriting.
- [x] SQ-9: Simplify notes + embeddings architecture. Done in v1.1.0 — notes as annotations, 769→768-dim, schema v15→v16.
- [x] SQ-11: Type-aware embeddings (PR #630). Append full signature to NL. +3.6pp R@1, TS MRR +0.068.
- [x] SQ-12: Index-time HyDE query predictions (PR #631). `--hyde-queries` flag, Batches API, purpose="hyde".
- [x] SQ-7: LoRA fine-tuning of E5-base-v2. **Ship as default embedding model.**
  - v1-v3 regressed hard eval (adversarial confusable pairs — not realistic usage).
  - v3 on CoIR: +4.3pp NDCG@10, +0.5pp cosqa transfer. Real queries are diverse like CoIR, not adversarial.
  - v4 (166k/3ep): over-specializes, CosQA drops to 0.305
  - **v5 (166k/1ep): best overall** — 0.683 CSN, 0.348 CosQA. Strictly better than shipped v3.
  - All variants use same 186k training data (CSN + docstring pairs). Differences are `--max-samples` and `--epochs`.
  - **Action:** Switch default from v3 to v5. Convert safetensors → ONNX, upload to HuggingFace.
  - **Plan:** Upload best merged ONNX to HuggingFace as default model. Env var override to fall back to base E5.
  - Hard eval regression is acceptable — the adversarial scenario (6 confusable sorting functions) almost never happens in real usage.
  - **Training plan (v5):** 1.7M CSN, checkpoint after each epoch. Eval at epoch 1 (~5.5 hrs), decide whether to continue to epoch 2-3 by resuming from checkpoint. Avoids 16-hour blind run.

- [x] SQ-10: Fine-tune code-specific cross-encoder reranker. **Result: REGRESSION.**
  - Trained on 50k CSN + 7.5k docstring pairs, 3 epochs. ONNX at jamie8johnson/code-reranker-v1.
  - Web-trained reranker: -10.9pp R@1. Code-trained: -81.8pp (catastrophic collapse).
  - Root cause: random same-language negatives too easy for cross-encoders.
  - Infrastructure kept: `CQS_RERANKER_MODEL` env var, eval harness in model_eval.rs.
  - Do NOT make reranking default. Revisit with hard negatives (V2) if warranted.

### Potential quality improvements (research backlog)

Ranked by difficulty / likely impact. 8 experiments + CoIR benchmark completed. Key lesson: different techniques help different eval regimes.

| # | Approach | Difficulty | Impact | Status |
|---|----------|-----------|--------|--------|
| 1 | **Weighted multi-signal fusion** | Easy | None (hard eval) | **Done (Exp 6)** — all 30 configs regress on hard eval |
| 2 | **Type-aware embeddings (SQ-11)** | Easy | +3.6pp R@1 | **Done (Exp 7, PR #630)** — first positive result. TS MRR +0.068. |
| 3 | **HyDE query predictions (SQ-12)** | Medium | Mixed | **Done (PR #631)** — shipped as `--hyde-queries`. Mixed on hard eval, neutral on stress eval. Untested on CoIR. |
| 4 | **LoRA fine-tuning (SQ-7)** | Medium | +4.3pp CoIR | **Done (v3)** — regresses hard eval but +4.3pp on CoIR. v4 training. |
| 5 | **Hard negative reranker (V2)** | Medium | Unknown | V1 failed (random negs). BM25 top-k negatives may fix. Untested on CoIR. |
| 6 | **Contrastive discriminating summaries** | Medium | +2-4pp est | Feed top-3 similar names to LLM: "unlike X, this function..." Exp 8 contrastive was +18pp vs +16pp. Needs: Store neighbor lookup, batch pipeline plumbing. |
| 7 | **Algorithm/pattern detection in NL** | Medium | +1-3pp est | Tree-sitter structural features (loops, recursion, data structures) in NL text. |
| 8 | **ColBERT late interaction** | Hard | Potentially high | Token-level matching. New index structure. Not started. |

**Evaluated and rejected:**
- **CodeSage-large-v2** — 94.26 on CSN but 20% R@1 on hard eval. Code-native model can't parse NL queries.
- **Consistency filtering** — CSN is already clean (0 pairs filtered). LoRA regressions aren't from noise.
- **Mixed LoRA (v6)** — CSN+CosQA+SO training dilutes CSN signal without improving CosQA. Our docstring pairs > CoIR training splits.

**Done:**
- Full CoIR pipeline run — enrichment hurts (-4.5pp). Product feature, not benchmark trick.
- v5 → default — ONNX converted, uploaded to HuggingFace, model card updated.

**Done (training improvements):**
1. **Hard negative mining** — 1.89M pairs mined, 65% got hard negatives. GPU FAISS, CoRNStack recipe.
2. **9-language training data** — CSN 6 + Stack Rust 56k, TS 58k, C++ 63k.
3. **v7 (unbalanced, GIST+Matryoshka)** — 200k subsample. **Best model.** CoIR 49.19, CSN 0.707, hard eval 89.1% (matches base). Shipped v1.3.1.
4. **v7b balanced** — 414k (46k/lang × 9). **No improvement.** CSN -0.5pp vs v7. Balance doesn't help NL→code.
5. **Resume-from-checkpoint** — `--resume-from-checkpoint` added to `train_lora.py`.
6. **ONNX opset-11 export** — weight injection into base E5 graph. Integrated into `train_lora.py --export-onnx`.

**Next (training — plans written, ready to execute):**
7. **KeyDAC query augmentation** ($0, ~1h code + 14-21h train) — keyword-preserving query rewrites. Preserve function name/param tokens, modify surrounding words (delete/swap/synonym). 200k pairs → ~600k augmented. Teaches phrasing robustness. Plan: `docs/superpowers/plans/2026-03-24-keydac-augmentation.md`. Python script in training-data repo.
8. **Contrastive discriminating summaries** ($0, ~1.5h code) — brute-force cosine on embeddings to find top-3 neighbors, pass to LLM prompt: "unlike X, this function..." Index-time improvement in Rust cqs binary. Plan: `docs/superpowers/plans/2026-03-24-contrastive-summaries.md`.
9. **LLM summary augmentation for training** (~$38) — generate discriminating summaries for 200k training pairs, add as additional (summary, code) query pairs. Enriches query side only. Script: `augment_with_summaries.py`.

**Later:**
10. **KD-LoRA distillation** (~12h on A6000) — distill CodeSage-large (1.3B, 64.18 CoIR) into E5-base (110M) via LoRA. Potentially largest single quality jump.
11. **Language-specific LoRA adapters** — if improvements plateau. LoRACode approach.
12. **Call-graph enriched training data** — clone repos, extract with structural context.
13. **Publish training dataset to HuggingFace** — after confirming final dataset composition.
14. **Agent task eval** — telemetry (CQS_TELEMETRY=1) collecting data.

**Done:**
- Sample size sweep (10k/50k/166k at 1ep) — 166k is optimal, more data at 1ep beats less data
- Discriminating descriptions — shipped in v1.2.0, +16pp R@1

**Other ideas (lower priority):**
- **Verified HF eval results** — run CoIR eval via HF Jobs + inspect-ai for cryptographic `verifyToken`. Requires CoIR benchmark datasets to have `eval.yaml` registered. Unverified results already uploaded.
- **Query expansion** — synonym table or small LLM. Cheap recall boost. No model changes.
- **SPLADE** — sparse learned retrieval. Could replace/augment FTS5.
- **GNN on call graph** — embed by call graph position. Marginal over SQ-4 text enrichment.
- **Mixed LoRA** — train on CSN + cosqa + SO for generalist adapter (prevents over-specialization).

### CoIR Benchmark Progress

**CoIR** (ACL 2025): 10 datasets, 8 tasks, 14 languages, 2M docs. Standard code retrieval benchmark.

**Results (CodeSearchNet, 6 languages):**

| Config | Avg NDCG@10 | vs Base | Leaderboard context |
|--------|-------------|---------|---------------------|
| Base E5-base-v2 | 0.627 | — | #7 on leaderboard (50.9 overall) |
| E5 + NL enrichment | 0.626 | -0.001 | Heuristic too crude for CoIR |
| **E5 + LoRA v3** | **0.671** | **+0.043** | Approaching #5-6 territory |
| E5 + LoRA v4 | 0.680 | +0.053 | Over-specializes (Python 0.971, CosQA drops) |
| E5 + LoRA v5 | 0.678 | +0.051 | Best CosQA transfer (0.348) |
| E5 + LoRA v6-mixed | 0.644 | +0.017 | CSN+CosQA+SO dilutes signal |
| E5 + Pipeline (v3+enrichment) | RUNNING | — | Full 10-task run in progress |

**Transfer (cosqa, out-of-distribution):** LoRA v3 +0.5pp, v5 +1.9pp, v4 -2.4pp (over-specialized).

**Leaderboard (13 models):** #1 SFR-Code-2B (67.41, 2B), #2 CodeSage-large-v2 (64.18, 1.3B), #5 Voyage-Code-002 (56.26), #6 E5-Mistral (55.18, 7B), **#7 E5-base-v2 (50.9, 110M)**.

See `docs/research-log.md` for full experiment history and next steps.

### Literature survey (before paper)

- **CoIR benchmark** — running, first results in. Adapter at `~/training-data/run_coir.py`.
- **CoRNStack** — large-scale contrastive training, claims SOTA. Compare methodology.
- **CodeXEmbed** (COLM 2025) — generalist code embedding family.
- **C2LLM** (arXiv 2512.21332) — contrastive code LLMs (0.5B, 7B).
- **CodeCSE** (arXiv 2407.06360) — multilingual code/comment sentence embeddings.
- **Refining embeddings with PEFT** (arXiv 2405.04126) — LoRA on CodeT5+. Closest to our approach.
- **Lore** (arXiv 2603.15566) — git commit messages as structured knowledge for AI agents.

### Production Stack (what ships in cqs)

| Layer | Feature | Status | Cost | Impact |
|-------|---------|--------|------|--------|
| 1 | Type-aware signatures (SQ-11) | Shipped (PR #630) | Free | +3.6pp R@1 |
| 2 | Call graph enrichment (SQ-4) | Shipped (v1.0.7) | Free | 63% of chunks enriched |
| 3 | LLM summaries (SQ-6) | Shipped (v1.0.14) | ~$0.15/3k fn | High for undocumented code |
| 4 | **LoRA embedding model** | **Next: ship as default** | Free (baked in) | +4.3pp CoIR NDCG@10 |
| 5 | Hyde predictions (SQ-12) | Shipped, optional | ~$0.15/3k fn | Optional enrichment |

### Paper thesis

"Different evaluation regimes surface different quality dimensions. Adversarial evals (confusable function pairs) test precision — type-aware embeddings dominate. Realistic benchmarks (CoIR) test recall and ranking — LoRA fine-tuning dominates. A layered architecture — signatures for precision, LoRA for recall, LLM enrichment for coverage — lets a 110M model compete with specialized models 3-20x larger."

### v1.1.0 Release Plan

**Execution order:**

1. **SQ-9: Notes simplification + 769→768-dim** (in progress — plan at `docs/superpowers/plans/2026-03-19-sq9-notes-simplification.md`)
   - Phase 1: Remove notes from search results
   - Phase 2: Drop sentiment dimension (769→768)
   - Phase 3: Schema v15 migration + reindex required
2. **P3 deferred audit fixes:** EX-6/EX-7 (Pattern/ChunkType macros), CQ-13 (shared test fixtures), PERF-11/13/16 (batch INSERT, llm allocations)
3. **P4 refactors:**
   - PERF-12: CAGRA lazy rebuild (stop rebuilding index after every search)
   - CQ-11: Extract `Store::open_with_config()` (80% duplication between open/open_readonly)
   - EX-8: Shared CLI/batch arg structs via `#[command(flatten)]`
   - Split `search.rs` (2576 lines) → `search/` module (scoring, finalize, orchestration)
   - Extract enrichment pass from `cli/pipeline.rs` into own file
   - Extract ORT provider setup from `embedder.rs` into own module
   - DS-9: Watch mode Store re-open (OnceLock cache staleness)
   - RM-18: BatchContext reference LRU eviction
   - EX-9: LLM config env/config overrides (CQS_LLM_MODEL, CQS_API_BASE)
   - EX-11: Consolidate search scoring constants into ScoringConfig
4. **Release v1.1.0** — doc fixes:
   - "Local ML" → "Local-first ML, GPU-accelerated, optional LLM enrichment" in repo description
   - README pipeline: add Enrich step, fix dimensions, fix Describe
   - CLAUDE.md: fix notes wording ("available immediately" not "indexes immediately"), opus-only agents, complete agent command list
   - CLAUDE.md agent tools: add `plan, blame, doctor, index, stats, batch` — drop `chat, completions, init, watch`
   - Bootstrap skill: sync agent tools list, fix `--json`/`--format json`, add `--include-refs`, add missing skills, opus-only
   - All 769→768 dimension references across README, PRIVACY, SECURITY, CONTRIBUTING, CLAUDE.md, lib.rs
   - Re-run eval benchmarks or qualify numbers with measured version
   - Update Cargo.toml version to 1.1.0

### Parked

- **MCP server** — re-add as slim read-only wrapper when CLI features are rock solid. Architecture proven clean (removed in v0.10.0 with zero core changes).
- **Pre-built reference packages** (#255) — `cqs ref install tokio`
- ~~**Index encryption**~~ — closed: use OS-level disk encryption (BitLocker/LUKS/FileVault). sqlx doesn't support SQLCipher natively; not worth the complexity.
- ~~**Query-intent routing**~~ — closed: `--ref` flag covers explicit scoping, and hybrid RRF already boosts keyword matches naturally.
- ~~**Pattern mining**~~ — closed: manual notes + `cqs suggest` cover practical needs. Automated AST pattern recognition is research-grade effort for uncertain payoff.
- **Post-index name matching** — fuzzy cross-doc references

### Red Team — Accepted/Deferred

Findings from v1.0.10 red team audit. Accepted as trade-offs — each needs upstream API changes or schema work to fix.

- RT-DATA-2: Enrichment no idempotency marker (medium — needs schema change)
- RT-DATA-3: HNSW orphan accumulation in watch mode (medium — no deletion API)
- RT-DATA-5: Batch OnceLock stale cache (medium — by design, restart to refresh)
- RT-DATA-6: SQLite/HNSW crash desync (medium — needs generation counter)
- RT-DATA-4: Notes file lock vs rename race (low)

### Open Issues

**External/Waiting:**
- #106: ort stable (currently 2.0.0-rc.12)
- #63: paste dep unmaintained (RUSTSEC-2024-0436) — transitive via `tokenizers`, waiting on HuggingFace to switch to `pastey`

**Feature:**
- #255: Pre-built reference packages
- #555: EX-4 `where_to_add` catch-all for 44 languages (P4, extensibility)

**Infrastructure:**
- #389: CAGRA CPU-side dataset retention (~146MB at 50k chunks) — cuVS `search()` consumes the index, so `dataset` is needed for rebuild. Blocked on upstream API change.

## 1.0 Release Criteria

- [x] Schema stable for 1+ week of daily use (v12 since 2026-03-13)
- [x] Used on 2+ different codebases without issues (cqs, aveva, rust)
- [x] No known correctness bugs

1.0 means: API stable, semver enforced, breaking changes = major bump.

---

*Completed phase history archived in `docs/roadmap-archive.md`.*
