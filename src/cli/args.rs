//! Shared argument structs for CLI and batch commands.
//! Eliminates duplication between Commands and BatchCmd enums.
//!
//! #947: each variant in the user-facing command surface should embed one of
//! these structs via `#[command(flatten)]`. Both the CLI path and the daemon
//! batch path read from the same arg struct, so adding a flag or changing a
//! default happens once and both paths pick it up automatically.

use clap::Args;

use super::{parse_finite_f32, parse_nonzero_usize};
use cqs::store::DeadConfidence;

/// Shared `--limit / -n` argument for graph commands that previously had no
/// per-subcommand limit (callers, callees, deps, impact, test-map, trace,
/// onboard, explain). Default mirrors the top-level `Cli::limit` (= 5) so a
/// bare `cqs <query>` and `cqs callers <name> -n N` agree on the cap.
///
/// Task A3: standardises `--limit` across every graph subcommand. Previously
/// only the top-level `Cli` accepted `--limit`, so batch users had no way to
/// cap graph output (`echo 'callers Foo --limit 5' | cqs batch` errored with
/// "unexpected argument"). Embedding this struct via `#[command(flatten)]`
/// gives every subcommand its own `--limit` while keeping the default in one
/// place.
#[derive(Args, Debug, Clone)]
pub(crate) struct LimitArg {
    /// Max results to return (per category for impact/explain)
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,
}

/// Arguments for semantic search: the flagship command. Shared between CLI
/// `search` (top-level + `cqs search …`) and batch `search`.
///
/// CQ-V1.25-1/4: this struct is the single source of truth for every search
/// knob. Previously `BatchCmd::Search` inline-duplicated 21 fields and
/// individual fields drifted (missing `--threshold`, missing `--pattern`,
/// etc.). If a flag is valid for search, it lives here.
#[derive(Args, Debug, Clone)]
pub(crate) struct SearchArgs {
    /// Search query (quote multi-word queries)
    pub query: String,

    /// Max results
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,

    /// Min similarity threshold
    ///
    /// NOTE: `-t` is intentionally overloaded across subcommands.
    /// In search/similar, it means "min similarity threshold" (default 0.3).
    /// In diff/drift, it means "match threshold" for identity (default 0.95).
    #[arg(short = 't', long, default_value = "0.3", value_parser = parse_finite_f32)]
    pub threshold: f32,

    /// Weight for name matching in hybrid search (0.0-1.0)
    #[arg(long, default_value = "0.2", value_parser = parse_finite_f32)]
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

    /// Re-rank results with cross-encoder (slower, more accurate)
    #[arg(long)]
    pub rerank: bool,

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
    /// queries: 1.0 = pure cosine, 0.0 = pure sparse, 0.7 was the legacy
    /// one-size default. Leaving this unset lets `classify_query` pick per
    /// category (the production path).
    #[arg(long, value_parser = parse_finite_f32)]
    pub splade_alpha: Option<f32>,

    /// Show only file:line, no code
    #[arg(long)]
    pub no_content: bool,

    /// Show N lines of context before/after the chunk
    #[arg(short = 'C', long)]
    pub context: Option<usize>,

    /// Expand results with parent context (small-to-big retrieval)
    #[arg(long)]
    pub expand: bool,

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
}

/// Arguments shared between CLI `gather` and batch `gather`.
#[derive(Args, Debug, Clone)]
pub(crate) struct GatherArgs {
    /// Search query / question
    pub query: String,
    /// Call graph expansion depth (0=seeds only, max 5)
    #[arg(long, default_value = "1")]
    pub expand: usize,
    /// Expansion direction: both, callers, callees
    #[arg(long, default_value = "both")]
    pub direction: cqs::GatherDirection,
    /// Max chunks to return
    #[arg(short = 'n', long, default_value = "10")]
    pub limit: usize,
    /// Maximum token budget (overrides --limit with token-based packing)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Cross-index gather: seed from reference, bridge into project code
    #[arg(long = "ref")]
    pub ref_name: Option<String>,
}

/// Arguments shared between CLI `impact` and batch `impact`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ImpactArgs {
    /// Function name or file:function
    pub name: String,
    /// Caller depth (1=direct, 2+=transitive)
    #[arg(long, default_value = "1")]
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
    /// Task A3: per-section truncation cap (callers, transitive_callers,
    /// tests, type_impacted). Defaults to 5 to match the top-level `Cli`.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `scout` and batch `scout`.
#[derive(Args, Debug, Clone)]
pub(crate) struct ScoutArgs {
    /// Search query to investigate
    pub query: String,
    /// Max file groups to return
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,
    /// Maximum token budget (includes chunk content within budget)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
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
}

/// Arguments shared between CLI `similar` and batch `similar`.
#[derive(Args, Debug, Clone)]
pub(crate) struct SimilarArgs {
    /// Function name or file:function (e.g., "search_filtered" or "src/search.rs:search_filtered")
    pub name: String,
    /// Max results
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,
    /// Min similarity threshold
    #[arg(short = 't', long, default_value = "0.3", value_parser = parse_finite_f32)]
    pub threshold: f32,
}

/// Arguments shared between CLI `blame` and batch `blame`.
#[derive(Args, Debug, Clone)]
pub(crate) struct BlameArgs {
    /// Function name or file:function
    pub name: String,
    /// Max commits to show.
    ///
    /// API-V1.22-4: renamed from `--depth`/`-d` to `--commits`/`-n` so blame
    /// stops sharing the `--depth` spelling with `onboard` (callee expansion
    /// depth) and `test-map` (call-chain BFS depth) — three commands had three
    /// different semantics under the same flag name. Hard rename, no alias —
    /// internal-only tool, see CLAUDE.md "No External Users".
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
    /// Max search depth (1-50)
    #[arg(long, default_value = "10", value_parser = clap::value_parser!(u16).range(1..=50))]
    pub max_depth: u16,
    /// Trace across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Task A3: cap on intermediate hops in the rendered path. Trace
    /// returns a single shortest path today; the cap applies to future
    /// k-shortest variants and to defensive truncation when path length
    /// exceeds expectation. Accepted for parity with other graph commands.
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
    /// Task A3: cap on callers/callees returned. Defaults to 5 to match the
    /// top-level `Cli`. The handler truncates the post-resolution list before
    /// rendering — both text and JSON paths respect the cap.
    #[command(flatten)]
    pub limit_arg: LimitArg,
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
    /// Task A3: cap on type users (forward) or used types (reverse). Defaults
    /// to 5 to match the top-level `Cli`. Truncated after fetch.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `test-map` and batch `test-map`.
#[derive(Args, Debug, Clone)]
pub(crate) struct TestMapArgs {
    /// Function name or file:function
    pub name: String,
    /// Max call chain depth to search
    #[arg(long, default_value = "5")]
    pub depth: usize,
    /// Search for tests across all configured reference projects
    #[arg(long)]
    pub cross_project: bool,
    /// Task A3: cap on test matches returned. Defaults to 5 to match the
    /// top-level `Cli`. Applied after BFS, before rendering.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `related` and batch `related`.
#[derive(Args, Debug, Clone)]
pub(crate) struct RelatedArgs {
    /// Function name or file:function
    pub name: String,
    /// Max results per category
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,
}

/// Arguments shared between CLI `onboard` and batch `onboard`.
#[derive(Args, Debug, Clone)]
pub(crate) struct OnboardArgs {
    /// Concept or query to explore
    pub query: String,
    /// Callee expansion depth
    #[arg(short = 'd', long, default_value = "3")]
    pub depth: usize,
    /// Maximum token budget
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Task A3: cap on call_chain + callers entries (entry_point always
    /// kept). Defaults to 5 to match the top-level `Cli`. Applies after
    /// `--depth` traversal and before `--tokens` packing.
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
    /// Task A3: cap on callers/callees/similar lists in the function card.
    /// Defaults to 5 to match the top-level `Cli`. Applied per-section.
    #[command(flatten)]
    pub limit_arg: LimitArg,
}

/// Arguments shared between CLI `where` and batch `where`.
#[derive(Args, Debug, Clone)]
pub(crate) struct WhereArgs {
    /// Description of the code to add
    pub description: String,
    /// Max file suggestions
    #[arg(short = 'n', long, default_value = "3")]
    pub limit: usize,
}

/// Arguments shared between CLI `plan` and batch `plan`.
#[derive(Args, Debug, Clone)]
pub(crate) struct PlanArgs {
    /// Task description to plan
    pub description: String,
    /// Max scout file groups
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,
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
    /// Max file groups to return
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,
    /// Maximum token budget (waterfall across sections)
    #[arg(long, value_parser = parse_nonzero_usize)]
    pub tokens: Option<usize>,
    /// Compact output (~200 tokens): files, at-risk functions, test coverage
    #[arg(long)]
    pub brief: bool,
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
#[derive(Args, Debug, Clone)]
pub(crate) struct NotesListArgs {
    /// Show only warnings (negative sentiment)
    #[arg(long)]
    pub warnings: bool,
    /// Show only patterns (positive sentiment)
    #[arg(long)]
    pub patterns: bool,
}

/// Arguments for the `index` command.
#[derive(Args, Debug, Clone)]
pub(crate) struct IndexArgs {
    /// Re-index all files, ignore mtime cache
    #[arg(long)]
    pub force: bool,
    /// Show what would be indexed (default writes the index).
    ///
    /// Audit P2 #38: per the CONTRIBUTING "Dry-Run vs Apply" rule, side-effect
    /// commands (`index`, `convert`) default to mutating; analyser commands
    /// (`doctor`, `suggest`) default to read-only and require `--fix`/`--apply`
    /// to mutate. TODO(docs-agent): document this rule in CONTRIBUTING.md.
    #[arg(long)]
    pub dry_run: bool,
    /// Index files ignored by .gitignore
    #[arg(long)]
    pub no_ignore: bool,
    /// Generate LLM summaries for functions (requires ANTHROPIC_API_KEY)
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub llm_summaries: bool,
    /// Generate and write back doc comments for undocumented functions (requires --llm-summaries)
    #[cfg(feature = "llm-summaries")]
    #[arg(long)]
    pub improve_docs: bool,
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
}
