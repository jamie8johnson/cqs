# Roadmap

## Current: v0.28.1

All agent experience features shipped. CLI-only (MCP removed in v0.10.0). 50 languages. Two full audits complete (v0.12.3 + v0.19.2). Recursive multi-grammar injection framework.

### Next — Commands

- [x] `cqs blame` — semantic git blame. Given a function, show who last changed it, when, and the commit message. Combines call graph with git log.
- [x] `cqs chat` — interactive REPL for chained queries. Readline, history, tab completion. Wraps batch mode.

### Next — Performance

- [x] PF-5: Lightweight HNSW candidate fetch (#510) — fetch only `(id, embedding)` for scoring, load full content only for top-k survivors.

### Next — Expansion

- [ ] Pre-built release binaries (GitHub Actions) — adoption friction

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

**Next — New grammars required:**
- [ ] Vue (.vue) → JS/TS, CSS, HTML — needs `tree-sitter-vue` grammar. `<script>`, `<style>`, `<template>` identical to HTML injection pattern.

**Next — Medium value (narrower scope):**
- [ ] Markdown → fenced code blocks — custom parser, not tree-sitter. Needs different approach (parse ` ```lang ` content with target grammar).
- [ ] YAML → Bash — GitHub Actions `run:` blocks. Detection fragile (not all strings are scripts).

**Lower priority (niche or fragile):**
- [ ] Astro (.astro) → JS/TS + HTML — needs grammar
- [ ] ERB (.erb) → Ruby in HTML — needs grammar
- [ ] EEx/HEEx (.eex, .heex) → Elixir in HTML — needs grammar
- [ ] SQL in string literals (Rust, Python, Go, Java) — fragile detection
- [ ] GraphQL in tagged templates (JS/TS) — fragile detection
- [ ] Shell in Makefile recipes — both grammars compatible
- [ ] CSS-in-JS (styled-components, emotion) — template literal detection

### Parked

- **MCP server** — re-add as slim read-only wrapper when CLI features are rock solid. Architecture proven clean (removed in v0.10.0 with zero core changes).
- **Pre-built reference packages** (#255) — `cqs ref install tokio`
- **Index encryption** — SQLCipher behind cargo feature flag
- **Query-intent routing** — auto-boost ref weight when query mentions product names
- **Pattern mining** (`cqs patterns`) — recurring code conventions. Large effort, defer.
- **Post-index name matching** — fuzzy cross-doc references

### Open Issues

- #389: CAGRA GPU memory — needs disk persistence layer
- #255: Pre-built reference packages
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## 1.0 Release Criteria

- [ ] Schema stable for 1+ week of daily use (currently v11)
- [ ] Used on 2+ different codebases without issues
- [ ] No known correctness bugs

1.0 means: API stable, semver enforced, breaking changes = major bump.

---

*Completed phase history archived in `docs/roadmap-archive.md`.*
