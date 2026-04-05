# Roadmap

## Current: v1.15.2

v1.15.2: 10th audit fully cleared (103/103 findings fixed). Typed JSON output structs. 52 languages + L5X/L5K PLC exports. ~2250 tests.

### Expanded Pipeline Eval (296 queries, 7 languages, 2026-03-31)

Config A (Cosine-only, best config). A6000. 77 hard + 219 holdout queries across Rust, Python, TypeScript, JavaScript, Go, Java, PHP.

| Model | Params | R@1 | R@5 | MRR | Raw R@1 | CoIR |
|-------|--------|-----|-----|-----|---------|------|
| **BGE-large FT** | 335M | **91.6%** | 99.3% | **0.9521** | 66.2% | **57.5** |
| **BGE-large** | 335M | 90.9% | 99.3% | 0.9493 | 61.8% | 55.7 |
| **v9-200k** | 110M | 90.5% | 99.3% | 0.9482 | **70.9%** | 52.7 |
| v9-200k-hn | 110M | 82.4% | 99.3% | 0.9033 | 70.9% | 0.614 |
| v9-200k-testq | 110M | 82.1% | 99.3% | 0.9025 | 70.9% | — |
| v9-175k | 110M | 82.1% | 99.0% | 0.9010 | 70.9% | 0.619 |
| v9-200k-1.5ep | 110M | 81.4% | 99.3% | 0.8995 | 70.9% | 0.607 |
| v9-500k | 110M | 81.1% | 99.3% | 0.8971 | 70.9% | 0.629 |
| contrastive-B | 110M | 81.1% | 99.3% | 0.8954 | 70.9% | 0.689 |
| E5-base | 110M | 75.3% | 99.0% | 0.8688 | 49.1% | 0.627 |

**Key findings from expanded eval:**
- BGE-large and v9-200k are virtually tied (90.9% vs 90.5%). The old 55-query eval exaggerated the gap.
- Basin now at ~81-82% (was 89.1% on narrow eval). v9-200k still clearly separates from the pack.
- RRF hurts at this scale (74.7% vs 90.9% cosine-only). Pure embedding search is best.
- Enrichment stack contributes ~15pp (E5-base 75.3% vs BGE-large 90.9% off-the-shelf).

### Done — v1.9.0: BGE-large Default
- [x] Switch `DEFAULT_MODEL_REPO` to BGE-large
- [x] `ModelConfig::default_model()` single source of truth + consistency test
- [x] `EMBEDDING_DIM` = 1024, all tests dim-agnostic
- [x] E5-base as lightweight preset (`CQS_EMBEDDING_MODEL=e5-base`)
- [x] Released v1.9.0, published crates.io

### Done — Training (Exp 19: 110M model improvement)

55-query eval numbers (historical). See expanded eval table (296 queries) at top for definitive results.

| Experiment | Data | Hard Negs | Epochs | 55q R@1 | Raw R@1 | Result |
|-----------|------|-----------|--------|---------|---------|--------|
| **v9-200k** | **200K** | **CG only** | **1** | **94.5%** | **70.9%** | **Winner — ties BGE-large** |
| v9-200k-hn | 200K | CG + FAISS | 1 | 89.1% | 70.9% | FAISS hurts pipeline |
| v9-500k | 500K | CG only | 1 | 89.1% | 70.9% | More data hurts pipeline |

- [x] Mine FAISS hard negatives (172K pairs, 6.8 avg negs)
- [x] Train v9-200k → **94.5% pipeline, 70.9% raw**
- [x] Train v9-200k-hn → 89.1% pipeline (FAISS negs regress -5.4pp)
- [x] Train v9-500k → 89.1% pipeline (more data regresses -5.4pp)
- [x] v9-200k-1.5ep → 89.1% pipeline (more epochs regresses -5.4pp). Raw flat (70.9%).
- [x] Contrastive-B (25% contrastive queries from call graph) → 89.1% pipeline (-5.4pp). Fifth confirmation.

**Next — Data size sweep (is 200K a peak or a plateau?):**
- [x] v9-175k — 82.1% R@1, 0.9010 MRR. Confirms per-language saturation is sharp at ~22K/lang.
- [ ] v9-225k (225K assembled from 500K pool, 25K/lang — ready to train)
- [ ] v9-100k, v9-150k (assembled, lower priority — run if 175K reveals a gradient)

Known (expanded eval, 296 queries): v9-mini (50K, 11K/lang) → basin, v9-175k (19.4K/lang) → 82.1%, v9-200k (22K/lang) → **90.5%**, v9-500k (55K/lang) → 81.1%.

**Result:** Saturation is sharp at ~22K/lang. v9-175k (19.4K/lang) hit 82.1% on expanded eval — firmly in the basin. v9-200k (22K/lang) is the only variant that escapes at 90.5%. The magic number is ~22K per language, not 200K total.

**Follow-up if plateau found:** Imbalanced 200K (30K Python/Rust + 10K others) tests whether per-language saturation varies. Lower priority given the sharp threshold result.

**Current best:** 200K x 1 epoch x CG-filter-only (v9-200k). On expanded eval (296 queries): 90.5% R@1, virtually tied with BGE-large (90.9%). Five independent perturbations all land in the basin (~81-82% expanded). Contrastive-B produced best CSN (0.689, +7.4pp) while hitting same basin — confirming basin = "generic retrieval" regime.

**The basin of attraction (55-query eval: 89.1%, expanded eval: ~81-82%):**
| Variant | Change | 55q R@1 | 296q R@1 |
|---------|--------|---------|----------|
| v9-200k | baseline | **94.5%** | **90.5%** |
| v9-500k | 2.5x more data | 89.1% | 81.1% |
| v9-200k-hn | + FAISS hard negatives | 89.1% | 82.4% |
| v9-200k-1.5ep | 1.5x more epochs | 89.1% | 81.4% |
| contrastive-B | 25% contrastive queries | 89.1% | 81.1% |
| v9-175k | 12.5% less data | — | 82.1% |

### Future — Training Signal Experiments (from paper Section 5.5)

7 experiments tested. All land in the basin. The expanded eval (296 queries) confirms the pattern at scale — the gap narrows from 5.4pp (55q) to ~8-9pp (296q) but the separation is still clear.

**New direction: improve the enrichment stack or eval, not training data.**

### Done — Fine-Tune BGE-large on 200K CG-Filtered Data (Exp 27, 2026-04-03)

**Result: Outcome 1 — model capacity + training signal is additive. Did not basin.**

| Eval | FT | Baseline | Delta |
|------|-----|---------|-------|
| Fixture R@1 (296q) | **91.6%** | 90.9% | +0.7pp |
| Raw R@1 (55q) | **66.2%** | 61.8% | +4.4pp |
| Real R@1 (100q) | 50.0% | 50.0% | 0pp |
| CoIR (19-subtask) | **57.5** | 55.7 | +1.8pp |
| CoIR CSN | **0.779** | 0.721 | +5.8pp |

Training: 200K CG-filtered + GIST + Matryoshka, 1 epoch, 12.75h on A6000. Published: [jamie8johnson/bge-large-v1.5-code-search](https://huggingface.co/jamie8johnson/bge-large-v1.5-code-search).

Enrichment dependency slightly increased vs baseline (doc -7.5pp vs -6.8pp) but nowhere near v9-200k's 2-3×. The dramatic enrichment synergy is E5-base architecture-specific.

- [ ] **Better TypeScript sort enrichment** — the 3 missing queries are all TypeScript sort/string functions. More discriminating contrastive summaries specifically for sorting algorithms might recover them.
- [x] **Expand eval fixtures** — Done (296 queries, 7 languages). See expanded eval table at top of file. The 3-query effect is diluted; BGE-large and v9-200k are virtually tied (90.9% vs 90.5%).
- [x] ~~**Test-derived training queries**~~ — Tested (Exp 26). 13.6K LLM-generated behavioral queries from test assertions → 89.1%. Behavioral descriptions produce the same embedding space.
- [x] ~~**Contrastive summaries as training pairs**~~ — Tested (Exp 24). → 89.1%, CSN 0.689 (best LoRA).
- [ ] **Type-aware negative mining** — still untested. Changes negative *selection*, not query format. May be worth trying but 7 basin data points suggest diminishing returns on training-side changes.
- [x] **f32→f64 cosine precision fix** — Done in v1.11.0 (AC-28).
- [ ] **Imbalanced 200K** — test whether per-language saturation varies. Lower priority given the per-query analysis.

### Done — GIST Margin Sweep (Exp 28, 2026-04-05)

Swept GIST margin on E5-base with v9-200k data. All other params fixed.

| Margin | Raw R@1 (55q) | Raw MRR |
|--------|---------------|---------|
| 0.01 | 69.1% | 0.785 |
| **0.03** | **72.7%** | **0.811** |
| 0.05 (v9 baseline) | 70.9% | 0.792 |
| 0.08 | 69.1% | 0.797 |
| 0.10 | 69.1% | 0.793 |

**Null result.** Margin=0.03 gives +1.8pp raw R@1 (repeatable, deterministic eval) but pipeline R@1 is within training variance (~80-83% for all margins). The GIST paper default (0.05) is confirmed correct. v9-200k remains the production E5-base model.

**Stale baseline discovered:** The 90.5% pipeline R@1 in the expanded eval table was from an older codebase. On the current v1.15.2 index, original v9-200k scores 80.1%. All E5-base models land 80-83%. Expanded eval numbers are relative rankings, not absolute.

**Rust stress MRR collapsed to 0.046** for all E5-base models on current index. Not margin-related — needs separate investigation.

- [x] Margin sweep complete (5/6 runs, 0.15 killed — flat trend)
- [x] Pipeline eval (296q) on all margins + original v9-200k — null result
- [x] Stress eval on 0.03 + original v9-200k — both show Rust collapse
- [ ] Band mining experiment (Exp #1) — still worth testing (changes negative selection, not filter). Use margin=0.05
- [ ] Investigate Rust stress MRR collapse on v1.15.2 index

### Done — Embedding Model Options
- [x] BGE-large-en-v1.5 as configurable alternative
- [x] ModelConfig registry with per-model prefix/dim/repo
- [x] Eval: BGE-large 94.5% R@1 (55q) / 90.9% (296q) vs E5-base 83.6% (55q) / 75.3% (296q)
- [x] Multi-model verified end-to-end (init dim fix, convenience wrappers deleted)

### Done — Training (Exp 18: v9-mini)
- [x] v9-mini training — Stack data + call-graph false-negative filter + synthetic queries
- [x] Eval: 65.5% raw R@1, 89.1% enriched, 0.638 CSN

### Next — Dataset & Training
**Raw pool**: 3.7M pairs across 9 languages from ~5,000 repos. Bottleneck: C++ at 121K.
**200K dataset assembled.** 22,222 × 9, 74% callers, 93% callees.

- [x] Gap-filling complete (2,010 repos indexed, 109 failed)
- [x] 200K dataset assembled + published: https://huggingface.co/datasets/jamie8johnson/cqs-code-search-200k
- [x] 500K dataset assembled locally
- [ ] ~~Publish 500K + 1M to HuggingFace~~ (parked — waiting for training experiments to settle)
- [x] Hard negatives mined (172K pairs, 6.8 avg negs)
- [x] Training ablation complete (v9-200k / v9-200k-hn / v9-500k)
- [x] Ship v9-200k as LoRA preset in cqs (commit 94731f5, `ModelConfig::v9_200k()`, `CQS_EMBEDDING_MODEL=v9-200k`)
- [x] Paper v0.6 — thesis rewritten (training signal quality > model capacity), all numbers from single verification run
- [ ] Paper v0.7 — add 89.1% basin finding (5 data points), Exp 24 contrastive results

### Done — Red Team v1.9.0
23 findings (0 critical, 2 high, 9 medium, 11 low). All actionable items fixed (PRs #712, #713).

- [x] #708 RT-DATA-7: Watch file deletion (fixed, merged)
- [x] #709 RT-DATA-8: Watch function_calls (fixed, merged)
- [x] #710 RT-RES-1: Impact BFS capped at 10K (fixed, merged)
- [x] Non-breaking fixes: INJ-1/2/4, RES-5/7, FS-1, DATA-10 (PR #712)
- [x] #711 RT-RES-9: Diff impact cap (fixed in v1.12.0, capped at 500 changed functions)

### Future — Migrate HNSW to `hnswlib-rs` (wilsonzlin/corenn)
**NOT the same author as hnsw_rs.** Different library: `hnswlib-rs` 0.10.0 by wilsonzlin (corenn project). Owned `Hnsw<K,M>`, `VectorStore` trait, zero unsafe, Apache-2.0.

**Why:** Current `hnsw_rs` returns `Hnsw<'a>` borrowing from `&'a mut HnswIo`, forcing `UnsafeCell` + `unsafe impl Send/Sync` + `self_cell`. `hnswlib-rs` decouples graph topology from vector storage via `VectorStore` trait. Zero consumer unsafe.

**Audit (2026-03-27):** Algorithm correct (faithful hnswlib port, property-tested). Concurrency sound (two-phase insert, no deadlocks). VectorStore works for SQLite-backed vectors. String keys, cosine distance, parallel + incremental insert all supported.

**Blockers:** (1) `corenn-kernels` requires nightly Rust (`#![feature(f16)]`) — fork without it, use our simsimd distances. (2) No per-query ef_search — 1-line fix to add parameter.

**Concerns:** `load_from` allows OOM via crafted max_nodes (cap it). Single squashed commit, no LICENSE file, 3.2K downloads.

**Path:** Fork ~2,600 lines (hnsw.rs + vectors.rs), replace corenn-kernels with simsimd Metric impl, add ef_search param, cap max_nodes. ~1 day. Eliminates all 5 unsafe blocks + self_cell dep.

- [ ] Spike: fork, implement simsimd Metric, benchmark vs current
- [ ] If comparable: full migration, delete self_cell + UnsafeCell machinery

### Eval Infrastructure (research platform) — Done

| # | Item | Status |
|---|------|--------|
| 1 | Per-query diagnostics (CQS_EVAL_OUTPUT) | Done (PR #740) |
| 2 | Cross-run stability script | Done (PR #740) |
| 3 | Enrichment ablation (CQS_SKIP_ENRICHMENT) | Done (PR #740) |
| 4 | Difficulty tiers + weighted R@1 | Done (PR #740) |
| 5 | Multi-answer queries (also_accept) | Done (PR #740) |
| 6 | Real codebase eval | Done — 187 queries (100 fn lookup + 40 conceptual + 20 callgraph + 27 gitblame) |

**187-query real eval (2026-04-01):** 100 function lookup, 40 conceptual, 20 callgraph, 27 gitblame. All 3 models evaluated.

| Model | Lookup R@1 (100q) | Lookup R@5 (100q) | Conceptual Good (40q) |
|-------|-------------------|-------------------|-----------------------|
| **v9-200k** | **49%** | **71%** | 24/40 (60%) |
| BGE-large | 48% | 71% | 24/40 (60%) |
| nomic | 32% | 56% | 17/40 (43%) |

**Key finding:** R@5 of 71% is the agent-relevant metric. Fixture inflation is 42-54pp. v9-200k and BGE-large virtually tied on real code.

**Enrichment ablation:** doc +6.8pp > filecontext +4.1pp >> signatures +1.4pp >> callgraph ≈ parent +0.4pp.

**Fixture inflation:** 45-61pp depending on model and corpus size. The eval was too easy, not search too bad.

### Next — Agent Adoption
Telemetry update (2026-03-29, 72 events since v1.9.0 reset): test-map 37%, notes 15%, health 7%, search 6%. Structural commands dominate (49%). Orchestrators (task, review, plan, impact-diff) still 0%.

**Done:**
- [x] CLAUDE.md restructured with task-triggered commands
- [x] Workflow examples added ("how a good session uses cqs")

**Done:**
- [x] **Pre-Edit hook** — `.claude/hooks/pre-edit-context.py` — PostToolUse hook on Edit runs `cqs context <file>`. Shipped v1.11.0.
- [x] **`cqs task --brief`** — compact output (~200 tokens: files to touch, functions at risk, test coverage). Shipped post-v1.12.0.
- [x] **Pre-commit review** — `.claude/hooks/stop-review.sh` — hook runs `cqs review` on Stop event.

**Next:**
- [ ] **Fewer commands in prompts** — only mention `scout` and `task` (highest value, fewest choices)
- [x] **Telemetry dashboard** — `cqs telemetry` command: frequency, categories, sessions, top queries. `--reset`, `--all`, `--json`.

### 1.0.x Highlights

- v1.0.5: ASP.NET Web Forms (51st language), Make → Bash injection, schema v12 (`parent_type_name`)
- v1.0.6: SQ-2 richer NL descriptions (+3.7pp R@1 on hard eval)
- v1.0.7: SQ-4 call-graph-enriched embeddings (two-pass, IDF callee filtering)
- v1.0.8: 14-category audit — 14 findings fixed
- v1.0.9: SQ-5 module-level context (filename stems with generic filter)
- v1.0.10: Red team audit — 7 findings fixed (HNSW ID desync, PDF script injection, path traversal)

### Refactoring — Done (v1.15.1–v1.15.2, PRs #757-783)

- [x] **CommandContext struct** — 32 redundant store opens eliminated (#757)
- [x] **`cli/mod.rs` split** → `cli/store.rs` (#765)
- [x] **`pipeline.rs` split** → 6 submodules (#766)
- [x] **`store/helpers.rs` split** → 7 submodules (#768)
- [x] **Telemetry subcommands** — clap CommandFactory replaces hardcoded list (#767)
- [x] **Lazy reranker + embedder** — OnceLock in CommandContext (#769, #774)
- [x] **Commands subdirectories** — 46 files → 7 dirs (#760)
- [x] **CI rust-cache** — clippy 2m→41s (#764)
- [x] **Batch/CLI unification** — 3 phases, shared functions + token packing utilities (#771-773)
- [x] **JSON schema migration** — typed Serialize structs for all 7 command groups + lib-level types (#776-783). ~60 new serialization tests. Field normalization (line→line_start, function→name). Spec: `docs/superpowers/specs/2026-04-03-json-schema-design.md`

### Refactoring — Remaining

- [ ] **Language definition code generation** — TOML data files + `build.rs` generator for 52 language defs. Spec: `docs/superpowers/specs/2026-04-03-language-macro-design.md`
- [ ] **Docs fixes** — README CLAUDE.md block missing `reconstruct`/`brief`/`neighbors`/`affected`/`train-pairs`, language count 51→52+L5X/L5K, CONTRIBUTING.md architecture stale after file splits
- [ ] **Reranker eval config** — add Config G (Cosine + rerank) to pipeline_eval.rs for 296q fixture eval

### Next — Ladder Logic (RLL) Parser

- [ ] **Tree-sitter grammar for Rockwell ladder logic textual DSL** — L5X/L5K exports contain ladder as text in CDATA: `XIC(tag)OTE(tag);` with bracket branches `[OTE(a),OTE(b)]`. Small regular grammar (~50-80 lines). Instructions: XIC, XIO, OTE, OTU, OTL, TON, TOF, CTU, CTD, JSR, MSG, etc. Call graph via `JSR` (jump to subroutine). Same CDATA extraction pattern as ST parser. Generalizable to other vendors (Siemens STL, Mitsubishi IL) via configurable mnemonic tables.
- [ ] **Integrate into L5X/L5K parser** — extract `RLLContent` CDATA alongside `STContent`. Full-plant indexing: ST routines + ladder routines + cross-routine JSR call graph.

### Next — Cross-Project Call Graph

- [ ] **Cross-project call graph traversal** — trace calls from project A into project B's API. Currently `cqs ref` does multi-project search with weighted merge, but call graph is single-project only. Needs: cross-project edge storage, reference-aware BFS in `store/calls/`, updated `callers`/`callees`/`impact`/`trace` commands. Directly useful for PLC project (20 controllers calling shared AOIs across L5X files).

### Next — Embedding Cache (Fast Model Switching)

- [ ] **Embedding cache by content_hash + model_name** — store embeddings keyed by `(content_hash, model_name)` so switching models doesn't require recomputing embeddings. First index per model is slow, subsequent switches are instant lookups. Eliminates the reindex-on-model-switch pain for research workflows. Schema change: new `embedding_cache` table, lookup before embed, store after.

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
- [ ] **Dart** — `tree-sitter-dart` on crates.io. Highest-impact next language (Flutter ecosystem). Template in CONTRIBUTING.md "Adding a New Language" guide.
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
- ~~**Query expansion**~~ — Done (v1.11.0). Static synonym map, 31 programming abbreviations. OR-based FTS expansion.
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
| E5 + Pipeline (v3+enrichment) | Stale | — | Was in progress, status unknown |

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
| 4 | LoRA embedding model (v9-200k) | Shipped as preset (`CQS_EMBEDDING_MODEL=v9-200k`) | Free (baked in) | 90.5% R@1 (296q), ties BGE-large |
| 5 | Hyde predictions (SQ-12) | Shipped, optional | ~$0.15/3k fn | Optional enrichment |

### Paper thesis

"Different evaluation regimes surface different quality dimensions. Adversarial evals (confusable function pairs) test precision — type-aware embeddings dominate. Realistic benchmarks (CoIR) test recall and ranking — LoRA fine-tuning dominates. A layered architecture — signatures for precision, LoRA for recall, LLM enrichment for coverage — lets a 110M model compete with specialized models 3-20x larger."

### Done — v1.1.0 through v1.12.0

Shipped: SQ-9 notes simplification (769->768-dim, schema v16), ScoringConfig consolidation (EX-11), shared CLI/batch arg structs (EX-8, OutputArgs/TextJsonArgs), search module split, doc fixes, dimension corrections. See CHANGELOG for full history.

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
