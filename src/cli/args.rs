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

    /// Enable SPLADE sparse-dense hybrid search (requires SPLADE model)
    #[arg(long)]
    pub splade: bool,

    /// SPLADE fusion weight: 1.0 = pure cosine, 0.0 = pure sparse (default: 0.7)
    #[arg(long, default_value = "0.7", value_parser = parse_finite_f32)]
    pub splade_alpha: f32,

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
    /// Max commits to show
    #[arg(short = 'd', long, default_value = "10")]
    pub depth: usize,
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
}

/// Arguments for the `index` command.
#[derive(Args, Debug, Clone)]
pub(crate) struct IndexArgs {
    /// Re-index all files, ignore mtime cache
    #[arg(long)]
    pub force: bool,
    /// Show what would be indexed, don't write
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
