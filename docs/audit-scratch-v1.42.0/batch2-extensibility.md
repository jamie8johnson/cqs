## Extensibility

#### Hardcoded synonym table — no extension hook for domain vocabulary
- **Difficulty:** easy
- **Location:** src/search/synonyms.rs:13-46
- **Description:** `SYNONYMS` is a `LazyLock<HashMap>` baked into the binary with 30 generic developer abbreviations (`auth`, `cfg`, `req`, `db`, etc.). Adding domain vocabulary — industrial automation (`plc`, `scada`, `opc`, `hmi`), manufacturing (`mes`, `erp`, `andon`), or cqs-internal terms (`hnsw`, `splade`, `cagra`, `rrf`, `slot`) — requires editing this file and recompiling. There is no config-side or per-project extension hook. cqs is positioned for manufacturing/industrial use cases per project memory; the synonym dictionary blocks that pivot at the source-edit boundary.
- **Suggested fix:** Load synonyms from a TOML table merged on top of the static defaults: `~/.config/cqs/synonyms.toml` (user-global) and `.cqs/synonyms.toml` (per-project). Each row: `key = ["expansion1", "expansion2"]`. Built-ins remain in source as the floor; project overrides win on conflict. Validation reuses the existing FTS-safe alpha-token check.

#### `apply_parent_boost` hardcodes Class/Struct/Interface as the only container kinds
- **Difficulty:** easy
- **Location:** src/search/scoring/candidate.rs:81-84
- **Description:** Parent-container boost only fires for `ChunkType::Class | Struct | Interface`. cqs already extracts `Trait`, `Enum`, `Module`, `Object`, `Protocol` (and parser/chunk.rs assigns `parent_type_name` for methods on Rust traits, Kotlin objects, Swift protocols, etc.). A query that semantically matches three trait method impls won't boost the trait itself — the heuristic silently degrades to one-result behavior on those languages. Adding a new container variant requires editing this match plus chasing every other `is_container` site.
- **Suggested fix:** Add `ChunkType::is_container() -> bool` on the enum (generated via `define_chunk_types!` macro to keep it data-table-driven), then call `r.chunk.chunk_type.is_container()`. Declare the container flag in the macro row alongside `hints = [...]` and `human = "..."`.

#### `is_test_chunk` heuristic is hardcoded and diverges from SQL-side `TEST_NAME_PATTERNS`
- **Difficulty:** medium
- **Location:** src/lib.rs:502-541, src/store/calls/mod.rs:126
- **Description:** Two parallel test-detection patches: `is_test_chunk` in lib.rs uses tightened name rules (`Test_`, `test_`, `_test`, `_spec`, `.test`) and language-registry path patterns; `TEST_NAME_PATTERNS = &["test_%", "Test%"]` in calls/mod.rs uses looser SQL LIKE patterns that still match `TestSuite` / `TestRegistry` (the production-type case the AC-4 audit explicitly fixed in lib.rs). Adding a new convention — JUnit5 `@DisplayName`-annotated, BDD `_when_should_*`, Go `it_*`, Rust `#[test_case(...)]` — requires touching both sites with different syntaxes (Rust regex vs SQL LIKE). Plus the markers are language-namespaced through `LanguageDef::test_markers` for content but the *name* heuristics are global.
- **Suggested fix:** Move test-name patterns into `LanguageDef::test_name_patterns` (mirroring `test_markers` and `test_path_patterns`), then have both `is_test_chunk` and `TEST_NAME_PATTERNS` consume the registry. Single source of truth, language-scoped, and adding a Kotlin/Swift convention is one line in the language module.

#### `OutputFormat` is a closed enum with hand-coded if/else-if chains at every render site
- **Difficulty:** medium
- **Location:** src/cli/definitions.rs:13-17, src/cli/commands/graph/trace.rs:126-281, src/cli/commands/graph/impact.rs:48-92
- **Description:** `OutputFormat { Text, Json, Mermaid }` is dispatched at 12+ sites via `if matches!(format, OutputFormat::Json) { ... } else if matches!(format, OutputFormat::Mermaid) { ... } else { ... }`. Adding a fourth format (CSV for spreadsheet pipelines, GraphViz/dot for graph commands, Markdown table for PR comments, NDJSON for streaming) requires hunting every render site and adding another `else if`. Worse, the chains aren't exhaustive — the trailing `else` silently falls into Text rendering for unhandled variants, so a new variant will produce text output instead of a compile error.
- **Suggested fix:** Define a `Renderer` trait per command type (`TraceRenderer`, `ImpactRenderer`) with `render_trace(&Trace)` / `render_impact(&Impact)` methods, and a `&[&dyn Renderer]` registry indexed by `OutputFormat`. Replace the if/else-if chain with a single `pick_renderer(format).render(&data)`. Match on the enum at one site (the registry lookup), and let the compiler enforce exhaustiveness via `#[deny(non_exhaustive_omitted_patterns)]`.

#### Hardcoded query classifier function — adding a category requires surgery
- **Difficulty:** medium
- **Location:** src/search/router.rs:632-773 (`classify_query_inner`)
- **Description:** Query classification is an 8-arm hand-coded if/return chain with priority semantics encoded in source order (Negation > Identifier > CrossLanguage > TypeFiltered > Structural > Behavioral > Conceptual > MultiStep). Each arm calls a hardcoded predicate (`is_identifier_query`, `is_cross_language_query`, etc.) and emits a hardcoded `(category, confidence, strategy, type_hints)` tuple. Adding a category — `RegexQuery`, `ApiCall`, `ErrorMessage`, `StackTrace` — requires inserting a new branch at the right priority position, declaring a new predicate, and remembering to add the variant to the `QueryCategory` macro AND to the centroid classifier in `reclassify_with_centroid`. The QueryCategory enum is macro-driven but the *classifier* is not.
- **Suggested fix:** Define `trait QueryClassifier { fn priority(&self) -> i32; fn classify(&self, query: &str, words: &[&str]) -> Option<Classification>; }` and hold a `&[&dyn QueryClassifier]` static slice sorted by descending priority. `classify_query_inner` becomes "first non-None classifier wins, fall back to Unknown." Each existing arm becomes one impl; new categories are one new struct + one slice row.

#### Three near-identical prompt builders — adding a fourth means another method
- **Difficulty:** medium
- **Location:** src/llm/prompts.rs:182-312
- **Description:** `build_contrastive_prompt`, `build_doc_prompt`, `build_hyde_prompt` share the same shape: truncate content, sanitize, fresh sentinel nonce, `format!` with sentinel-bracketed body. The `BatchKind` enum (P3-47 / EX-V1.33-1) abstracted the dispatch but the prompt builders themselves remain three independent methods on `LlmClient`. The doc-comment for `BatchKind` already lists three pending purposes (`Classification`, `ContrastiveRepair`, `CodeReview`) — each will require: (1) a new method on `LlmClient`, (2) a new variant on `BatchKind`, (3) a new arm in the dispatcher mapping kind → builder. Three coordinated edits per new prompt purpose.
- **Suggested fix:** Define `trait PromptBuilder { fn build(&self, item: &BatchSubmitItem) -> String; }` with one impl per kind. Replace `BatchKind` dispatch with a `&[&dyn PromptBuilder]` registry indexed by kind. Adding a fourth purpose: one new struct + one new enum variant + one slice row. The shared sentinel/truncate/sanitize prelude moves into a `BasePrompt` helper that all impls call.

#### `PoolingStrategy` dispatch site loses exhaustiveness via `unreachable!`
- **Difficulty:** easy
- **Location:** src/embedder/mod.rs:1390-1399, src/embedder/models.rs:111-137
- **Description:** The 4-variant `PoolingStrategy` is dispatched in the encode loop with `Identity` mapped to `unreachable!()` because it's intercepted earlier as a 2D shortcut. Adding a fifth pooling strategy (e.g., `WeightedMean`, `MaxPool`, `AttentionPool` for LLM2Vec-style heads) means: (1) add variant, (2) implement pooler function, (3) edit dispatch site, (4) decide whether the 2D shortcut applies and edit the 2D path. The `unreachable!` arm is brittle — any future model whose ONNX returns 3D AND uses Identity pooling silently panics in production.
- **Suggested fix:** Make pooling a trait: `trait Pooler { fn pool_3d(&self, hidden: &Array3<f32>, mask: &Array2<i64>, dim: usize) -> Vec<Vec<f32>>; fn handles_2d_directly(&self) -> bool { false } }`. Each variant becomes one impl. The 2D shortcut becomes `if pooler.handles_2d_directly() && hidden.is_2d()`. Removes the unreachable arm and makes adding a new pooler one struct + one enum row.

#### Hardcoded NEGATION / MULTISTEP / cross-language token lists in router
- **Difficulty:** easy
- **Location:** src/search/router.rs (NEGATION_TOKENS, MULTISTEP_PATTERNS_AC, language-name lists in is_cross_language_query)
- **Description:** The classifier predicates rely on a fixed set of English vocabulary: `NEGATION_TOKENS` (without/no/not/...), MULTISTEP conjunctions (then/and then/...), language-name lists (rust, python, ...). Non-English queries, domain-specific negation ("ignoring X"), or new languages added via `lang-*` features don't propagate to the classifier — the language registry is the source of truth for "what languages exist" but the classifier maintains its own parallel list. The `1.95-msrv` edition note in CLAUDE.md mentions "let-chains in if/while are out of scope" — these classifier predicates would benefit from let-chain refactoring once edition bumps, but the underlying vocabulary lock-in is the deeper issue.
- **Suggested fix:** (1) Have `is_cross_language_query` consume `Language::valid_names()` instead of a hardcoded slice — drops one drift point. (2) Move NEGATION_TOKENS / MULTISTEP_PATTERNS to `~/.config/cqs/classifier.toml` with built-in defaults, mirroring the synonym fix above.

#### LLM model-name validation hand-coded per provider
- **Difficulty:** medium
- **Location:** src/llm/mod.rs:326-326 + each `ProviderRegistry::build` impl
- **Description:** While the `ProviderRegistry` trait is properly pluggable (P4-EX work), each provider's model-name validation is implicit / informal — `LocalProvider::new` parses `LlmConfig.model` as a string with no constraint, so `cqs feedback --provider anthropic --model gpt-4o` silently submits to Anthropic with an OpenAI model name and fails at API time with a vague error. There's no `provider.is_valid_model(name) -> Result<(), Error>` step before submission.
- **Suggested fix:** Add `fn validate_model(&self, name: &str) -> Result<(), LlmError>` to `BatchProvider` (or `ProviderRegistry`). Anthropic checks against a known prefix list (`claude-`, `claude-3`, etc.); Local accepts anything because OpenAI-compat servers expose arbitrary model names; a future OpenAI provider can validate `gpt-` / `o1-` / `o3-`. Call it from `submit_batch` before the API roundtrip. Adds one method per provider but catches the fast-fail case.

#### CAGRA threshold and backend selection are env-var-driven, not policy-extensible
- **Difficulty:** medium
- **Location:** src/index.rs (BackendContext) + cagra/hnsw backends + `CQS_CAGRA_THRESHOLD` env var
- **Description:** The `IndexBackend` registry is now table-driven (P4-EX work) but the *selection policy* — "use CAGRA when chunk_count >= 5000 and GPU available" — is hand-coded inside each backend's `try_open`. There's no shared `SelectionPolicy` trait, so adding a third backend (USearch, Metal, ROCm, SIMD brute-force) requires re-deriving "when should I be picked" from ad-hoc env vars and chunk-count thresholds. The `i32 priority()` is iteration order, not a real eligibility rule. This blocks slot-aware policies like "prefer USearch on slot=foo because it's tuned for that corpus shape" without a config-side knob.
- **Suggested fix:** Promote the eligibility logic to a `SelectionPolicy` shape on `BackendContext` (e.g., `cqs.toml [index.policy]` with `prefer = "cagra"`, `cagra_min_chunks = 5000`, `disabled_backends = ["usearch"]`), then have each backend's `try_open` read it from `ctx.policy`. New backends declare their default policy entry; operators override per-slot in TOML.

DONE
