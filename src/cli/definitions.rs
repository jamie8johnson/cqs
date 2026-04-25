//! Clap argument definitions: CLI struct, subcommand enum, output types.

use clap::{Parser, Subcommand};

use super::args;

// EX-4: Verify clap default_value strings match the actual constants.
// Integer constants can use compile-time assert; f32 checked in tests below.
const _: () = assert!(crate::cli::config::DEFAULT_LIMIT == 5);

/// Output format for commands that support text/json/mermaid
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Mermaid,
}

/// Parse an `OutputFormat` that only allows text or json (rejects mermaid at parse time).
impl std::fmt::Display for OutputFormat {
    /// Formats the enum variant as a human-readable string representation.
    ///
    /// # Arguments
    ///
    /// * `f` - The formatter to write the output to
    ///
    /// # Returns
    ///
    /// A `std::fmt::Result` indicating success or formatting error
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => write!(f, "text"),
            Self::Json => write!(f, "json"),
            Self::Mermaid => write!(f, "mermaid"),
        }
    }
}

/// AD-49: Common output format arguments shared across commands that support text/json/mermaid.
#[derive(Clone, Debug, clap::Args)]
pub struct OutputArgs {
    /// Output format: text, json, mermaid (use --json as shorthand for --format json)
    #[arg(long, default_value = "text")]
    pub format: OutputFormat,
    /// Shorthand for --format json
    #[arg(long, conflicts_with = "format")]
    pub json: bool,
}

impl OutputArgs {
    /// Resolve the effective format (--json overrides --format).
    pub fn effective_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format.clone()
        }
    }
}

/// AD-49 + v1.22.0 audit API-1: Output format for commands that only support
/// text or JSON. Previously exposed `--format text|json` alongside `--json`,
/// but 25+ command handlers read `output.json` directly and never checked
/// `output.format`, so `--format json` was silently accepted and ignored.
/// Removed `--format`; commands that genuinely support multiple output formats
/// (e.g. `--format mermaid`) use [`OutputArgs`] instead.
#[derive(Clone, Debug, clap::Args)]
pub struct TextJsonArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl TextJsonArgs {
    pub fn effective_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        }
    }
}

/// Re-export `GateThreshold` so CLI and batch code can reference it directly.
pub use cqs::ci::GateThreshold;

/// Audit mode state for the audit-mode command
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum AuditModeState {
    /// Enable audit mode
    On,
    /// Disable audit mode
    Off,
}

/// Parse a non-zero usize for --tokens validation
pub(crate) fn parse_nonzero_usize(s: &str) -> std::result::Result<usize, String> {
    let val: usize = s.parse().map_err(|e| format!("{e}"))?;
    if val == 0 {
        return Err("value must be at least 1".to_string());
    }
    Ok(val)
}

/// Validate that a float parameter is finite (not NaN or Infinity).
pub(crate) fn validate_finite_f32(val: f32, name: &str) -> anyhow::Result<f32> {
    if val.is_finite() {
        Ok(val)
    } else {
        anyhow::bail!("Invalid {name}: {val} (must be a finite number)")
    }
}

/// Clap-compatible parser for finite f32 flags.
///
/// API-V1.25-7: used as `value_parser` on every f32 CLI flag to reject
/// `NaN` / `Infinity` / `-Infinity` at argument-parse time, before the value
/// can flow into scoring, thresholds, or filter construction. The signature
/// matches clap's expectation (`fn(&str) -> Result<T, String>`), unlike
/// `validate_finite_f32` which runs on an already-parsed f32.
pub(crate) fn parse_finite_f32(s: &str) -> std::result::Result<f32, String> {
    let val: f32 = s.parse().map_err(|e| format!("{e}"))?;
    if val.is_finite() {
        Ok(val)
    } else {
        Err(format!(
            "value must be a finite number, got {val} (NaN/Infinity rejected)"
        ))
    }
}

/// AC-V1.29-5: finite-f32 bounded to the `[0.0, 1.0]` unit interval. Used as
/// `value_parser` for CLI flags that encode a weight or blending fraction
/// (e.g., `--name-boost`, `cqs ref add --weight`) where out-of-range values
/// silently corrupt scoring instead of surfacing a clap error. Rejects NaN /
/// ±Inf via the underlying `parse_finite_f32`, then fences the range.
pub(crate) fn parse_unit_f32(s: &str) -> std::result::Result<f32, String> {
    let v = parse_finite_f32(s)?;
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("value must be in [0.0, 1.0], got {v}"));
    }
    Ok(v)
}

#[derive(Parser)]
#[command(name = "cqs")]
#[command(about = "Semantic code search with local embeddings")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub(super) command: Option<Commands>,

    /// Search query (quote multi-word queries)
    pub query: Option<String>,

    /// Max results
    #[arg(short = 'n', long, default_value = "5")]
    pub limit: usize,

    /// Min similarity threshold
    ///
    /// NOTE: `-t` is intentionally overloaded across subcommands.
    /// In search/similar (here and top-level), it means "min similarity threshold" (default 0.3).
    /// In diff/drift, it means "match threshold" for identity (default 0.95).
    /// The semantics differ because the baseline similarity differs: search returns
    /// low-similarity results worth filtering, while diff/drift compare known pairs
    /// where 0.95+ means "unchanged".
    #[arg(short = 't', long, default_value = "0.3", value_parser = parse_finite_f32)]
    pub threshold: f32,

    /// Weight for name matching in hybrid search (0.0-1.0)
    ///
    /// AC-V1.29-5: bounded parser — see `SearchArgs::name_boost`.
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

    /// Enable RRF hybrid search (keyword + semantic fusion). Off by default — pure cosine is faster and scores higher on expanded eval.
    #[arg(long)]
    pub rrf: bool,

    /// Include documentation, markdown, and config chunks in search results. Default: code only.
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

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Show only file:line, no code
    #[arg(long)]
    pub no_content: bool,

    /// Show N lines of context before/after the chunk
    #[arg(short = 'C', long)]
    pub context: Option<usize>,

    /// Expand search results with their parent type/module context (small-to-big retrieval).
    ///
    /// API-V1.22-3: renamed from `--expand` to `--expand-parent` to disambiguate
    /// from `gather --expand <N>` (graph depth, `usize`). Same flag name with two
    /// incompatible types had bitten agents that batched both commands. The old
    /// `--expand` form is no longer accepted; switch scripts to `--expand-parent`.
    #[arg(long = "expand-parent")]
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

    /// Suppress progress output
    #[arg(short, long)]
    pub quiet: bool,

    /// Disable staleness checks (skip per-file mtime comparison)
    #[arg(long)]
    pub no_stale_check: bool,

    /// Disable search-time demotion of test functions and underscore-prefixed names
    #[arg(long)]
    pub no_demote: bool,

    /// Embedding model: bge-large (default), e5-base, or custom
    #[arg(long)]
    pub model: Option<String>,

    /// Named slot to use (overrides `CQS_SLOT` env and `.cqs/active_slot`).
    ///
    /// Slots are project-scoped, side-by-side full indexes living under
    /// `.cqs/slots/<name>/`. Default behaviour is to read the active slot
    /// from `.cqs/active_slot` (falls back to `default`). Spec:
    /// `docs/plans/2026-04-24-embeddings-cache-and-slots.md`.
    #[arg(long, global = true)]
    pub slot: Option<String>,

    /// Show debug info (sets RUST_LOG=debug)
    #[arg(short, long)]
    pub verbose: bool,

    /// Resolved model config (set by dispatch, not CLI).
    #[arg(skip)]
    pub resolved_model: Option<cqs::embedder::ModelConfig>,
}

impl Cli {
    /// Get the resolved model config, returning an error if not yet resolved.
    ///
    /// Prefer this over [`model_config`] in new code — it propagates a proper
    /// error instead of panicking.
    pub fn try_model_config(&self) -> anyhow::Result<&cqs::embedder::ModelConfig> {
        self.resolved_model
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ModelConfig not resolved — call resolve_model() first"))
    }
}

#[derive(Subcommand)]
pub(super) enum Commands {
    /// Download model and create .cqs/
    Init,
    /// One-line-per-function summary for a file
    Brief {
        /// File path (as stored in index, e.g. src/lib.rs)
        path: String,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Check model, index, hardware
    Doctor {
        /// Auto-fix detected issues (stale→index, schema→migrate)
        #[arg(long)]
        fix: bool,
        /// Dump full setup introspection: resolved model config, env vars,
        /// daemon socket state, index metadata, config precedence.
        ///
        /// Use this when queries return zero results or agents hit weird
        /// model/daemon state — `--verbose` surfaces the cause in one call.
        #[arg(long)]
        verbose: bool,
        /// Emit a single JSON document on stdout containing both the check
        /// results and the verbose introspection. Implies `--verbose`.
        ///
        /// Colored human-readable check progress is routed to stderr in this
        /// mode so `cqs doctor --json | jq` works.
        #[arg(long)]
        json: bool,
    },
    /// Index current project
    Index {
        #[command(flatten)]
        args: args::IndexArgs,
    },
    /// Show index statistics
    Stats {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Watch for changes and reindex
    Watch {
        /// Debounce interval in milliseconds. Default 500ms suits inotify
        /// on native Linux; WSL DrvFS (/mnt/) and --poll mode auto-bump to
        /// 1500ms because NTFS mtime resolution is 1s. Override here or
        /// via CQS_WATCH_DEBOUNCE_MS (takes precedence over the flag).
        #[arg(long, default_value = "500")]
        debounce: u64,
        /// Index files ignored by .gitignore
        #[arg(long)]
        no_ignore: bool,
        /// Use polling instead of inotify (reliable on WSL /mnt/ paths)
        #[arg(long)]
        poll: bool,
        /// Also listen on a Unix socket for query requests (daemon mode)
        #[arg(long)]
        serve: bool,
    },
    /// What functions, callers, and tests are affected by current diff
    Affected {
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Read diff from stdin instead of running git.
        ///
        /// API-V1.22-6: brings `affected` in line with `review`, `ci`, and
        /// `impact-diff`, which already accept `--stdin`. Lets agents pipe a
        /// captured diff (`git diff main | cqs affected --stdin --json`)
        /// without re-shelling git.
        #[arg(long)]
        stdin: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Batch mode: read commands from stdin, output JSONL
    Batch,
    /// Semantic git blame: who changed a function, when, and why
    Blame {
        #[command(flatten)]
        args: args::BlameArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Interactive REPL for cqs commands
    Chat,
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Show type dependencies: who uses a type, or what types a function uses
    Deps {
        #[command(flatten)]
        args: args::DepsArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions that call a given function
    Callers {
        #[command(flatten)]
        args: args::CallersArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions called by a given function
    Callees {
        #[command(flatten)]
        args: args::CallersArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Guided codebase tour: entry point → call chain → types → tests
    Onboard {
        #[command(flatten)]
        args: args::OnboardArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Brute-force nearest neighbors for a function by cosine similarity
    Neighbors {
        /// Function name or file:function
        name: String,
        /// Max neighbors to return
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// List and manage notes
    Notes {
        #[command(subcommand)]
        subcmd: NotesCommand,
    },
    /// Manage reference indexes for multi-index search
    Ref {
        #[command(subcommand)]
        subcmd: RefCommand,
    },
    /// Semantic diff between indexed snapshots
    Diff {
        #[command(flatten)]
        args: args::DiffArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Detect semantic drift between a reference and the project
    Drift {
        #[command(flatten)]
        args: args::DriftArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Generate a function card (signature, callers, callees, similar)
    Explain {
        #[command(flatten)]
        args: args::ExplainArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find code similar to a given function
    Similar {
        #[command(flatten)]
        args: args::SimilarArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Impact analysis: what breaks if you change a function
    Impact {
        #[command(flatten)]
        args: args::ImpactArgs,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Impact analysis from a git diff — what callers and tests are affected
    #[command(name = "impact-diff")]
    ImpactDiff {
        #[command(flatten)]
        args: args::ImpactDiffArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Comprehensive diff review: impact + notes + risk scoring
    Review {
        #[command(flatten)]
        args: args::ReviewArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// CI pipeline analysis: impact + risk + dead code + gate
    Ci {
        #[command(flatten)]
        args: args::CiArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Trace call chain between two functions
    Trace {
        #[command(flatten)]
        args: args::TraceArgs,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Find tests that exercise a function
    TestMap {
        #[command(flatten)]
        args: args::TestMapArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// What do I need to know to work on this file
    Context {
        #[command(flatten)]
        args: args::ContextArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions with no callers (dead code detection)
    Dead {
        #[command(flatten)]
        args: args::DeadArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Gather minimal code context to answer a question
    Gather {
        #[command(flatten)]
        args: args::GatherArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Manage cross-project search registry
    Project {
        #[command(subcommand)]
        subcmd: ProjectCommand,
    },
    /// Remove stale chunks and rebuild index
    Gc {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Codebase quality snapshot — dead code, staleness, hotspots, coverage
    Health {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Toggle audit mode (exclude notes from search/read)
    #[command(name = "audit-mode")]
    AuditMode {
        /// State: on or off (omit to query current state)
        state: Option<AuditModeState>,
        /// Expiry duration (e.g., "30m", "1h", "2h30m")
        #[arg(long, default_value = "30m")]
        expires: String,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Usage telemetry dashboard — command frequency, categories, sessions
    Telemetry {
        /// Reset: archive current telemetry and start fresh
        #[arg(long)]
        reset: bool,
        /// Reason for reset (used in the reset event)
        #[arg(long, requires = "reset")]
        reason: Option<String>,
        /// Include archived telemetry files
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Check index freshness — list stale and missing files
    Stale {
        #[command(flatten)]
        args: args::StaleArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Auto-suggest notes from codebase patterns (dead code, untested hotspots)
    Suggest {
        #[command(flatten)]
        args: args::SuggestArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Read a file with notes injected as comments
    Read {
        #[command(flatten)]
        args: args::ReadArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Reconstruct source file from index (works without source on disk)
    Reconstruct {
        /// File path (as indexed)
        path: String,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions related by shared callers, callees, or types
    Related {
        #[command(flatten)]
        args: args::RelatedArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Suggest where to add new code matching a description
    Where {
        #[command(flatten)]
        args: args::WhereArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Pre-investigation dashboard: search, group, count callers/tests, check staleness
    Scout {
        #[command(flatten)]
        args: args::ScoutArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Task planning with template classification: classify + scout + checklist
    Plan {
        #[command(flatten)]
        args: args::PlanArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// One-shot implementation context: scout + code + impact + placement + notes
    Task {
        #[command(flatten)]
        args: args::TaskArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Convert documents (PDF, HTML, CHM) to Markdown
    #[cfg(feature = "convert")]
    Convert {
        /// File or directory to convert
        path: String,
        /// Output directory for .md files [default: same as input]
        #[arg(short = 'o', long)]
        output: Option<String>,
        /// Overwrite existing .md files
        #[arg(long)]
        overwrite: bool,
        /// Preview conversions (default writes the .md files).
        ///
        /// Audit P2 #38: per the CONTRIBUTING "Dry-Run vs Apply" rule, side-effect
        /// commands (`index`, `convert`) default to mutating; analyser commands
        /// (`doctor`, `suggest`) default to read-only and require `--fix`/`--apply`
        /// to mutate. TODO(docs-agent): document this rule in CONTRIBUTING.md.
        #[arg(long)]
        dry_run: bool,
        /// Cleaning rule tags (comma-separated, e.g. "aveva,generic") [default: all]
        #[arg(long)]
        clean_tags: Option<String>,
    },
    /// Export a HuggingFace model to ONNX format for use with cqs
    ExportModel {
        /// HuggingFace model repo ID
        #[arg(long)]
        repo: String,
        /// Output directory
        #[arg(long, default_value = ".")]
        output: std::path::PathBuf,
        /// Embedding dimension override (auto-detected from config.json if omitted)
        #[arg(long)]
        dim: Option<u64>,
    },
    /// Generate training data for fine-tuning from git history
    TrainData {
        /// Paths to git repositories to process
        #[arg(long, required = true, num_args = 1..)]
        repos: Vec<std::path::PathBuf>,
        /// Output JSONL file path
        #[arg(long)]
        output: std::path::PathBuf,
        /// Maximum commits to process per repo (omit for unlimited).
        ///
        /// API-V1.22-13: `Option<usize>` (`None` = unlimited). Was `usize` with
        /// `0` as a magic sentinel — now matches `TrainPairs::limit`.
        #[arg(long)]
        max_commits: Option<usize>,
        /// Minimum commit message length to include
        #[arg(long, default_value = "15")]
        min_msg_len: usize,
        /// Maximum files changed per commit to include
        #[arg(long, default_value = "20")]
        max_files: usize,
        /// Maximum identical-query triplets (dedup cap)
        #[arg(long, default_value = "5")]
        dedup_cap: usize,
        /// Resume from checkpoint
        #[arg(long)]
        resume: bool,
        /// Verbose output
        #[arg(long)]
        verbose: bool,
    },
    /// Extract (NL, code) training pairs from index as JSONL
    TrainPairs {
        /// Output JSONL file path.
        ///
        /// API-V1.22-13: `PathBuf` (was `String`) so the same file-path concept
        /// uses one type across both training commands.
        #[arg(long)]
        output: std::path::PathBuf,
        /// Max pairs to extract (omit for unlimited)
        ///
        /// API-V1.29-7: `-n` short flag added for parity with other result-cap
        /// knobs across the CLI surface.
        #[arg(short = 'n', long)]
        limit: Option<usize>,
        /// Filter by language (e.g., "Rust", "Python")
        #[arg(long)]
        language: Option<String>,
        /// Add contrastive prefixes from call graph callees
        #[arg(long)]
        contrastive: bool,
    },
    /// Manage the embeddings cache (stats, prune, compact). Project-scoped
    /// at `<project>/.cqs/embeddings_cache.db`.
    Cache {
        #[command(subcommand)]
        subcmd: CacheCommand,
    },
    /// Manage named slots — side-by-side full indexes under `.cqs/slots/<name>/`
    Slot {
        #[command(subcommand)]
        subcmd: SlotCommand,
    },
    /// Daemon healthcheck — show daemon model, uptime, and counters
    ///
    /// Connects to the running daemon socket and prints its current state.
    /// Exits 1 if no daemon is running. Use `--json` for machine-readable
    /// output. Reuses [`cqs::daemon_translate::daemon_ping`] under the hood
    /// so other tools (e.g. `cqs doctor --verbose`) can pull the same data.
    Ping {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Invalidate daemon caches and re-open the Store
    ///
    /// API-V1.29-6: exposes the existing `BatchCmd::Refresh` handler at the
    /// top-level CLI so agents can trigger `ctx.invalidate()` without the
    /// `cqs batch` JSON dance. No-op when no daemon is running — a fresh CLI
    /// process has no caches to invalidate.
    #[command(visible_alias = "invalidate")]
    Refresh,
    /// First-class eval harness: run query set against current index, print R@K
    Eval {
        #[command(flatten)]
        args: super::commands::EvalCmdArgs,
    },
    /// Show / list / swap the embedding model recorded in the index
    Model {
        #[command(subcommand)]
        subcmd: ModelCommand,
    },
    /// Start the cqs serve web UI (call graph + chunk detail).
    ///
    /// Binds to `127.0.0.1:8080` by default. Read-only — single-user
    /// local exploration. Pair `--open` to launch a browser tab.
    /// Spec: `docs/plans/2026-04-21-cqs-serve-v1.md`.
    #[cfg(feature = "serve")]
    Serve {
        /// TCP port to bind. Default 8080.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Bind address. Default 127.0.0.1 — anything else exposes the
        /// (un-authenticated) server beyond localhost. Spec'd as a
        /// deliberate decision, not an oversight.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Open the system browser on start.
        #[arg(long)]
        open: bool,
    },
}

// Re-export the subcommand types used in Commands variants
pub(super) use super::commands::{
    CacheCommand, ModelCommand, NotesCommand, ProjectCommand, RefCommand, SlotCommand,
};

/// Classifier used by `try_daemon_query` to decide whether a CLI command can
/// be forwarded to the batch daemon.
///
/// #947: replaces the hand-maintained allowlist that previously lived inline
/// in `try_daemon_query`. The rule is simple: every `Commands` variant must
/// classify itself here, the `match` is exhaustive (no wildcard), and adding
/// a new CLI variant without picking a classification fails to compile.
///
/// The policy:
/// - `Cli`: command is CLI-only, do not forward. Reasons include process
///   lifecycle (init/index/watch/chat/completions), read-write store access
///   (gc, notes mutations, suggest --apply — batch holds a `Store<ReadOnly>`),
///   or not-yet-implemented on the batch side.
/// - `Daemon`: command has a matching `BatchCmd` variant and can be forwarded.
/// - `DaemonIfReadonly(&dyn Fn())`: for `Notes`, only the `list` subcommand
///   is daemon-compatible; mutations (`add` / `update` / `remove`) must hit
///   the CLI so the filesystem reindex runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BatchSupport {
    /// CLI-only: daemon path should return early (None) and fall through.
    Cli,
    /// Daemon-dispatchable: forward to batch handler.
    Daemon,
}

impl Commands {
    /// Classify this variant for daemon dispatch.
    ///
    /// Exhaustive match: adding a new `Commands` variant forces an explicit
    /// classification decision. This is the whole point of the refactor —
    /// no more silent daemon-forwarding drift.
    pub(crate) fn batch_support(&self) -> BatchSupport {
        match self {
            // Process / lifecycle — never daemon.
            Commands::Init
            | Commands::Index { .. }
            | Commands::Watch { .. }
            | Commands::Batch
            | Commands::Chat
            | Commands::Completions { .. }
            | Commands::Doctor { .. }
            // Telemetry / audit / training — CLI-only tooling.
            | Commands::AuditMode { .. }
            | Commands::Telemetry { .. }
            | Commands::TrainData { .. }
            | Commands::TrainPairs { .. }
            | Commands::Cache { .. }
            | Commands::Slot { .. }
            // Registry commands — not on batch surface.
            | Commands::Ref { .. }
            | Commands::Project { .. }
            | Commands::ExportModel { .. }
            // Task B2: `ping` is a CLI-only healthcheck. The daemon side
            // exposes a `BatchCmd::Ping` handler, but the CLI handler
            // (`cmd_ping`) talks to the socket directly so it can return a
            // distinct exit code when no daemon is running. Routing through
            // `try_daemon_query` would silently fall through to a
            // store-opening CLI path — which we explicitly do not want.
            | Commands::Ping { .. }
            // Not-yet-on-batch commands. Candidates for a future BatchCmd.
            | Commands::Affected { .. }
            | Commands::Brief { .. }
            | Commands::Neighbors { .. }
            | Commands::Reconstruct { .. }
            // Eval is a long-running per-process operation (file I/O, progress
            // to stderr, optional --save side effect). Not a fit for daemon
            // dispatch — runs inline via the CLI store path.
            | Commands::Eval { .. }
            // Model swaps mutate `.cqs/`, restart the daemon, and may take
            // minutes to reindex — exclusively a CLI operation.
            | Commands::Model { .. } => BatchSupport::Cli,

            // cqs serve is a long-running HTTP server — never daemon-dispatched.
            #[cfg(feature = "serve")]
            Commands::Serve { .. } => BatchSupport::Cli,

            #[cfg(feature = "convert")]
            Commands::Convert { .. } => BatchSupport::Cli,

            // Notes: list is daemon-compatible; mutations must hit CLI for
            // the filesystem reindex. Inline-decide here so the call-site stays
            // trivial.
            Commands::Notes { subcmd } => match subcmd {
                NotesCommand::List { .. } => BatchSupport::Daemon,
                _ => BatchSupport::Cli,
            },

            // All remaining commands have a matching `BatchCmd` variant.
            Commands::Stats { .. }
            | Commands::Blame { .. }
            | Commands::Deps { .. }
            | Commands::Callers { .. }
            | Commands::Callees { .. }
            | Commands::Onboard { .. }
            | Commands::Diff { .. }
            | Commands::Drift { .. }
            | Commands::Explain { .. }
            | Commands::Similar { .. }
            | Commands::Impact { .. }
            | Commands::ImpactDiff { .. }
            | Commands::Review { .. }
            | Commands::Ci { .. }
            | Commands::Trace { .. }
            | Commands::TestMap { .. }
            | Commands::Context { .. }
            | Commands::Dead { .. }
            | Commands::Gather { .. }
            | Commands::Health { .. }
            | Commands::Stale { .. }
            | Commands::Read { .. }
            | Commands::Related { .. }
            | Commands::Where { .. }
            | Commands::Scout { .. }
            | Commands::Plan { .. }
            | Commands::Task { .. }
            // API-V1.29-6: forward to the existing `BatchCmd::Refresh` handler.
            // When no daemon is running the top-level dispatch in `dispatch.rs`
            // bails with "nothing to refresh" — classifying as Daemon here is
            // still correct because the handler lives on the batch side.
            | Commands::Refresh => BatchSupport::Daemon,

            // #946 typestate: Gc mutates the DB (prune_all + HNSW rebuild).
            // Daemon holds `Store<ReadOnly>`, so `prune_all` is literally
            // not callable there. Must go through the CLI path, which
            // opens `Store<ReadWrite>` via `CommandContext::open_readwrite`.
            Commands::Gc { .. } => BatchSupport::Cli,

            // #946 typestate: Suggest with --apply rewrites notes.toml and
            // calls `index_notes` → `replace_notes_for_file` (a write).
            // Classify on the `apply` flag: read-only dry-run is daemon-
            // dispatchable; the write variant must hit CLI.
            Commands::Suggest { ref args, .. } => {
                if args.apply {
                    BatchSupport::Cli
                } else {
                    BatchSupport::Daemon
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_finite_f32_normal_values() {
        assert!(validate_finite_f32(0.0, "test").is_ok());
        assert!(validate_finite_f32(1.0, "test").is_ok());
        assert!(validate_finite_f32(-1.0, "test").is_ok());
        assert!(validate_finite_f32(0.5, "test").is_ok());
    }

    #[test]
    fn validate_finite_f32_rejects_nan() {
        let result = validate_finite_f32(f32::NAN, "threshold");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("threshold"));
    }

    #[test]
    fn validate_finite_f32_rejects_infinity() {
        assert!(validate_finite_f32(f32::INFINITY, "test").is_err());
        assert!(validate_finite_f32(f32::NEG_INFINITY, "test").is_err());
    }

    #[test]
    fn validate_finite_f32_returns_value_on_success() {
        assert_eq!(validate_finite_f32(0.42, "x").unwrap(), 0.42);
    }

    /// EX-4: Verify clap default_value strings match the f32 constants.
    /// f32 can't be used in const assert, so we check at test time.
    #[test]
    fn clap_defaults_match_constants() {
        use crate::cli::config::{DEFAULT_LIMIT, DEFAULT_NAME_BOOST, DEFAULT_THRESHOLD};
        // default_value = "5"
        assert_eq!(DEFAULT_LIMIT, 5);
        // default_value = "0.3"
        assert!((DEFAULT_THRESHOLD - 0.3).abs() < f32::EPSILON);
        // default_value = "0.2"
        assert!((DEFAULT_NAME_BOOST - 0.2).abs() < f32::EPSILON);
    }

    // API-V1.25-7: parse_finite_f32 accepts finite values and rejects NaN/±Inf.
    #[test]
    fn parse_finite_f32_accepts_finite() {
        assert_eq!(parse_finite_f32("0.0").unwrap(), 0.0);
        assert_eq!(parse_finite_f32("0.5").unwrap(), 0.5);
        assert_eq!(parse_finite_f32("-1.0").unwrap(), -1.0);
        assert_eq!(parse_finite_f32("1e10").unwrap(), 1e10);
    }

    #[test]
    fn parse_finite_f32_rejects_nan() {
        let r = parse_finite_f32("NaN");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("NaN"));
    }

    #[test]
    fn parse_finite_f32_rejects_infinity() {
        assert!(parse_finite_f32("inf").is_err());
        assert!(parse_finite_f32("Infinity").is_err());
        assert!(parse_finite_f32("-inf").is_err());
    }

    #[test]
    fn parse_finite_f32_rejects_garbage() {
        assert!(parse_finite_f32("not a number").is_err());
        assert!(parse_finite_f32("").is_err());
    }

    // AC-V1.29-5: `parse_unit_f32` is `parse_finite_f32` bounded to [0.0, 1.0]
    // so flags like `--name-boost 1.5` surface a clap parse error instead of
    // silently degrading scoring.
    #[test]
    fn parse_unit_f32_accepts_unit_range() {
        assert_eq!(parse_unit_f32("0.0").unwrap(), 0.0);
        assert_eq!(parse_unit_f32("0.5").unwrap(), 0.5);
        assert_eq!(parse_unit_f32("1.0").unwrap(), 1.0);
    }

    #[test]
    fn parse_unit_f32_rejects_above_one() {
        let r = parse_unit_f32("1.5");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("[0.0, 1.0]"));
    }

    #[test]
    fn parse_unit_f32_rejects_negative() {
        let r = parse_unit_f32("-0.1");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("[0.0, 1.0]"));
    }

    #[test]
    fn parse_unit_f32_rejects_nan_and_infinity() {
        assert!(parse_unit_f32("NaN").is_err());
        assert!(parse_unit_f32("inf").is_err());
        assert!(parse_unit_f32("-inf").is_err());
    }

    #[test]
    fn search_name_boost_rejects_out_of_range() {
        use clap::Parser;
        // `cqs search --name-boost 1.5 foo` must return a clap parse error
        // now that `SearchArgs::name_boost` is bounded via `parse_unit_f32`.
        let r = Cli::try_parse_from(["cqs", "search", "--name-boost", "1.5", "foo"]);
        assert!(r.is_err());
    }

    // #947: spot-check the batch-support classifier. The exhaustive match in
    // `Commands::batch_support` does the real work — if a new variant is
    // added without classification, the whole crate fails to compile. These
    // tests pin the policy for a few sensitive variants so an accidental
    // flip (e.g. classifying Init as Daemon) shows up as a failing test.
    #[test]
    fn batch_support_lifecycle_commands_are_cli_only() {
        use clap::Parser;
        // Lifecycle commands must never forward to the daemon.
        let cli = Cli::try_parse_from(["cqs", "init"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Cli);

        let cli = Cli::try_parse_from(["cqs", "chat"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Cli);

        let cli = Cli::try_parse_from(["cqs", "index"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Cli);
    }

    #[test]
    fn batch_support_notes_mutations_are_cli_only() {
        use clap::Parser;
        // v1.25.0: notes add/update/remove reindex the filesystem; list mode
        // is daemon-safe. The classifier must distinguish them.
        let cli = Cli::try_parse_from(["cqs", "notes", "list"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Daemon);

        let cli = Cli::try_parse_from(["cqs", "notes", "add", "foo"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Cli);

        let cli = Cli::try_parse_from(["cqs", "notes", "remove", "foo"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Cli);
    }

    #[test]
    fn batch_support_search_commands_daemon_dispatchable() {
        use clap::Parser;
        // The flagship query surface must hit the daemon (3-19ms vs 2s CLI).
        let cli = Cli::try_parse_from(["cqs", "scout", "foo"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Daemon);

        let cli = Cli::try_parse_from(["cqs", "impact", "foo"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Daemon);

        let cli = Cli::try_parse_from(["cqs", "stale"]).unwrap();
        assert_eq!(cli.command.unwrap().batch_support(), BatchSupport::Daemon);
    }

    /// API-V1.22-3: top-level `--expand-parent` (bool, parent context) and
    /// `gather --expand <N>` (usize, graph depth) must parse independently
    /// without colliding. Pinning both spellings here so a future revert
    /// can't silently re-introduce the same-name-different-type ambiguity.
    #[test]
    fn cli_expand_parent_distinct_from_gather_expand() {
        use clap::Parser;
        // Top-level flag is `--expand-parent`, no value.
        let cli = Cli::try_parse_from(["cqs", "--expand-parent", "foo"]).unwrap();
        assert!(cli.expand_parent);
        assert_eq!(cli.query.as_deref(), Some("foo"));

        // The bare `--expand` alone is rejected (would have been ambiguous
        // with gather's `--expand <N>`).
        let bare = Cli::try_parse_from(["cqs", "--expand", "foo"]);
        assert!(
            bare.is_err(),
            "bare --expand on top-level Cli should be rejected — use --expand-parent"
        );

        // Gather still accepts `--expand <N>` (graph depth).
        let cli = Cli::try_parse_from(["cqs", "gather", "alarm", "--expand", "2"]).unwrap();
        match cli.command {
            Some(Commands::Gather { ref args, .. }) => assert_eq!(args.expand, 2),
            _ => panic!("expected Gather command"),
        }
    }

    /// API-V1.22-6: `cqs affected --stdin` accepts a captured diff piped in.
    /// Was a divergence from `review`/`ci`/`impact-diff`, all of which already
    /// take `--stdin`. Pinning the parse here so the flag stays valid.
    #[test]
    fn cli_affected_accepts_stdin_flag() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["cqs", "affected", "--stdin"]).unwrap();
        match cli.command {
            Some(Commands::Affected { stdin, .. }) => assert!(stdin),
            _ => panic!("expected Affected command"),
        }
    }
}
