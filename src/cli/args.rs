//! Shared argument structs for CLI and batch commands.
//!
//! Each variant in the user-facing command surface embeds one of these structs
//! via `#[command(flatten)]`. Both the CLI path and the daemon batch path read
//! from the same arg struct, so adding a flag or changing a default happens
//! once and both paths pick it up.

use clap::Args;

use super::{parse_finite_f32, parse_nonzero_usize, parse_unit_f32};
use cqs::store::DeadConfidence;

// ============ depth-flag default rule ============
//
// Five graph commands take a "depth" knob. The spread
// (gather=1, impact=1, test-map=5, trace=10, onboard=3) is intentional;
// the named constants below document the per-command rationale.
//
// Two rule classes:
//
//   BLAST   — depth=1 by default. The chunk is a "what does *this*
//             thing reach" query. Direct callers / direct callees
//             only; deeper traversals usually swamp the answer
//             with chunks that don't materially change the
//             decision. Used by `gather`, `impact`.
//
//   WALK    — depth=3 by default. The chunk is a "trace this
//             concept across a few hops" query. Three is the
//             smallest depth where you reliably catch a chain like
//             "feature → orchestrator → primitive" without
//             expanding beyond what an agent can read in one
//             context window. Used by `onboard`. `test-map` keeps
//             depth=5 because tests are leaves on the call graph,
//             so the WALK has to descend further to reach them.
//
// `trace` keeps `--max-depth` semantics: it's a path-search (find
// route from A to B), not BFS-traversal, so its 10-step ceiling
// expresses "give up after 10 hops" rather than "expand 10 deep."
// `--depth` is accepted as an alias so the spelling matches the others.

/// Default `--limit` for graph commands (callers, callees, deps, impact,
/// test-map, trace, ...). Mirrors the top-level `Cli::limit` so a bare
/// `cqs <query>` and `cqs callers <name>` agree on the cap. Single source
/// for both the clap `LimitArg` default and the core `*Args` `Default` impls.
pub const DEFAULT_LIMIT: usize = 5;

/// Default for "blast radius" depth flags (gather, impact). One step
/// of direct callers / callees.
pub const DEFAULT_DEPTH_BLAST: usize = 1;

/// Default for "walk the call graph" depth flags (onboard). Three
/// hops covers feature → orchestrator → primitive without
/// over-expanding into chunks that don't materially change the
/// agent's mental model of the area.
pub const DEFAULT_DEPTH_WALK: usize = 3;

/// `test-map` walks deeper than the standard WALK because tests are
/// leaves on the call graph — depth 3 frequently misses test files
/// that are 4+ hops from a deep production chunk. Five is the smallest
/// value that catches typical project-level tests without dramatically
/// blowing up the response.
pub const DEFAULT_DEPTH_TEST_MAP: u16 = 5;

/// `trace` is path-search, not BFS-traversal. The flag is
/// `--max-depth` (with `--depth` alias) and means "give up looking for
/// a route between source and target after N hops." 10 is large enough
/// to find paths through long indirection chains but small enough that
/// an unreachable pair surfaces in <1s.
pub const DEFAULT_DEPTH_TRACE: u16 = 10;

/// Cross-encoder reranker mode for retrieval surfaces.
///
/// Search and eval share this flag shape. `--reranker none|onnx` is the
/// canonical form; `--help` lists only modes the binary supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum RerankerMode {
    /// No reranking — stage-1 retrieval is the final answer (default).
    None,
    /// Cross-encoder reranker via [`cqs::OnnxReranker`].
    Onnx,
}

/// Shared `--limit / -n` argument for graph commands (callers, callees, deps,
/// impact, test-map, trace, onboard, explain). Default mirrors the top-level
/// `Cli::limit` (= 5) so a bare `cqs <query>` and `cqs callers <name> -n N`
/// agree on the cap. Embedding this struct via `#[command(flatten)]` gives
/// every subcommand its own `--limit` while keeping the default in one place.
///
/// Rejects `--limit 0` at parse time — limit=0 is meaningless everywhere.
#[derive(Args, Debug, Clone)]
pub(crate) struct LimitArg {
    /// Max results to return (per category for impact/explain)
    #[arg(short = 'n', long, default_value_t = DEFAULT_LIMIT, value_parser = parse_nonzero_usize)]
    pub limit: usize,
}

/// The worktree-overlay tri-state flags, flattened into the commands whose SEED
/// retrieval the overlay shadows (Part A): `scout`, `gather`, `task`. The
/// `search` command carries the same three flags inline ([`SearchArgs`]); this
/// is the shared subset for the seed-overlaid graph-adjacent commands. Each
/// resolves activation through the same `resolve_overlay_active` precedence the
/// search surface uses, so no two surfaces can diverge.
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct OverlayArgs {
    /// Overlay the worktree's uncommitted/committed delta on top of the parent
    /// index so the SEED search reflects this checkout's edits, not main's.
    /// Default-on when run from a worktree; off in the main checkout. Tri-state
    /// env `CQS_WORKTREE_OVERLAY`: `1` forces on, `0` forces off, unset =
    /// default. `--overlay` forces on; `--no-overlay` forces off (opt-out wins).
    /// Phase 1 builds overlays on the daemon path only. The call-graph
    /// expansion stays on parent-truth — a `_meta.overlay_graph = "seed-only"`
    /// marker says so. Requires `--json` to forward to the daemon.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay: bool,

    /// Opt out of the worktree seed overlay even when run from a worktree
    /// (where it is default-on). The explicit-off counterpart of `--overlay`;
    /// equivalent to `CQS_WORKTREE_OVERLAY=0`. Opt-out wins over every opt-in.
    #[arg(long, conflicts_with = "overlay")]
    pub no_overlay: bool,

    /// Wire-only: the absolute worktree root to build the overlay for. Hidden
    /// from `--help` — computed by the CLI (`cqs::worktree::overlay_root`) and
    /// appended to the daemon-forwarded args, never set by a human. The daemon
    /// VALIDATES it (canonicalize + `resolve_main_project_dir == served root`)
    /// before reading any of its files.
    #[arg(long, hide = true)]
    pub overlay_root: Option<std::path::PathBuf>,
}

/// Arguments for semantic search: the flagship command. Shared between CLI
/// `search` (top-level + `cqs search …`) and batch `search`.
///
/// Single source of truth for every search knob. If a flag is valid for
/// search, it lives here.
#[derive(Args, Debug, Clone)]
pub(crate) struct SearchArgs {
    /// Search query (quote multi-word queries)
    pub query: String,

    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,

    /// Min similarity threshold
    ///
    /// NOTE: `-t` is intentionally overloaded across subcommands.
    /// In search/similar, it means "min similarity threshold" (default 0.3).
    /// In diff/drift, it means "match threshold" for identity (default 0.95).
    #[arg(short = 't', long, default_value = "0.3", value_parser = parse_finite_f32)]
    pub threshold: f32,

    /// Weight for name matching in hybrid search (0.0-1.0)
    ///
    /// `value_parser` is `parse_unit_f32` (bounded [0.0, 1.0]) so out-of-range
    /// values are rejected at parse time — a value > 1.0 would otherwise
    /// subtract from embedding weight and silently degrade search.
    #[arg(long, default_value = "0.2", value_parser = parse_unit_f32)]
    pub name_boost: f32,

    /// Filter by language
    #[arg(short = 'l', long)]
    pub lang: Option<String>,

    /// Include only these chunk types in results (e.g., function, struct, test, endpoint)
    #[arg(long, alias = "chunk-type")]
    pub include_type: Option<Vec<String>>,

    /// Exclude these chunk types from results (e.g., test, variable, configkey)
    #[arg(long)]
    pub exclude_type: Option<Vec<String>>,

    /// Filter by path pattern (glob)
    #[arg(short = 'p', long)]
    pub path: Option<String>,

    /// Filter by structural pattern (builder, error_swallow, async, mutex, unsafe, recursion)
    #[arg(long)]
    pub pattern: Option<String>,

    /// Definition search: find by name only, skip embedding (faster)
    #[arg(long)]
    pub name_only: bool,

    /// Enable RRF hybrid search (keyword + semantic fusion).
    #[arg(long)]
    pub rrf: bool,

    /// Include documentation, markdown, and config chunks in search results.
    #[arg(long)]
    pub include_docs: bool,

    /// Reranker mode: `none|onnx`.
    ///
    /// Mirrors `cqs eval --reranker`. `none` is the default; `onnx` runs the
    /// cross-encoder configured by `[reranker]` / `CQS_RERANKER_MODEL`.
    #[arg(long = "reranker", value_enum)]
    pub reranker: Option<RerankerMode>,

    /// Force-enable SPLADE sparse-dense hybrid search.
    ///
    /// Default behavior already runs SPLADE with per-category routing when the
    /// classifier matches a known category. This flag forces SPLADE on even
    /// for Unknown-category queries. Combine with `--splade-alpha` to pin
    /// a specific fusion weight across all categories.
    #[arg(long)]
    pub splade: bool,

    /// SPLADE fusion weight (None = use per-category router).
    ///
    /// When set, overrides the per-category router with a constant α for all
    /// queries: 1.0 = pure cosine, 0.0 = pure sparse. Leaving this unset lets
    /// `classify_query` pick per category (the production path).
    #[arg(long, value_parser = parse_finite_f32)]
    pub splade_alpha: Option<f32>,

    /// Show only file:line, no code
    #[arg(long)]
    pub no_content: bool,

    /// Show N lines of context before/after the chunk
    #[arg(short = 'C', long)]
    pub context: Option<usize>,

    /// Expand results with parent context (small-to-big retrieval)
    ///
    /// `--expand-parent` aligns with the top-level `Cli::expand_parent` flag
    /// (`src/cli/definitions.rs`). `--expand` is accepted as a visible alias.
    #[arg(long = "expand-parent", visible_alias = "expand")]
    pub expand_parent: bool,

    /// Search only this reference index (skip project index)
    #[arg(long = "ref")]
    pub ref_name: Option<String>,

    /// Include reference indexes in search results (default: project only)
    #[arg(long)]
    pub include_refs: bool,

    /// Maximum token budget for results (packs highest-scoring into budget)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,

    /// Disable staleness checks (skip per-file mtime comparison)
    #[arg(long)]
    pub no_stale_check: bool,

    /// Disable search-time demotion of test functions and underscore-prefixed names
    #[arg(long)]
    pub no_demote: bool,

    /// Suppress per-result `rank_signals` ranking provenance in JSON output
    /// (saves tokens on tight-budget calls). The text surface never emits it.
    #[arg(long)]
    pub no_rank_signals: bool,

    /// Overlay the worktree's uncommitted/committed delta on top of the parent
    /// index so results reflect this checkout's edits, not main's. Default-on
    /// when run from a worktree; off in the main checkout. Tri-state env
    /// `CQS_WORKTREE_OVERLAY`: `1` forces on, `0` forces off, unset = default.
    /// `--overlay` forces on; `--no-overlay` forces off (opt-out wins). Phase 1
    /// builds overlays on the daemon path only — a CLI-direct search (no daemon)
    /// serves the parent index with a `_meta.worktree_overlay =
    /// "skipped-no-daemon"` marker. Requires `--json` to forward to the daemon.
    #[arg(long, conflicts_with = "no_overlay")]
    pub overlay: bool,

    /// Opt out of the worktree search overlay even when run from a worktree
    /// (where it is default-on). The explicit-off counterpart of
    /// `--overlay`; equivalent to `CQS_WORKTREE_OVERLAY=0`. Opt-out wins over
    /// every opt-in signal.
    #[arg(long, conflicts_with = "overlay")]
    pub no_overlay: bool,

    /// Wire-only: the absolute worktree root to build the overlay for. Hidden
    /// from `--help` — it is computed by the CLI (`cqs::worktree::overlay_root`)
    /// and appended to the daemon-forwarded args, never set by a human. The
    /// daemon's cwd is the parent project and the wire request carries no cwd,
    /// so the client must say which worktree. The daemon VALIDATES it
    /// (canonicalize + `resolve_main_project_dir == served root`) before reading
    /// any of its files — an unvalidated value would be an arbitrary-directory
    /// read primitive over the socket.
    #[arg(long, hide = true)]
    pub overlay_root: Option<std::path::PathBuf>,
}

impl SearchArgs {
    /// Effective reranker mode. Returns `RerankerMode::None` when no flag is set.
    pub(crate) fn rerank_mode(&self) -> RerankerMode {
        self.reranker.unwrap_or(RerankerMode::None)
    }

    /// `true` if any reranker stage is selected (Onnx or Llm).
    pub(crate) fn rerank_active(&self) -> bool {
        !matches!(self.rerank_mode(), RerankerMode::None)
    }
}

/// Arguments shared between CLI `gather` and batch `gather`.
#[derive(Args, Debug, Clone)]
pub(crate) struct GatherArgs {
    /// Search query / question
    pub query: String,
    /// Call-graph BFS depth for gather expansion (0=seeds only, max 5).
    /// Shares `--depth`/`-d` with `onboard`/`impact`/`test-map`; `--expand`
    /// is accepted as a visible alias. BLAST default — direct callers /
    /// callees only.
    #[arg(short = 'd', long, default_value_t = DEFAULT_DEPTH_BLAST, visible_alias = "expand")]
    pub depth: usize,
    /// Expansion direction: both, callers, callees
    #[arg(long, default_value = "both")]
    pub direction: cqs::GatherDirection,
    /// Shared `--limit` arg via `LimitArg` flatten. Default 5.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Maximum token budget (overrides --limit with token-based packing)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Cross-index gather: seed from reference, bridge into project code
    #[arg(long = "ref")]
    pub ref_name: Option<String>,
    /// Worktree-overlay tri-state for the seed search (Part A).
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `impact` and batch `impact`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ImpactArgs {
    /// Function name or file:function
    pub name: String,
    /// Caller depth (1=direct, 2+=transitive). `-d` short flag matches
    /// `OnboardArgs::depth`. BLAST default — direct callers only.
    #[arg(short = 'd', long, default_value_t = DEFAULT_DEPTH_BLAST)]
    pub depth: usize,
    /// Suggest tests for untested callers
    #[arg(long)]
    pub suggest_tests: bool,
    /// Include type-impacted functions (via shared type dependencies)
    #[arg(long)]
    pub type_impact: bool,
    /// Query callers/impact across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Per-section truncation cap (callers, transitive_callers, tests,
    /// type_impacted). Defaults to 5 to match the top-level `Cli`.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Worktree-overlay tri-state for the direct-callers section (#1858 Part B).
    /// Built daemon-side only (phase 1); the CLI-direct adapter ignores it.
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `scout` and batch `scout`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ScoutArgs {
    /// Search query to investigate
    pub query: String,
    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Maximum token budget (includes chunk content within budget)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Override the number of search results retrieved before grouping
    /// (default: 15). Higher surfaces more candidate files.
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub search_limit: Option<usize>,
    /// Override the minimum search score threshold (default: 0.2). Lower
    /// admits weaker matches.
    #[arg(long, value_parser = parse_finite_f32)]
    pub search_threshold: Option<f32>,
    /// Override the min relative score gap that splits a ModifyTarget from a
    /// Dependency (default: 0.10). Lower yields more ModifyTargets.
    #[arg(long, value_parser = parse_finite_f32)]
    pub min_gap_ratio: Option<f32>,
    /// Worktree-overlay tri-state for the seed search (Part A).
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `context` and batch `context`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ContextArgs {
    /// File path relative to project root
    pub path: String,
    /// Return summary counts instead of full details
    #[arg(long)]
    pub summary: bool,
    /// Signatures-only TOC with caller/callee counts (no code bodies)
    #[arg(long)]
    pub compact: bool,
    /// Maximum token budget (includes chunk content within budget)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
}

/// Arguments shared between CLI `dead` and batch `dead`.
#[derive(Args, Debug, Clone)]
pub(crate) struct DeadArgs {
    /// Include public API functions in the main list
    #[arg(long)]
    pub include_pub: bool,
    /// Minimum confidence level to report
    #[arg(long, default_value = "low")]
    pub min_confidence: DeadConfidence,
    /// Restrict output to one verdict: `test-only`, `low-confidence-live`,
    /// `known-gap`, `dead`, or `unclassified`. `--verdict dead` is the
    /// actionable residue. Omit for all verdicts.
    #[arg(long, value_name = "VERDICT")]
    pub verdict: Option<String>,
    /// Worktree-overlay tri-state for the merged-graph dead computation (#1858
    /// Part B). Built daemon-side only (phase 1); the CLI-direct adapter ignores
    /// it.
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `similar` and batch `similar`.
#[derive(Args, Debug, Clone)]
pub(crate) struct SimilarArgs {
    /// Function name or file:function (e.g., "search_filtered" or "src/search.rs:search_filtered")
    pub name: String,
    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Min similarity threshold
    #[arg(short = 't', long, default_value = "0.3", value_parser = parse_finite_f32)]
    pub threshold: f32,
    /// Filter by language.
    ///
    /// On the CLI these scope flags reach `cmd_similar` via the top-level
    /// `Cli::lang`/`Cli::path`; carrying them on the shared `SimilarArgs` is
    /// what lets the daemon `dispatch_similar` honor the same scoping (the
    /// daemon translator forwards the top-level values onto this subcommand
    /// tail). Spellings mirror `SearchArgs` (`-l`/`--lang`, `-p`/`--path`).
    #[arg(short = 'l', long)]
    pub lang: Option<String>,
    /// Filter by path pattern (glob).
    #[arg(short = 'p', long)]
    pub path: Option<String>,
}

/// Arguments shared between CLI `blame` and batch `blame`.
#[derive(Args, Debug, Clone)]
pub(crate) struct BlameArgs {
    /// Function name or file:function
    pub name: String,
    /// Max commits to show.
    ///
    /// Spelled `--commits`/`-n` rather than `--depth` so blame doesn't share
    /// the `--depth` spelling with `onboard` (callee expansion depth) and
    /// `test-map` (call-chain BFS depth), which carry different semantics.
    ///
    /// Default 10 (not `LimitArg`'s 5) is intentional: this counts commits to
    /// show in the blame walk, not a result-set size, so it does not share
    /// `LimitArg`'s ceiling or default.
    #[arg(short = 'n', long, default_value = "10")]
    pub commits: usize,
    /// Also show callers of the function
    #[arg(long)]
    pub callers: bool,
}

/// Arguments shared between CLI `trace` and batch `trace`.
#[derive(Args, Debug, Clone)]
pub(crate) struct TraceArgs {
    /// Source function name or file:function
    pub source: String,
    /// Target function name or file:function
    pub target: String,
    /// Max search depth (1-50). `trace` is path-search (find route
    /// A → B), not BFS-traversal — `--max-depth` means "give up looking
    /// for a path after N hops." `--depth` is accepted as an alias so the
    /// spelling matches the rest of the depth-knob family even though the
    /// semantic differs.
    #[arg(
        short = 'd',
        long,
        visible_alias = "depth",
        default_value_t = DEFAULT_DEPTH_TRACE,
        value_parser = clap::value_parser!(u16).range(1..=50)
    )]
    pub max_depth: u16,
    /// Trace across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Cap on intermediate hops in the rendered path. Trace returns a
    /// single shortest path today; the cap applies to future k-shortest
    /// variants and to defensive truncation when path length exceeds
    /// expectation. Accepted for parity with other graph commands.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `callers`/`callees` and batch equivalents.
#[derive(Args, Debug, Clone)]
pub(crate) struct CallersArgs {
    /// Function name to search for
    pub name: String,
    /// Query callers across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Restrict to call edges of one provenance kind: `call` (syntactic),
    /// `serde_callback`, `macro_heuristic`, `fn_pointer`, or `doc_reference`.
    /// Omit for all kinds.
    #[arg(long, value_name = "KIND")]
    pub edge_kind: Option<String>,
    /// Cap on callers/callees returned. Defaults to 5 to match the
    /// top-level `Cli`. The handler truncates the post-resolution list before
    /// rendering — both text and JSON paths respect the cap.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Worktree-overlay tri-state for the call-graph query (#1858 Part B).
    /// Built daemon-side only (phase 1); the CLI-direct adapter ignores it.
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `deps` and batch `deps`.
#[derive(Args, Debug, Clone)]
pub(crate) struct DepsArgs {
    /// Type name (forward) or function name (with --reverse)
    pub name: String,
    /// Reverse: show types used by a function instead of type users
    #[arg(long)]
    pub reverse: bool,
    /// Query across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Cap on type users (forward) or used types (reverse). Defaults
    /// to 5 to match the top-level `Cli`. Truncated after fetch.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `test-map` and batch `test-map`.
#[derive(Args, Debug, Clone)]
pub(crate) struct TestMapArgs {
    /// Function name or file:function
    pub name: String,
    /// Max call chain depth to search. `-d` short flag matches
    /// `OnboardArgs::depth`. Deeper than the WALK default because tests are
    /// leaves on the call graph; depth=3 frequently misses test files 4+ hops
    /// from a deep production chunk. Range-bounded (1..=50, matching
    /// `TraceArgs::max_depth`) so a pathological `--depth usize::MAX` can't
    /// reach the BFS `+ 1` arithmetic and panic.
    #[arg(
        short = 'd',
        long,
        default_value_t = DEFAULT_DEPTH_TEST_MAP,
        value_parser = clap::value_parser!(u16).range(1..=50),
    )]
    pub depth: u16,
    /// Search for tests across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Cap on test matches returned. Defaults to 5 to match the
    /// top-level `Cli`. Applied after BFS, before rendering.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `related` and batch `related`.
#[derive(Args, Debug, Clone)]
pub(crate) struct RelatedArgs {
    /// Function name or file:function
    pub name: String,
    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `onboard` and batch `onboard`.
#[derive(Args, Debug, Clone)]
pub(crate) struct OnboardArgs {
    /// Concept or query to explore
    pub query: String,
    /// Call-chain expansion depth — WALK default.
    #[arg(short = 'd', long, default_value_t = DEFAULT_DEPTH_WALK)]
    pub depth: usize,
    /// Expansion direction: both, callers, callees. Defaults to `callees`,
    /// matching gather/test-map cross-command muscle memory.
    #[arg(long, default_value = "callees")]
    pub direction: cqs::GatherDirection,
    /// Maximum token budget
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Cap on call_chain + callers entries (entry_point always kept).
    /// Defaults to 5 to match the top-level `Cli`. Applies after `--depth`
    /// traversal and before `--tokens` packing.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `explain` and batch `explain`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ExplainArgs {
    /// Function name or file:function
    pub name: String,
    /// Maximum token budget (includes source content within budget)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Cap on callers/callees/similar lists in the function card.
    /// Defaults to 5 to match the top-level `Cli`. Applied per-section.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `where` and batch `where`.
#[derive(Args, Debug, Clone)]
pub(crate) struct WhereArgs {
    /// Description of the code to add
    pub description: String,
    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `plan` and batch `plan`.
#[derive(Args, Debug, Clone)]
pub(crate) struct PlanArgs {
    /// Task description to plan
    pub description: String,
    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Maximum token budget
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
}

/// Arguments shared between CLI `task` and batch `task`.
///
/// The `brief` flag is CLI-only for now (batch `task` doesn't surface it),
/// but lives here so a future flip to enabling it in batch is a no-op.
#[derive(Args, Debug, Clone)]
pub(crate) struct TaskArgs {
    /// Task description
    pub description: String,
    /// Shared `--limit` arg via `LimitArg` flatten.
    #[command(flatten)]
    pub limit_arg: LimitArg,
    /// Maximum token budget (waterfall across sections)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Compact output (~200 tokens): files, at-risk functions, test coverage
    #[arg(long)]
    pub brief: bool,
    /// Worktree-overlay tri-state for the scout-seed search (Part A).
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `read` and batch `read`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ReadArgs {
    /// File path relative to project root
    pub path: String,
    /// Focus on a specific function (returns only that function + type deps)
    #[arg(long)]
    pub focus: Option<String>,
}

/// Arguments shared between CLI `stale` and batch `stale`.
#[derive(Args, Debug, Clone)]
pub(crate) struct StaleArgs {
    /// Show counts only, skip file list
    #[arg(long)]
    pub count_only: bool,
}

/// Arguments shared between CLI `suggest` and batch `suggest`.
#[derive(Args, Debug, Clone)]
pub(crate) struct SuggestArgs {
    /// Apply suggestions (add notes to docs/notes.toml)
    #[arg(long)]
    pub apply: bool,
}

/// Arguments shared between CLI `diff` and batch `diff`.
#[derive(Args, Debug, Clone)]
pub(crate) struct DiffArgs {
    /// Source reference name
    pub source: String,
    /// Target reference (default: project)
    pub target: Option<String>,
    /// Similarity threshold for "modified" (default: 0.95)
    ///
    /// `-t` here means "match threshold" — pairs above this are "unchanged",
    /// below are "modified". Different from search's `-t` (min similarity 0.3).
    #[arg(short = 't', long, default_value = "0.95", value_parser = parse_finite_f32)]
    pub threshold: f32,
    /// Filter by language
    #[arg(short = 'l', long)]
    pub lang: Option<String>,
}

/// Arguments shared between CLI `drift` and batch `drift`.
#[derive(Args, Debug, Clone)]
pub(crate) struct DriftArgs {
    /// Reference name to compare against
    pub reference: String,
    /// Similarity threshold (default: 0.95). See Diff's `-t` doc.
    #[arg(short = 't', long, default_value = "0.95", value_parser = parse_finite_f32)]
    pub threshold: f32,
    /// Minimum drift to show (default: 0.0)
    #[arg(long, default_value = "0.0", value_parser = parse_finite_f32)]
    pub min_drift: f32,
    /// Filter by language
    #[arg(short = 'l', long)]
    pub lang: Option<String>,
    /// Maximum entries to show
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,
}

/// Arguments shared between CLI `review` and batch `review`.
///
/// The `stdin` flag is CLI-only (batch `review` reads the diff itself via
/// `base` and the working tree). Keeping it on the shared struct costs one
/// flag on the batch grammar but keeps the path symmetric.
#[derive(Args, Debug, Clone)]
pub(crate) struct ReviewArgs {
    /// Git ref to diff against (default: unstaged changes)
    #[arg(long)]
    pub base: Option<String>,
    /// Read diff from stdin instead of running git
    #[arg(long)]
    pub stdin: bool,
    /// Maximum token budget for output (truncates callers/tests lists)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Worktree-overlay tri-state for the direct-callers section (#1858 Part B).
    #[command(flatten)]
    pub overlay: OverlayArgs,
}

/// Arguments shared between CLI `ci` and batch `ci`.
#[derive(Args, Debug, Clone)]
pub(crate) struct CiArgs {
    /// Git ref to diff against (default: unstaged changes)
    #[arg(long)]
    pub base: Option<String>,
    /// Read diff from stdin instead of running git
    #[arg(long)]
    pub stdin: bool,
    /// Gate threshold: high, medium, off
    #[arg(long, default_value = "high")]
    pub gate: super::GateThreshold,
    /// Maximum token budget for output
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
}

/// Arguments shared between CLI `impact-diff` and batch `impact-diff`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ImpactDiffArgs {
    /// Git ref to diff against (default: unstaged changes)
    #[arg(long)]
    pub base: Option<String>,
    /// Read diff from stdin instead of running git
    #[arg(long)]
    pub stdin: bool,
}

/// Arguments shared between CLI `notes` (list subcommand) and batch `notes`.
///
/// Subcommand mutations (`add` / `update` / `remove`) remain on the CLI
/// `NotesCommand` subcommand enum and are not batch-dispatchable — see the
/// `BatchSupport` classifier for the policy.
///
/// `NotesCommand::List` flattens this struct (same pattern as
/// `Commands::Search { args: SearchArgs }`). The flattened fields include
/// `check`, which the daemon batch path picks up via
/// `BatchCmd::Notes { args, .. }`.
#[derive(Args, Debug, Clone)]
pub(crate) struct NotesListArgs {
    /// Show only warnings (negative sentiment)
    #[arg(long)]
    pub warnings: bool,
    /// Show only patterns (positive sentiment)
    #[arg(long)]
    pub patterns: bool,
    /// Filter by kind tag (e.g. `todo`, `design-decision`, `known-bug`).
    /// Matches against the v25 `notes.kind` column (kebab-case lowercase).
    /// ANDs with `--warnings` / `--patterns` when combined.
    #[arg(long)]
    pub kind: Option<String>,
    /// Check mentions for staleness (verifies files exist and symbols are in index)
    #[arg(long)]
    pub check: bool,
}

/// Arguments for the `index` command.
#[derive(Args, Debug, Clone)]
pub(crate) struct IndexArgs {
    /// Re-index all files, ignore mtime cache
    #[arg(long)]
    pub force: bool,
    /// Show what would be indexed (default writes the index).
    ///
    /// Per the CONTRIBUTING "Dry-Run vs Apply" rule, side-effect commands
    /// (`index`, `convert`) default to mutating; analyser commands (`doctor`,
    /// `suggest`) default to read-only and require `--fix`/`--apply` to mutate.
    #[arg(long)]
    pub dry_run: bool,
    /// Index files ignored by .gitignore
    #[arg(long)]
    pub no_ignore: bool,
    /// Skip the first-encounter prompt for committed `docs/notes.toml`.
    ///
    /// On the first index of a fresh repo containing `docs/notes.toml`, cqs
    /// prompts to confirm — committed notes affect search rankings and surface
    /// in agent context. Pass `--accept-shared-notes` to bypass the prompt for
    /// non-interactive use (CI, scripts). Acceptance is persisted to
    /// `.cqs/.accepted-shared-notes` so the prompt doesn't repeat.
    #[arg(long)]
    pub accept_shared_notes: bool,
    /// Generate LLM summaries for functions (requires ANTHROPIC_API_KEY)
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub llm_summaries: bool,
    /// Generate doc comments for undocumented functions (requires --llm-summaries).
    ///
    /// By default, writes proposed edits as unified-diff patches to
    /// `.cqs/proposed-docs/<rel>.patch` for human review. Apply with
    /// `git apply .cqs/proposed-docs/**/*.patch`. Pass `--apply` to write
    /// directly to source files without review.
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub improve_docs: bool,
    /// Write generated doc comments directly to source files instead of producing
    /// review patches under `.cqs/proposed-docs/`. Requires `--improve-docs`.
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub apply: bool,
    /// Regenerate doc comments for all functions, even those with existing docs (requires --improve-docs)
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub improve_all: bool,
    /// Maximum number of functions to generate docs for (used with --improve-docs)
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub max_docs: Option<usize>,
    /// Generate hyde query predictions for functions (requires ANTHROPIC_API_KEY)
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub hyde_queries: bool,
    /// Maximum number of functions to generate hyde predictions for
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub max_hyde: Option<usize>,
    /// Skip the post-pipeline prune of orphaned `llm_summaries` rows.
    ///
    /// Default (when omitted): orphan rows whose `content_hash` no longer
    /// matches any chunk are deleted at the end of the index pipeline so
    /// `cqs stats` reports honest summary coverage. Pass `--no-prune-summaries`
    /// when you plan to copy summaries to a sibling slot by content_hash —
    /// orphaned rows there may match chunks in the destination slot, and
    /// pruning before copy permanently loses those summaries (cross-slot
    /// summary reuse is the only workflow this matters for).
    #[arg(long)]
    pub no_prune_summaries: bool,
    /// Project chunk embeddings into 2D via UMAP and write to `chunks.umap_x/umap_y`.
    ///
    /// Enables the `cqs serve` cluster view (`?view=cluster`). Requires
    /// `umap-learn` Python package (`pip install umap-learn`). Skipped with
    /// a warning if Python or umap-learn is missing. Runs once per `cqs index`
    /// invocation; on large corpora (50k+ chunks) can take ~2 minutes CPU.
    #[arg(long)]
    pub umap: bool,
    /// Emit a structured JSON envelope summarizing the index run on
    /// completion. Suppresses progress prints in favor of a single
    /// `{indexed_files, indexed_chunks, took_ms, model, …}` summary so
    /// JSON-driven agents can chain `cqs init && cqs index --json`.
    #[arg(long)]
    pub json: bool,
}

/// Args for `BatchCmd::Reconcile`. Wrapped in a struct so the dispatch
/// table can hand the variant straight to its handler the same way every
/// other variant does (`dispatch_x(ctx, &args)`). Fields are advisory —
/// they ride along for tracing/logging and don't change the reconcile
/// algorithm itself.
#[derive(Args, Debug, Clone)]
pub(crate) struct ReconcileArgs {
    /// Name of the hook that fired this reconcile (e.g. `post-checkout`).
    /// Logged for operator diagnostics; not used for the walk itself.
    #[arg(long)]
    pub hook: Option<String>,
    /// Free-form positional payload from the hook (e.g. previous and
    /// current commit SHAs). Captured for tracing only.
    #[arg(long = "arg", value_name = "ARG")]
    pub args: Vec<String>,
}

/// Args for `BatchCmd::WaitFresh`. Wrapped in a struct so the dispatch
/// table can hand the variant straight to its handler under the uniform
/// `&args` shape.
#[derive(Args, Debug, Clone)]
pub(crate) struct WaitFreshArgs {
    /// Maximum seconds to block before returning the current
    /// (still-stale) snapshot. Capped server-side at 86_400 (24 h) for
    /// parity with the client-side cap in `wait_for_fresh`.
    #[arg(long, default_value_t = 60)]
    pub wait_secs: u64,
}
