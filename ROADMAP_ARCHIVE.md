# Roadmap Archive

Completed items moved from ROADMAP.md to save context tokens. Git history has the full details.

## v1.9.0: BGE-large Default
- Switch `DEFAULT_MODEL_REPO` to BGE-large, `ModelConfig::default_model()` SSoT, dim-agnostic tests
- E5-base as lightweight preset, released v1.9.0

## Embedding Model Options
- BGE-large-en-v1.5 configurable, ModelConfig registry, multi-model verified end-to-end

## Training (Exp 18: v9-mini)
- v9-mini: 65.5% raw R@1, 89.1% enriched, 0.638 CSN

## Red Team v1.9.0
- 23 findings (0 critical, 2 high, 9 medium, 11 low). All fixed (PRs #708-#713, #711 in v1.12.0).

## 1.0.x Highlights
- v1.0.5: ASP.NET Web Forms (51st language), Make → Bash injection, schema v12
- v1.0.6: SQ-2 richer NL descriptions (+3.7pp R@1)
- v1.0.7: SQ-4 call-graph-enriched embeddings (two-pass, IDF callee filtering)
- v1.0.8: 14-category audit — 14 findings fixed
- v1.0.9: SQ-5 module-level context (filename stems with generic filter)
- v1.0.10: Red team audit — 7 findings fixed

## Commands (done)
- `cqs blame` — semantic git blame
- `cqs chat` — interactive REPL

## Performance (done)
- PF-5: Lightweight HNSW candidate fetch (#510) — id+embedding only, full content for top-k survivors

## Expansion (done)
- Pre-built release binaries (GitHub Actions) — Linux x86_64, macOS ARM64, Windows x86_64

## Search Quality (SQ-1 through SQ-12, all done)
- SQ-1: Adaptive name_boost — dead end
- SQ-2: Richer NL descriptions — +3.7pp R@1 (v1.0.6)
- SQ-4: Call-graph-enriched embeddings — 63% enriched (v1.0.7)
- SQ-5: Module-level context — filename stems (v1.0.9)
- SQ-6: LLM summaries — Haiku batch, cached by content_hash (v1.0.14)
- SQ-7: LoRA fine-tuning — v5 (166k/1ep) shipped, CSN 0.683
- SQ-8: Doc comment generation — `--improve-docs` flag
- SQ-9: Notes + embeddings simplification — schema v15→v16
- SQ-10: Cross-encoder reranker — REGRESSION (-81.8pp). Random negs too easy. Infrastructure kept.
- SQ-11: Type-aware embeddings — +3.6pp R@1 (PR #630)
- SQ-12: HyDE query predictions — `--hyde-queries` flag, mixed results

## Training Improvements (done)
1. Hard negative mining — 1.89M pairs, GPU FAISS, CoRNStack recipe
2. 9-language training data — CSN 6 + Stack Rust/TS/C++
3. v7 (GIST+Matryoshka) — CoIR 49.19, CSN 0.707, shipped v1.3.1
4. v7b balanced (414k, 46k/lang) — no improvement over v7
5. Resume-from-checkpoint in train_lora.py
6. ONNX opset-11 export with weight injection
7. v8 KeyDAC — CSN 0.652, extreme trade-off (Python 0.996, PHP collapsed)
8. Contrastive summaries (SQ-10b) — +1.9pp over non-contrastive ($0.38/index)

## Multi-Grammar Parsing (done)
Injection framework v0.27.0. Done: HTML→JS/CSS, PHP→HTML→JS/CSS, Svelte→JS/CSS, LaTeX→code listings, Nix→Bash, HCL→Bash, Make→Bash, Razor→JS/CSS, Vue→JS/CSS, Markdown→fenced code blocks.

## Future Languages — Completed
Elixir, Erlang, Haskell, OCaml, Julia, Gleam, CSS, Perl, HTML, JSON, XML, INI, Nix, Make, LaTeX, Solidity, CUDA, GLSL, Svelte, Razor, VB.NET, Vue, Markdown, ASP.NET Web Forms.

## Potential Quality Improvements (evaluated)
- Weighted multi-signal fusion — done, no impact on hard eval
- CodeSage-large-v2 — rejected (can't parse NL queries)
- Consistency filtering — rejected (CSN already clean)
- Mixed LoRA (v6) — rejected (dilutes signal)
- Full CoIR pipeline — enrichment hurts benchmarks (-4.5pp)
