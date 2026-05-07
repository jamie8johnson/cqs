# Extensibility Audit — post-v1.38.0

Audit run 2026-05-06 against main (post v1.38.0 / pre-next). Audit mode ON.
Skips findings already closed by #1474 / #1482 / #1483 / #1500 / #1495 /
#1508 / #1509 / #1510 / #1511.

#### EX-V1.38-1: `doc_format_for` dispatches by string-tag through 12-arm match — adding a doc style is a 2-place edit
- **Difficulty:** easy
- **Location:** `src/doc_writer/formats.rs:50-130` + `src/language/mod.rs:392` (`pub doc_format: &'static str`)
- **Description:** `LanguageDef.doc_format` carries a string tag (`"javadoc"`, `"triple_slash"`, `"python_docstring"`, …) that `doc_format_from_tag()` matches against to produce a `DocFormat` literal. Adding a new comment style (e.g. Zig's `///`-with-`!` doc, Nim's `##`, Tcl's `#`-aligned) requires both: a row in the giant `match tag` *and* the right tag string in `LANG_FOO`. Mistype the tag and the language silently falls into the `_ =>` default (Java-ish `// `). 13 languages opted into `"javadoc"`, so the bulk-rename cost is also non-trivial.
- **Suggested fix:** Replace `pub doc_format: &'static str` with `pub doc_format: DocFormat` (or `&'static DocFormat`). Move the 13 `DocFormat { … }` literals next to each `LANG_FOO` definition (or behind named statics like `DOC_FORMAT_JAVADOC` for de-duplication). Delete `doc_format_from_tag` and the tag string entirely. Compiler enforces full population, no silent fallback.

#### EX-V1.38-2: C# / Java / Kotlin / Python `post_process_chunk` re-hardcodes `[Test]` / `@GetMapping` lists already on `LanguageDef::test_markers`
- **Difficulty:** medium
- **Location:** `src/language/languages.rs:581-595` (csharp); analogous blocks in java / kotlin / python post-processors
- **Description:** `LanguageDef.test_markers` exists and is used for indexing analytics (`all_test_markers`), but the `post_process_chunk_*` functions that promote `Function → Test` / `Function → Endpoint` carry their **own** parallel hardcoded `header.contains("[Test]")` / `header.contains("@Test")` / `header.contains("[HttpGet]")` checklists. So adding xUnit's `[Theory(...)]` or Spring's `@RestController`-derived endpoints means editing two unrelated spots — and the table for `Endpoint` markers (`[HttpGet]`/`@GetMapping`/`@RequestMapping`/`@app.route`/`@router.get`) has no `LanguageDef` field at all.
- **Suggested fix:** (1) Make `post_process_chunk_*` consult `lang.def().test_markers` instead of inline literals — the existing field is the source of truth. (2) Add `pub endpoint_markers: &'static [&'static str]` on `LanguageDef`; populate per-language. (3) Replace the inline `header.contains("[HttpGet]") || …` chains with `endpoint_markers.iter().any(|m| header.contains(m))`.

#### EX-V1.38-3: Reranker tunables (`batch`, `max_length`, `pool_max`, `over_retrieval`) are env-only — no `[reranker]` config knobs
- **Difficulty:** easy
- **Location:** `src/reranker.rs:92` (CQS_RERANKER_BATCH), `src/reranker.rs:264` (CQS_RERANKER_MAX_LENGTH), `src/cli/limits.rs:62-73` (CQS_RERANK_OVER_RETRIEVAL / CQS_RERANK_POOL_MAX)
- **Description:** `[reranker]` table in `.cqs.toml` (`AuxModelSection`) only has `preset` / `model_path` / `tokenizer_path`. Operators tuning rerank perf for a slow model can only set 4 env vars. New `[index.policy]` (#1511) is the right precedent — same gap exists for reranker. Same for the SPLADE-side knobs (CQS_SPLADE_ALPHA, used heavily in eval sweeps per memory).
- **Suggested fix:** Promote to `AuxModelSection`: optional `batch`, `max_length`, `pool_max`, `over_retrieval`. Keep env vars as override-on-top precedence (matches existing CLI → env → TOML chain). Add `[reranker.policy]` sub-table if you want symmetry with `[index.policy]`.

#### EX-V1.38-4: Classifier vocab overlay covers only 2 of 6 vocabularies
- **Difficulty:** medium
- **Location:** `src/search/router.rs:303-363` (BEHAVIORAL_VERBS, CONCEPTUAL_NOUNS, STRUCTURAL_KEYWORDS — all `const`); `src/search/router.rs:519` (`install_classifier_vocab_overlay` only handles negation + multistep)
- **Description:** #1483 added a `classifier.toml` overlay for `negation_tokens` and `multistep_patterns`, but the other four vocabularies the classifier consults — `BEHAVIORAL_VERBS` (28 entries), `CONCEPTUAL_NOUNS` (14 entries), `STRUCTURAL_KEYWORDS`, and the implicit NL_INDICATORS — remain compile-time constants. A user wanting to teach the router that "orchestrates" is behavioral, or that "topology" is conceptual, has no recourse short of a fork. The overlay design is already there; finishing it is mechanical.
- **Suggested fix:** Mirror the `NEGATION_TOKENS` `LazyLock<RwLock<HashSet>>` / `MULTISTEP_PATTERNS_AC` `LazyLock<RwLock<Arc<AhoCorasick>>>` pattern for the remaining four sets. Extend `load_classifier_vocab_overlay` schema (`behavioral_verbs`, `conceptual_nouns`, `structural_keywords`, `nl_indicators`). Same TOML, same install function.

#### EX-V1.38-5: `cqs task` waterfall budget weights are `const f64` — no operator knob
- **Difficulty:** easy
- **Location:** `src/cli/commands/train/task.rs:268-275`
- **Description:** WATERFALL_SCOUT/CODE/IMPACT/PLACEMENT (0.15/0.50/0.15/0.10) decide how `--max-tokens` is divided across sections of `cqs task` output. Operators and agents have very different preferences (an agent doing impact analysis wants more `impact` budget). Right now the only path is recompile.
- **Suggested fix:** Promote to a new `[task]` section in config (or extend `ScoringOverrides` knob registry — same pattern: one row, no schema churn). At minimum, add env vars `CQS_TASK_WATERFALL_{SCOUT,CODE,IMPACT,PLACEMENT}` matching the existing `parse_env_*` pattern in `src/limits.rs`.

#### EX-V1.38-6: `extract_calls_from_chunk` has a hardcoded `Language::Markdown` branch — should route through `custom_call_parser`
- **Difficulty:** easy
- **Location:** `src/parser/calls.rs:124-137`
- **Description:** The exact code smell `LanguageDef::custom_call_parser` was meant to eliminate: `if chunk.language == Language::Markdown { return crate::parser::markdown::extract_calls_from_markdown_chunk(chunk); }`. Adding another grammar-less language with custom call extraction (e.g. SQL stored-proc cross-refs, L5X tag references, or a future natural-language doc format) requires editing this site instead of just populating `def.custom_call_parser`. Note the related dispatch in `src/parser/mod.rs:516-548` — grammar-less languages without `custom_all_parser` *silently fall through to the markdown path*, which is also surprising.
- **Suggested fix:** Add a `chunk_call_parser: Option<fn(&Chunk) -> Vec<CallSite>>` field on `LanguageDef` and wire `extract_calls_from_chunk` to consult it before the tree-sitter path. Markdown registers `extract_calls_from_markdown_chunk`. Then the `Language::Markdown` literal goes away.

#### EX-V1.38-7: `Language::Python` docstring extraction lives inside `extract_doc_comment`, not on `LanguageDef`
- **Difficulty:** medium
- **Location:** `src/parser/chunk.rs:251-263`
- **Description:** Python is the only language that places doc comments **inside** the function body (`def f(): """docstring"""`) rather than as preceding sibling comments. The fallback path in `extract_doc_comment` carries an `if language == Language::Python { … }` block that walks `body → expression_statement → string`. Any other "docstring-style" language (a hypothetical Lua via LDoc-as-first-string, or Python-syntax DSLs in `.cqs` query files) needs to be hand-stitched here. `InsertionPosition::InsideBody` already exists in `DocFormat` — symmetry would say there's an `extract_inside_body_doc` hook.
- **Suggested fix:** Add `pub inside_body_doc_extractor: Option<fn(node, source) -> Option<String>>` (or a `DocPlacement` enum: `BeforeAsSibling | InsideBodyAsString { kind: &'static str }`). Move the Python branch into the python `LanguageDef` populator. Removes the `Language::Python ==` literal from chunk.rs.

#### EX-V1.38-8: `doc_writer/formats.rs` has Go-specific "prepend FuncName to first line" rule hardcoded with `if language == Language::Go`
- **Difficulty:** easy
- **Location:** `src/doc_writer/formats.rs:158-170`
- **Description:** Go's `// FuncName does X` convention is encoded as a literal `if language == Language::Go { /* prepend func name */ }` in the formatter. Other languages that want subject-first conventions (e.g. Erlang `%% function/arity:`, Elixir doc that wants `@doc "function/arity ..."`, or a custom house style) can't opt in without code edits.
- **Suggested fix:** Add a `doc_first_line_template: Option<&'static str>` (e.g. `"{name} "` for Go, `None` for everyone else) to `DocFormat` (or as a sibling field on `LanguageDef`). Formatter does template substitution if `Some`. Removes the `Language::Go` literal.

#### EX-V1.38-9: `extract_method_receiver_type` has `if language != Language::Go { return None; }` — Go-specific receiver logic should be on `LanguageDef`
- **Difficulty:** medium
- **Location:** `src/parser/chunk.rs:611-624`
- **Description:** Go is the only language whose method-receiver type isn't on the parent container — it's on the method node itself (`func (r *Server) Handle()`). The current code guards with `language != Language::Go { return None; }`. Rust trait impls, Swift extensions, Objective-C categories, and C++ out-of-class member definitions all have similar "look-elsewhere for the container" needs that today either don't work or fall back to surrounding-container heuristics.
- **Suggested fix:** Add `pub receiver_type_extractor: Option<fn(node, source) -> Option<String>>` on `LanguageDef`. Go populates it with the existing function. The `language != Language::Go` literal goes away and other languages can opt in.

#### EX-V1.38-10: `test_type_queries_compile` hand-lists the 11 languages with type queries instead of iterating `Language::ALL`
- **Difficulty:** easy
- **Location:** `src/parser/calls.rs:1096-1121`
- **Description:** Test harness for "all type queries compile" enumerates `Language::Rust, TypeScript, Python, Go, Java, C, CSharp, Scala, Cpp, Php, Zig` — adding a 12th language with a `type_query` won't be tested unless someone remembers this list. The exhaustive-iteration version is `Language::ALL.iter().filter(|l| l.def().type_query.is_some())`.
- **Suggested fix:** Replace the array with `for lang in Language::ALL.iter().copied().filter(|l| l.def().type_query.is_some())`. Bonus: add a sibling test that asserts `try_def().is_some()` for every `type_query.is_some()` language so disabled-feature combinations don't silently skip the query.
