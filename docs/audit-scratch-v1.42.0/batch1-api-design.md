## API Design

#### `cqs project search` and top-level `cqs <q>` are two semantic search entry points with diverging surfaces
- **Difficulty:** medium
- **Location:** `cqs project search` (src/cli/commands/infra/project.rs:88) vs top-level `Cli` (src/cli/definitions.rs:153)
- **Description:** The top-level `cqs <q>` command exposes ~20 search knobs (`--rrf`, `--rerank`, `--reranker`, `--name-boost`, `--include-type`, `--exclude-type`, `--path`, `--pattern`, `--name-only`, `--no-content`, `-C/--context`, `--expand-parent`, `--ref`, `--include-refs`, `--tokens`, `--no-stale-check`, `--no-demote`, `--splade`, `--splade-alpha`, `--include-docs`, …). `cqs project search` exposes only `-n`, `-t`, `--json`. So the same conceptual operation (semantic search) has wildly different ergonomics depending on which scope you target. Agents who learn the top-level flag set get nothing transferable when they want cross-project search; the cross-project path silently can't filter by language, type, path, or rerank.
- **Suggested fix:** Either (a) flatten the same `SearchArgs` into both commands so the flag surface is identical, or (b) document explicitly that `project search` is the minimal-surface entry-point and add the top 4–5 most-used filters (`-l`, `--include-type`, `--path`, `--rerank`).

#### `cqs project` and `cqs ref` are two registries of external indexes with overlapping responsibilities
- **Difficulty:** medium
- **Location:** `cqs project add/list/remove/search` vs `cqs ref add/list/remove/update`
- **Description:** Both subcommand trees register external code indexes. `ref` adds a "reference index" (with `--weight`, `update` re-indexes from source); `project` adds an existing `.cqs/index.db` to a "cross-project search registry". Top-level search has both `--ref <name>` and `--cross-project` flags that point at these two different registries. Naming overlap is severe: a "project" is a "reference" and vice-versa from the agent's POV. Compare with the documented P3-29 fix (`project register` → `project add`) which only addressed the verb mismatch — the deeper concept duplication remains. Top-level `cqs --help` shows both `ref` and `project` adjacent with no hint of when to use which.
- **Suggested fix:** Pick one noun. Merge the registries: a single `cqs ref add <name> <path-or-source> [--weight N] [--no-index]` covers "external indexed codebase" whether built locally or already-indexed. Drop `project` (or alias). Long-term this also collapses the `--ref`/`--cross-project` flag confusion in search.

#### `cqs index` and `cqs ref update` both build/refresh an index but use opposite verbs and flag sets
- **Difficulty:** easy
- **Location:** `cqs index` (definitions.rs:388) vs `cqs ref update` (cli/commands/infra/reference.rs)
- **Description:** Re-indexing the project is `cqs index --force` (with `--no-ignore`, `--accept-shared-notes`, `--llm-summaries`, `--improve-docs`, `--apply`, `--max-docs`, `--hyde-queries`, `--dry-run`). Re-indexing a registered ref is `cqs ref update <name>` (no flags). Same operation, two verbs (`index` vs `update`), and the ref path skips every quality-affecting flag the project path supports — so a refreshed reference can't get LLM summaries or HyDE queries even though the project index can.
- **Suggested fix:** Either alias `cqs ref update` → `cqs ref reindex` and pass the same `IndexArgs`, or document clearly that ref refresh is intentionally minimal (it isn't — agents will hit this).

#### `--depth` short flag `-d` exists on `impact`/`onboard`/`test-map` but not on `gather` or `trace`
- **Difficulty:** easy
- **Location:** src/cli/args.rs:284 (`gather`), src/cli/commands/graph/trace.rs:32 (`trace`), vs args.rs:310/473/500
- **Description:** API-V1.29-10 added `-d` for parity, but only on three of the five depth-bearing commands. `gather` declares `#[arg(long, default_value_t = DEFAULT_DEPTH_BLAST, visible_alias = "expand")]` with no short. `trace` uses `--max-depth` with `--depth` only as an alias and no short. `cqs gather "x" -d 2` errors with `unexpected argument '-d' found`, while `cqs impact x -d 2` works. The cross-command muscle memory the audit comment promised isn't actually delivered.
- **Suggested fix:** Add `short = 'd'` to `GatherArgs::depth` and to the `trace` `--max-depth` flag (with the existing `--depth` alias).

#### `--rerank` (bool) and `--reranker` (enum) are both live on `cqs <q>` — two flags for one knob
- **Difficulty:** medium (API design, easy code)
- **Location:** src/cli/definitions.rs:215 (`pub rerank: bool`) and 224 (`pub reranker: Option<RerankerMode>`)
- **Description:** Tracked as P2-14 / #1372 but still both shipped. `--rerank` is "boolean shorthand for `--reranker onnx`". The collision is then resolved by `Cli::rerank_mode()` picking enum-over-bool. This is exactly the "two ways to express the same thing" anti-pattern the audit elsewhere flags as cause of agent confusion. Public docstring even names the loser ("Takes precedence over the legacy `--rerank` bool when both are passed"). Per CLAUDE.md "No External Users" this can be a hard deletion.
- **Suggested fix:** Drop `--rerank` (the bool). Documentation says it's "muscle memory" but a hard rename without alias is the project policy (see `--commits` rename in `blame`). #1372 should ship as a deletion, not as the dual-flag state we're in now.

#### `cqs gather --direction` defaults to `both`; `cqs onboard` only walks callees with no direction knob
- **Difficulty:** easy
- **Location:** `cqs gather` (args.rs:286), `cqs onboard` (args.rs:494)
- **Description:** `gather` exposes `--direction both|callers|callees`, but the conceptually parallel `onboard` (also call-chain BFS expansion) silently hardcodes "callees" with no flag. An agent learning "depth + direction" on `gather` cannot transfer to `onboard`. Similarly, `cqs callers <name>` and `cqs callees <name>` are sibling commands but `cqs onboard` doesn't allow specifying the direction at all.
- **Suggested fix:** Add `--direction` to `OnboardArgs` (default `callees` for back-compat) so depth+direction is a uniform pair across `gather`, `onboard`, and `test-map`.

#### `cqs eval --reranker` accepts `none|onnx|llm` but `llm` errors at runtime — flag advertises a non-existent capability
- **Difficulty:** easy
- **Location:** src/cli/commands/eval/mod.rs (RerankerMode) — help text describes `llm` as "reserved for #1220 and currently bails with a 'not yet implemented' error"
- **Description:** Surfacing a placeholder enum variant in `--help` and the value parser is the same scaffold-as-API anti-pattern that got `LlmReranker` demoted in P1-33. The variant exists in the public CLI surface specifically so "production wiring can land without a breaking CLI change", but per "No External Users" / no-deprecation-cycles policy, that argument doesn't apply here. Agents reading `--help` see `llm` as a real choice.
- **Suggested fix:** Drop `Llm` from `RerankerMode` until #1220 actually wires it. Add it back with the implementation; flipping the variant is one-line at the wire-up site.

#### `cqs slot create --model <preset>` exists but `cqs index --model <preset>` doesn't — model preset is split across two commands inconsistently
- **Difficulty:** medium
- **Location:** `cqs slot create --model` (definitions.rs Slot subcommand), `cqs index` (no `--model`), top-level `Cli::model` (definitions.rs:292)
- **Description:** Model selection is on (a) top-level `cqs <q> --model <name>` (search-time), (b) `cqs slot create --model <name>` (slot bootstrap), but NOT on `cqs index --model <name>` (re-index time). To switch models you must `cqs slot create --model X && cqs slot promote X && cqs index`. Model swap inside the active slot also requires going through `cqs model swap`, a third entry point. Three commands manage one concept and the user has to know the right verb for the right context.
- **Suggested fix:** Add `cqs index --model <preset>` that reindexes into the active slot with the new model (or refuses if the recorded model differs, with a hint to use `model swap`). Consolidate around `model swap` as the canonical "change my model" entry point and delete `slot create --model`'s duplicate behaviour, or document the layering explicitly.

#### `IndexBackend::try_open` returns `Option<Box<dyn VectorIndex>>` but error semantics vs `Ok(None)` are unclear
- **Difficulty:** medium
- **Location:** src/index.rs:160 (`pub trait IndexBackend`)
- **Description:** Trait splits "not applicable, try next backend" (Ok(None)) from "store-level abort" (Err). But `IndexBackendError` only carries `Store` / `ChecksumMismatch` / `LoadFailed` — checksum/load failures are described in the doc comment as "self-handled with `tracing::warn!` + `Ok(None)`", meaning two of the three error variants are documented as never-emitted. So implementations have to internalise a "warn-and-return-Ok(None)" convention with no compile-time enforcement, while still importing the error type. Future backend authors will reasonably reach for `LoadFailed` and break the selector contract silently.
- **Suggested fix:** Either (a) drop `ChecksumMismatch`/`LoadFailed` and tighten the trait return to `Result<Option<...>, StoreError>`, or (b) add a `selector_action: enum { Skip, Abort }` to the error so the contract is encoded in types instead of comments.

#### `BatchProvider::set_on_item_complete` requires `&mut self` but other methods take `&self` — forces `Mutex` everywhere
- **Difficulty:** easy
- **Location:** src/llm/provider.rs:150
- **Description:** Trait method signature: `fn set_on_item_complete(&mut self, _cb: OnItemCallback) {}`. All four other methods take `&self`. Callers that hold a `&dyn BatchProvider` (most of the orchestration code) need to either rebuild the provider with the callback baked in or wrap it in a `Mutex<dyn BatchProvider>` just to call this one configuration setter. The comment justifies "callback may be invoked from multiple worker threads concurrently" — which is fine, but that's about callback invocation, not callback registration.
- **Suggested fix:** Builder pattern: take the callback at construction (`AnthropicBatchProvider::new(...).with_on_item_complete(cb)`), drop the trait method entirely. Keeps the provider `&self`-only and the trait shape uniform.

#### Wildcard `pub use` re-exports in `lib.rs` were collapsed for cross_project but `cross_project` is a special-case `pub mod` defined inside `lib.rs`
- **Difficulty:** easy
- **Location:** src/lib.rs:212-220
- **Description:** Per #1372/P3-52 the file replaced wildcard re-exports with explicit lists for `diff`/`gather`/`impact`/`scout`/`task`/`onboard`/`related`. But `cross_project` was special-cased as an inline `pub mod cross_project { pub use crate::impact::cross_project::...; pub use crate::store::calls::cross_project::...; }` instead of being an explicit re-export at the same level as the others. The single-file fix that the audit comment claimed ("each module now lists exactly what crosses the lib boundary") is undermined by this one ad-hoc nested module that pulls from two different crate paths and obscures where the types actually live.
- **Suggested fix:** Either (a) move `cross_project` types into a real `crate::cross_project` module that re-exports the originals, or (b) hoist the explicit `pub use` list to the top alongside the others (so all of `lib.rs`'s re-exports follow one pattern) and document why a virtual module is the right shape for these specific types.

#### Inconsistent `--limit` defaults: 3 (`where`), 5 (most), 10 (`gather`/`neighbors` actually 5/10), 20 (`eval`)
- **Difficulty:** easy (docs) / medium (semantics)
- **Location:** src/cli/args.rs:120/137/291/333/374/491/532/543/559, eval/mod.rs:52
- **Description:** `where` defaults to 3 file suggestions; `gather` to 10 chunks; everything else to 5; `eval` to 20. These are all "max results returned" but agents have to memorize five different defaults. Compare to the `-d` short flag and the `--commits` rename — recent work has been actively normalizing common knobs across commands. `--limit` (the most-used flag in the entire CLI) has no such effort.
- **Suggested fix:** Either (a) standardize all to 5, with the per-command rationale documented inline, or (b) at minimum harmonise gather/where to the dominant 5, and document `eval`'s 20 as an R@K-specific cap with a comment so it doesn't read as random.

DONE
