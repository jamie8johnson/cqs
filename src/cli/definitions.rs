//! Clap argument definitions: CLI struct, subcommand enum, output types.

use clap::{Parser, Subcommand};

use super::args;

/// Output format for commands that support text/json/mermaid
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Mermaid,
}

/// Parse an `OutputFormat` that only allows text or json (rejects mermaid at parse time).
///
/// Used by `review` and `ci` commands which accept `--format` but don't support mermaid output.
/// Catches the error at argument parsing rather than failing at runtime.
fn parse_text_or_json_format(s: &str) -> std::result::Result<OutputFormat, String> {
    match s.to_ascii_lowercase().as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        "mermaid" => {
            Err("mermaid output is not supported for this command — use text or json".into())
        }
        other => Err(format!("invalid format '{other}' — expected text or json")),
    }
}

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

/// AD-49: Output format arguments for commands that only support text/json (no mermaid).
#[derive(Clone, Debug, clap::Args)]
pub struct TextJsonArgs {
    /// Output format: text, json (use --json as shorthand for --format json; mermaid not supported)
    #[arg(long, default_value = "text", value_parser = parse_text_or_json_format)]
    pub format: OutputFormat,
    /// Shorthand for --format json
    #[arg(long, conflicts_with = "format")]
    pub json: bool,
}

impl TextJsonArgs {
    /// Resolve the effective format (--json overrides --format).
    pub fn effective_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format.clone()
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
    #[arg(short = 't', long, default_value = "0.3")]
    pub threshold: f32,

    /// Weight for name matching in hybrid search (0.0-1.0)
    #[arg(long, default_value = "0.2")]
    pub name_boost: f32,

    /// Filter by language
    #[arg(short = 'l', long)]
    pub lang: Option<String>,

    /// Filter by chunk type (function, method, class, struct, enum, trait, interface, constant, section, property, delegate, event, module, macro, object, typealias)
    #[arg(long)]
    pub chunk_type: Option<Vec<String>>,

    /// Filter by path pattern (glob)
    #[arg(short = 'p', long)]
    pub path: Option<String>,

    /// Filter by structural pattern (builder, error_swallow, async, mutex, unsafe, recursion)
    #[arg(long)]
    pub pattern: Option<String>,

    /// Definition search: find by name only, skip embedding (faster)
    #[arg(long)]
    pub name_only: bool,

    /// Pure semantic similarity, disable RRF hybrid search
    #[arg(long)]
    pub semantic_only: bool,

    /// Re-rank results with cross-encoder (slower, more accurate)
    #[arg(long)]
    pub rerank: bool,

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
    /// Get the resolved model config. Panics if called before dispatch resolves it.
    pub fn model_config(&self) -> &cqs::embedder::ModelConfig {
        self.resolved_model
            .as_ref()
            .expect("ModelConfig not resolved — call resolve_model() first")
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Watch for changes and reindex
    Watch {
        /// Debounce interval in milliseconds
        #[arg(long, default_value = "500")]
        debounce: u64,
        /// Index files ignored by .gitignore
        #[arg(long)]
        no_ignore: bool,
        /// Use polling instead of inotify (reliable on WSL /mnt/ paths)
        #[arg(long)]
        poll: bool,
    },
    /// What functions, callers, and tests are affected by current diff
    Affected {
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Batch mode: read commands from stdin, output JSONL
    Batch,
    /// Semantic git blame: who changed a function, when, and why
    Blame {
        /// Function name or file:function
        name: String,
        /// Max commits to show
        #[arg(short = 'd', long, default_value = "10")]
        depth: usize,
        /// Also show callers of the function
        #[arg(long)]
        callers: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions that call a given function
    Callers {
        /// Function name to search for
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions called by a given function
    Callees {
        /// Function name to search for
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Guided codebase tour: entry point → call chain → types → tests
    Onboard {
        /// Concept or query to explore
        query: String,
        /// Callee expansion depth
        #[arg(short = 'd', long, default_value = "3")]
        depth: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        #[arg(short = 't', long, default_value = "0.95")]
        threshold: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Detect semantic drift between a reference and the project
    Drift {
        /// Reference name to compare against
        reference: String,
        /// Similarity threshold (default: 0.95)
        ///
        /// See Diff's `-t` doc — same overload rationale applies.
        #[arg(short = 't', long, default_value = "0.95")]
        threshold: f32,
        /// Minimum drift to show (default: 0.0)
        #[arg(long, default_value = "0.0")]
        min_drift: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Maximum entries to show
        #[arg(short = 'n', long)]
        limit: Option<usize>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Generate a function card (signature, callers, callees, similar)
    Explain {
        /// Function name or file:function
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum token budget (includes source content within budget)
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Find code similar to a given function
    Similar {
        /// Function name or file:function (e.g., "search_filtered" or "src/search.rs:search_filtered")
        target: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Min similarity threshold
        #[arg(short = 't', long, default_value = "0.3")]
        threshold: f32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Source function name or file:function
        source: String,
        /// Target function name or file:function
        target: String,
        /// Max search depth (1-50)
        #[arg(long, default_value = "10", value_parser = clap::value_parser!(u16).range(1..=50))]
        max_depth: u16,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// What do I need to know to work on this file
    Context {
        #[command(flatten)]
        args: args::ContextArgs,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions with no callers (dead code detection)
    Dead {
        #[command(flatten)]
        args: args::DeadArgs,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Gather minimal code context to answer a question
    Gather {
        #[command(flatten)]
        args: args::GatherArgs,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Manage cross-project search registry
    Project {
        #[command(subcommand)]
        subcmd: ProjectCommand,
    },
    /// Remove stale chunks and rebuild index
    Gc {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Codebase quality snapshot — dead code, staleness, hotspots, coverage
    Health {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Toggle audit mode (exclude notes from search/read)
    #[command(name = "audit-mode")]
    AuditMode {
        /// State: on or off (omit to query current state)
        state: Option<AuditModeState>,
        /// Expiry duration (e.g., "30m", "1h", "2h30m")
        #[arg(long, default_value = "30m")]
        expires: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Check index freshness — list stale and missing files
    Stale {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show counts only, skip file list
        #[arg(long)]
        count_only: bool,
    },
    /// Auto-suggest notes from codebase patterns (dead code, untested hotspots)
    Suggest {
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions related by shared callers, callees, or types
    Related {
        /// Function name or file:function
        name: String,
        /// Max results per category
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Suggest where to add new code matching a description
    Where {
        /// Description of the code to add
        description: String,
        /// Max file suggestions
        #[arg(short = 'n', long, default_value = "3")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Pre-investigation dashboard: search, group, count callers/tests, check staleness
    Scout {
        #[command(flatten)]
        args: args::ScoutArgs,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Task planning with template classification: classify + scout + checklist
    Plan {
        /// Task description to plan
        description: String,
        /// Max scout file groups
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Maximum token budget (waterfall across sections)
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
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
}

// Re-export the subcommand types used in Commands variants
pub(super) use super::commands::{NotesCommand, ProjectCommand, RefCommand};

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
}
