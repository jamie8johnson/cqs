# Roadmap

## Current: v1.0.13

First stable release. All agent experience features shipped. CLI-only (MCP removed in v0.10.0). 51 languages. Two full audits complete (v0.12.3 + v0.19.2). Recursive multi-grammar injection framework.

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

All 16 variants shipped and used across languages. Only one potential new variant remains: `Extension` for Swift.

| Variant | Shipped in | Used by |
|---------|-----------|---------|
| `Module` | v0.16.0 | F#, Ruby, TS (namespace) |
| `Macro` | v0.17.0 | Rust, C (`#define(...)`) |
| `TypeAlias` | v0.17.0 | Scala, Rust, TypeScript, Go, C, F#, SQL |
| `Object` | v0.17.0 | Scala |

Infrastructure for adding variants is now cheap: per-language LanguageDef fields, data-driven container extraction, dynamic callable SQL. New variant = enum arm + Display/FromStr + is_callable decision + nl.rs + capture_types.

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
- [ ] SQ-8: LLM doc comment generation — augment thin doc comments and generate docs for undocumented functions.
  - **Augment existing:** Functions with thin docs (one-liner, param-list-only) get doc + body sent to Claude for an improved version.
  - **Generate new:** Functions with `doc: None` get full doc comments generated from the function body.
  - **Infrastructure:** Reuses `llm_summary_pass` paging, Batches API, content_hash caching. Different system prompt, higher `MAX_TOKENS` (500-1000 vs 100 for summaries). Separate model/source tag in `llm_summaries` to distinguish summary vs generated-doc.
  - **CLI:** `cqs index --improve-docs` (separate from `--llm-summaries`). Default behavior: write generated comments back to source files. `--dry-run` to preview. `cqs read` shows generated docs as virtual comments when `--improve-docs` hasn't written back yet.
  - **Prioritization:** `is_callable()` functions first, sorted by: no doc > thin doc > adequate doc. Use content length and doc/content ratio as heuristics.
  - **Cost:** Higher than summaries (~5-10x output tokens). Batch API 50% discount helps. Cache by content_hash — pay once per function body.
- [ ] SQ-9: Simplify notes + embeddings architecture.
  - **Notes as annotations, not search targets:** Remove notes from unified search results (burns 40% of slots on metadata). Drop `search_notes()`, `note_embeddings()`, brute-force cosine scan, unified slot allocation, `note_weight`/`note_only` flags. Keep: mention-based boost on code scoring, `cqs read` injection, `cqs notes list/add/remove`, review/health inputs.
  - **Drop sentiment dimension (769→768-dim):** With note embeddings gone, every vector has sentiment=0.0 — dead weight. Revert to pure 768-dim E5-base-v2 output. Fixes AC-8 (CAGRA distance conversion divergence — all vectors become truly unit-norm). Simplifies embedding pipeline (no sentiment append/strip).
  - **Schema migration:** Dimension change requires reindex. Update `EMBEDDING_DIM` constant, HNSW build, CAGRA build, brute-force cosine, all embedding serialization.
  - **Docs update:** README, PRIVACY.md, SECURITY.md, CLAUDE.md, CONTRIBUTING.md, lib.rs — all reference 769-dim, update to 768.
- [ ] SQ-7: Fine-tune E5-base-v2 with LoRA on code search pairs.
  - **Hardware:** A6000 (48GB VRAM), can fine-tune in hours
  - **LoRA:** Low-Rank Adaptation — freezes base weights, trains ~0.5-2M adapter params (vs 110M full). Adapter is ~10-50MB.
  - **Training data:** hard eval (55 queries) + holdout (143 queries) + synthetic pairs from cqs/aveva codebases
  - **Deployment:** Upload merged ONNX to HuggingFace (`jamie8johnson/e5-base-v2-code-search`), cqs downloads it instead of base E5. Or upload LoRA adapter separately for A/B testing.
  - **Why:** E5-base-v2 is a general NL model — prose (README/CHANGELOG) naturally scores higher than generated code NL descriptions. LoRA teaches the model that "parse config file" should match `fn parse_config()` better than a README paragraph about configuration. This is the real fix for code-vs-doc ranking.

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
