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

    /// Enable RRF hybrid search (keyword + semantic fusion). Off by default — pure cosine is faster and scores higher on expanded eval.
    #[arg(long)]
    pub rrf: bool,

    /// Include documentation, markdown, and config chunks in search results. Default: code only.
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

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

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

    /// Get the resolved model config. Panics if called before dispatch resolves it.
    ///
    /// **Deprecated:** Use [`try_model_config`] instead — it returns `Result`
    /// rather than panicking on unresolved config.
    #[deprecated(note = "use try_model_config() which returns Result instead of panicking")]
    pub fn model_config(&self) -> &cqs::embedder::ModelConfig {
        self.resolved_model
            .as_ref()
            .expect("BUG: ModelConfig not resolved — dispatch() must call resolve_model() first")
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
        /// Type name (forward) or function name (with --reverse)
        name: String,
        /// Reverse: show types used by a function instead of type users
        #[arg(long)]
        reverse: bool,
        /// Query across all configured reference projects
        #[arg(long)]
        cross_project: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions that call a given function
    Callers {
        /// Function name to search for
        name: String,
        /// Query callers across all configured reference projects
        #[arg(long)]
        cross_project: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions called by a given function
    Callees {
        /// Function name to search for
        name: String,
        /// Query callees across all configured reference projects
        #[arg(long)]
        cross_project: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Guided codebase tour: entry point → call chain → types → tests
    Onboard {
        /// Concept or query to explore
        query: String,
        /// Callee expansion depth
        #[arg(short = 'd', long, default_value = "3")]
        depth: usize,
        #[command(flatten)]
        output: TextJsonArgs,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
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
        /// Reference name to compare from
        source: String,
        /// Reference name or "project" (default: project)
        target: Option<String>,
        /// Similarity threshold for "modified" (default: 0.95)
        ///
        /// `-t` here means "match threshold" — pairs above this are "unchanged",
        /// below are "modified". Different from search's `-t` (min similarity 0.3).
        /// See top-level threshold doc for rationale.
        #[arg(short = 't', long, default_value = "0.95", value_parser = parse_finite_f32)]
        threshold: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Detect semantic drift between a reference and the project
    Drift {
        /// Reference name to compare against
        reference: String,
        /// Similarity threshold (default: 0.95)
        ///
        /// See Diff's `-t` doc — same overload rationale applies.
        #[arg(short = 't', long, default_value = "0.95", value_parser = parse_finite_f32)]
        threshold: f32,
        /// Minimum drift to show (default: 0.0)
        #[arg(long, default_value = "0.0", value_parser = parse_finite_f32)]
        min_drift: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Maximum entries to show
        #[arg(short = 'n', long)]
        limit: Option<usize>,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Generate a function card (signature, callers, callees, similar)
    Explain {
        /// Function name or file:function
        name: String,
        #[command(flatten)]
        output: TextJsonArgs,
        /// Maximum token budget (includes source content within budget)
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
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
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Read diff from stdin instead of running git
        #[arg(long)]
        stdin: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Comprehensive diff review: impact + notes + risk scoring
    Review {
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Read diff from stdin instead of running git
        #[arg(long)]
        stdin: bool,
        #[command(flatten)]
        output: TextJsonArgs,
        /// Maximum token budget for output (truncates callers/tests lists)
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// CI pipeline analysis: impact + risk + dead code + gate
    Ci {
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Read diff from stdin instead of running git
        #[arg(long)]
        stdin: bool,
        #[command(flatten)]
        output: TextJsonArgs,
        /// Gate threshold: high, medium, off (default: high)
        #[arg(long, default_value = "high")]
        gate: GateThreshold,
        /// Maximum token budget for output
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
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
        /// Function name or file:function
        name: String,
        /// Max call chain depth to search
        #[arg(long, default_value = "5")]
        depth: usize,
        /// Search for tests across all configured reference projects
        #[arg(long)]
        cross_project: bool,
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
        output: TextJsonArgs,
        /// Show counts only, skip file list
        #[arg(long)]
        count_only: bool,
    },
    /// Auto-suggest notes from codebase patterns (dead code, untested hotspots)
    Suggest {
        #[command(flatten)]
        output: TextJsonArgs,
        /// Apply suggestions (add notes to docs/notes.toml)
        #[arg(long)]
        apply: bool,
    },
    /// Read a file with notes injected as comments
    Read {
        /// File path relative to project root
        path: String,
        /// Focus on a specific function (returns only that function + type deps)
        #[arg(long)]
        focus: Option<String>,
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
        /// Function name or file:function
        name: String,
        /// Max results per category
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Suggest where to add new code matching a description
    Where {
        /// Description of the code to add
        description: String,
        /// Max file suggestions
        #[arg(short = 'n', long, default_value = "3")]
        limit: usize,
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
        /// Task description to plan
        description: String,
        /// Max scout file groups
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        #[command(flatten)]
        output: TextJsonArgs,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// One-shot implementation context: scout + code + impact + placement + notes
    Task {
        /// Task description
        description: String,
        /// Max file groups to return
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        #[command(flatten)]
        output: TextJsonArgs,
        /// Maximum token budget (waterfall across sections)
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
        /// Compact output (~200 tokens): files, at-risk functions, test coverage
        #[arg(long)]
        brief: bool,
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
        /// Preview conversions without writing files
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
        /// Maximum commits to process per repo (0 = unlimited)
        #[arg(long, default_value = "0")]
        max_commits: usize,
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
        /// Output JSONL file path
        #[arg(long)]
        output: String,
        /// Max pairs to extract
        #[arg(long)]
        limit: Option<usize>,
        /// Filter by language (e.g., "Rust", "Python")
        #[arg(long)]
        language: Option<String>,
        /// Add contrastive prefixes from call graph callees
        #[arg(long)]
        contrastive: bool,
    },
    /// Manage global embedding cache (stats, clear, prune)
    Cache {
        #[command(subcommand)]
        subcmd: CacheCommand,
    },
}

// Re-export the subcommand types used in Commands variants
pub(super) use super::commands::{CacheCommand, NotesCommand, ProjectCommand, RefCommand};

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
}
