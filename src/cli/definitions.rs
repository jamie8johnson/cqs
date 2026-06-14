//! Clap argument definitions: CLI struct, subcommand enum, output types.

use clap::{Parser, Subcommand};

use super::args;

// Verify clap default_value strings match the actual constants.
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

/// Common output format arguments shared across commands that support text/json/mermaid.
///
/// **`--json` vs `--format`:** the two flags are
/// intentionally mutually exclusive (`conflicts_with = "format"`), not an
/// accidental shadow. `--json` is the universal spelling every command
/// accepts (via `TextJsonArgs` elsewhere) and every agent/test invokes;
/// `--format text|json|mermaid` is the multi-format escape hatch the handful
/// of commands with a third rendering (impact / trace → mermaid) expose. A
/// caller picks one axis: the `--json` shorthand, or the explicit `--format`
/// when they need `mermaid`. Combining them (`--json --format mermaid`) is a
/// parse-time error rather than a silent precedence surprise — pinned by
/// `test_{impact,trace}_json_conflicts_with_format`. Keeping `--json` (vs the
/// audit's "drop it" option) is deliberate: dropping it would break every
/// `cqs impact <fn> --json` agent call and force `--format json` everywhere.
#[derive(Clone, Debug, clap::Args)]
pub struct OutputArgs {
    /// Output format: text, json, mermaid (use --json as shorthand for --format json)
    #[arg(long, default_value = "text")]
    pub format: OutputFormat,
    /// Shorthand for --format json. Mutually exclusive with --format (pick one).
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

/// Output format for commands that only support text or JSON. Exposes `--json`
/// only; commands that genuinely support multiple output formats (e.g.
/// `--format mermaid`) use [`OutputArgs`] instead.
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
#[derive(Clone, Debug, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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
/// Used as `value_parser` on every f32 CLI flag to reject `NaN` / `Infinity`
/// / `-Infinity` at argument-parse time, before the value can flow into
/// scoring, thresholds, or filter construction. The signature matches clap's
/// expectation (`fn(&str) -> Result<T, String>`), unlike `validate_finite_f32`
/// which runs on an already-parsed f32.
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

/// Finite-f32 bounded to the `[0.0, 1.0]` unit interval. Used as
/// `value_parser` for CLI flags that encode a weight or blending fraction
/// (e.g., `--name-boost`, `cqs ref add --weight`) where out-of-range values
/// would silently corrupt scoring instead of surfacing a clap error. Rejects
/// NaN / ±Inf via the underlying `parse_finite_f32`, then fences the range.
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
    ///
    /// Rejects `--limit 0` at parse time — search with limit=0 is
    /// semantically meaningless, so it fails fast at the boundary.
    #[arg(short = 'n', long, default_value = "5", value_parser = parse_nonzero_usize)]
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
    /// Bounded parser — see `SearchArgs::name_boost`.
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

    /// Reranker mode: `none|onnx`.
    ///
    /// Mirrors `cqs eval --reranker`. `none` is the default; `onnx` runs the
    /// cross-encoder configured by `[reranker]` / `CQS_RERANKER_MODEL`.
    #[arg(long = "reranker", value_enum)]
    pub reranker: Option<args::RerankerMode>,

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
    /// Named `--expand-parent` to disambiguate from `gather --expand <N>`
    /// (graph depth, `usize`) — same flag name with two incompatible types
    /// would bite agents that batch both commands.
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
    ///
    /// `global = true` so `-q`/`--quiet` works after a subcommand
    /// (`cqs index -q`), matching how the skill docs already assumed it
    /// behaved. No subcommand defines its own `-q` short flag, so
    /// globalizing introduces no collision.
    #[arg(short, long, global = true)]
    pub quiet: bool,

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

    /// Embedding model: embeddinggemma-300m (default), bge-large, e5-base, or custom.
    ///
    /// Honored across all commands: `cqs <q> --model X` selects the query embedder,
    /// `cqs index --model X` stamps X into a fresh index (or, on incremental,
    /// requires `--force` to switch off the previously-stamped model).
    #[arg(long)]
    pub model: Option<String>,

    /// Named slot to use (overrides `CQS_SLOT` env and `.cqs/active_slot`).
    ///
    /// Slots are project-scoped, side-by-side full indexes living under
    /// `.cqs/slots/<name>/`. Default behaviour is to read the active slot
    /// from `.cqs/active_slot` (falls back to `default`). Spec:
    /// `docs/plans/2026-04-24-embeddings-cache-and-slots.md`.
    ///
    /// **Ignored (and rejected at runtime) on `cqs slot` and `cqs cache`
    /// subcommands** — those manage slots and the cache themselves, so
    /// scoping them to a slot is incoherent. Use the explicit
    /// `<subcommand> <name>` argument instead (e.g. `cqs slot remove
    /// <name>`, `cqs cache stats <name>`). clap-derive can't suppress a
    /// `global = true` arg per subcommand, so the runtime bails in
    /// `cli/commands/infra/slot.rs` and `cli/commands/infra/cache_cmd.rs`.
    #[arg(long, global = true)]
    pub slot: Option<String>,

    /// Show debug info (sets RUST_LOG=debug)
    #[arg(short, long)]
    pub verbose: bool,

    /// Acknowledge writing to a parent index from inside a git worktree.
    ///
    /// From a worktree under a parent Cargo workspace (e.g.
    /// `.claude/worktrees/<agent>/`), cqs's project-root discovery walks
    /// up past the worktree's own `.git` to the parent's index — deliberate
    /// for *reads*. A WRITE command
    /// (`init`/`index`/`notes add`/`cache prune`/`slot create`/…) that
    /// resolves to a parent index outside the current worktree would
    /// silently mutate it, defeating worktree isolation. Such a write is
    /// refused unless this flag — or `CQS_PARENT_INDEX_OK=1` —
    /// acknowledges it. Reads are never gated.
    ///
    /// `global = true` so it can sit after the subcommand
    /// (`cqs index --parent-index`).
    #[arg(long, global = true)]
    pub parent_index: bool,

    /// Resolved model config (set by dispatch, not CLI).
    ///
    /// `pub(super)` because the field is `#[arg(skip)]` — only `cli::dispatch`
    /// (writer) and `cli::definitions::Cli::try_model_config` (reader) touch
    /// it. External readers should go through `try_model_config()`.
    #[arg(skip)]
    pub(super) resolved_model: Option<cqs::embedder::ModelConfig>,
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

    /// Effective reranker mode. Returns `RerankerMode::None` when no flag is set.
    pub(crate) fn rerank_mode(&self) -> args::RerankerMode {
        self.reranker.unwrap_or(args::RerankerMode::None)
    }

    /// `true` if any reranker stage is selected (Onnx or Llm).
    pub(crate) fn rerank_active(&self) -> bool {
        !matches!(self.rerank_mode(), args::RerankerMode::None)
    }
}

#[derive(Subcommand, cqs_macros::CqsCommands)]
pub(super) enum Commands {
    /// Download model and create .cqs/
    #[cqs_cmd(group = "a", batch = "cli")]
    Init {
        /// Emit a structured JSON envelope summarizing the init, for parity
        /// with the rest of the CLI's `--json` contract so JSON-driven agents
        /// can confirm the directory and model that got created.
        ///
        /// Flattens shared `TextJsonArgs` so a future change to the flag
        /// (NDJSON, `--pretty`, `--format`) is a one-file edit.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// One-line-per-function summary for a file
    #[cqs_cmd(group = "b", batch = "cli")]
    Brief {
        /// File path (as stored in index, e.g. src/lib.rs)
        path: String,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Check model, index, hardware
    #[cqs_cmd(group = "a", batch = "cli")]
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
        ///
        /// Flattens shared `TextJsonArgs` — see `Init` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Index current project
    #[cqs_cmd(group = "a", batch = "cli")]
    Index {
        #[command(flatten)]
        args: args::IndexArgs,
    },
    /// Show index statistics
    #[cqs_cmd(group = "b", batch = "daemon")]
    Stats {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Watch for changes and reindex
    #[cqs_cmd(group = "a", batch = "cli")]
    Watch {
        /// Quiet gap in milliseconds (idle-flush debounce): pending
        /// changes reindex after this much event silence, so a bulk
        /// burst (e.g. git checkout) coalesces into one cycle fired
        /// just after the burst ends. Default 500ms suits inotify on
        /// native Linux; WSL DrvFS (/mnt/) and --poll mode auto-bump
        /// to 1500ms because NTFS mtime resolution is 1s. Override
        /// here or via CQS_WATCH_DEBOUNCE_MS (takes precedence over
        /// the flag). A never-quiet event stream still flushes within
        /// CQS_WATCH_MAX_DEBOUNCE_MS (default 6x the quiet gap).
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
    ///
    /// `batch = "cli"`: the batch dispatcher has no `affected` subcommand, so
    /// a daemon-marked classification made `cqs affected` error daemon-up
    /// while working daemon-down (the exhaustiveness test in
    /// `cli::batch::commands` now pins the link). The command also reads the
    /// diff from `--stdin`/git in the *CLI* process, which a daemon dispatch
    /// cannot reproduce. Flip back to `daemon` only together with a
    /// `BatchCmd::Affected` handler.
    #[cqs_cmd(group = "b", batch = "cli")]
    Affected {
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Read diff from stdin instead of running git.
        ///
        /// Lets agents pipe a captured diff
        /// (`git diff main | cqs affected --stdin --json`) without re-shelling
        /// git, in line with `review`, `ci`, and `impact-diff`.
        #[arg(long)]
        stdin: bool,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Batch mode: read commands from stdin, output JSONL
    #[cqs_cmd(group = "a", batch = "cli")]
    Batch,
    /// Semantic git blame: who changed a function, when, and why
    #[cqs_cmd(group = "b", batch = "daemon")]
    Blame {
        #[command(flatten)]
        args: args::BlameArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Interactive REPL for cqs commands
    #[cqs_cmd(group = "a", batch = "cli")]
    Chat,
    /// Generate shell completions
    #[cqs_cmd(group = "a", batch = "cli")]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Show type dependencies: who uses a type, or what types a function uses
    #[cqs_cmd(group = "b", batch = "daemon")]
    Deps {
        #[command(flatten)]
        args: args::DepsArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions that call a given function
    #[cqs_cmd(group = "b", batch = "daemon")]
    Callers {
        #[command(flatten)]
        args: args::CallersArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions called by a given function
    #[cqs_cmd(group = "b", batch = "daemon")]
    Callees {
        #[command(flatten)]
        args: args::CallersArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Guided codebase tour: entry point → call chain → types → tests
    #[cqs_cmd(group = "b", batch = "daemon")]
    Onboard {
        #[command(flatten)]
        args: args::OnboardArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Brute-force nearest neighbors for a function by cosine similarity
    #[cqs_cmd(group = "b", batch = "cli")]
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
    #[cqs_cmd(group = "a", batch = "runtime")]
    Notes {
        #[command(subcommand)]
        subcmd: NotesCommand,
    },
    /// Manage reference indexes for multi-index search
    #[cqs_cmd(group = "a", batch = "cli")]
    Ref {
        #[command(subcommand)]
        subcmd: RefCommand,
    },
    /// Semantic diff between indexed snapshots
    #[cqs_cmd(group = "a", batch = "cli")]
    Diff {
        #[command(flatten)]
        args: args::DiffArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Detect semantic drift between a reference and the project
    #[cqs_cmd(group = "a", batch = "cli")]
    Drift {
        #[command(flatten)]
        args: args::DriftArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Generate a function card (signature, callers, callees, similar)
    #[cqs_cmd(group = "b", batch = "daemon")]
    Explain {
        #[command(flatten)]
        args: args::ExplainArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find code similar to a given function
    #[cqs_cmd(group = "b", batch = "daemon")]
    Similar {
        #[command(flatten)]
        args: args::SimilarArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Impact analysis: what breaks if you change a function
    #[cqs_cmd(group = "b", batch = "daemon")]
    Impact {
        #[command(flatten)]
        args: args::ImpactArgs,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Impact analysis from a git diff — what callers and tests are affected
    #[command(name = "impact-diff")]
    #[cqs_cmd(group = "b", batch = "daemon")]
    ImpactDiff {
        #[command(flatten)]
        args: args::ImpactDiffArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Comprehensive diff review: impact + notes + risk scoring
    #[cqs_cmd(group = "b", batch = "daemon")]
    Review {
        #[command(flatten)]
        args: args::ReviewArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// CI pipeline analysis: impact + risk + dead code + gate
    #[cqs_cmd(group = "b", batch = "daemon")]
    Ci {
        #[command(flatten)]
        args: args::CiArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Trace call chain between two functions
    #[cqs_cmd(group = "b", batch = "daemon")]
    Trace {
        #[command(flatten)]
        args: args::TraceArgs,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Find tests that exercise a function
    #[cqs_cmd(group = "b", batch = "daemon")]
    TestMap {
        #[command(flatten)]
        args: args::TestMapArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// What do I need to know to work on this file
    #[cqs_cmd(group = "b", batch = "daemon")]
    Context {
        #[command(flatten)]
        args: args::ContextArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions with no callers (dead code detection)
    #[cqs_cmd(group = "b", batch = "daemon")]
    Dead {
        #[command(flatten)]
        args: args::DeadArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Gather minimal code context to answer a question
    #[cqs_cmd(group = "b", batch = "daemon")]
    Gather {
        #[command(flatten)]
        args: args::GatherArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Manage cross-project search registry
    #[cqs_cmd(group = "a", batch = "cli")]
    Project {
        #[command(subcommand)]
        subcmd: ProjectCommand,
    },
    /// Remove stale chunks and rebuild index
    #[cqs_cmd(group = "a", batch = "cli")]
    Gc {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Codebase quality snapshot — dead code, staleness, hotspots, coverage
    #[cqs_cmd(group = "b", batch = "daemon")]
    Health {
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Toggle audit mode (exclude notes from search/read)
    #[command(name = "audit-mode")]
    #[cqs_cmd(group = "a", batch = "cli")]
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
    #[cqs_cmd(group = "a", batch = "cli")]
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
    #[cqs_cmd(group = "b", batch = "daemon")]
    Stale {
        #[command(flatten)]
        args: args::StaleArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Auto-suggest notes from codebase patterns (dead code, untested hotspots)
    #[cqs_cmd(group = "b", batch = "runtime")]
    Suggest {
        #[command(flatten)]
        args: args::SuggestArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Read a file with notes injected as comments
    #[cqs_cmd(group = "b", batch = "daemon")]
    Read {
        #[command(flatten)]
        args: args::ReadArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Reconstruct source file from index (works without source on disk)
    #[cqs_cmd(group = "b", batch = "cli")]
    Reconstruct {
        /// File path (as indexed)
        path: String,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Find functions related by shared callers, callees, or types
    #[cqs_cmd(group = "b", batch = "daemon")]
    Related {
        #[command(flatten)]
        args: args::RelatedArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Suggest where to add new code matching a description
    #[cqs_cmd(group = "b", batch = "daemon")]
    Where {
        #[command(flatten)]
        args: args::WhereArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Pre-investigation dashboard: search, group, count callers/tests, check staleness
    #[cqs_cmd(group = "b", batch = "daemon")]
    Scout {
        #[command(flatten)]
        args: args::ScoutArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Task planning with template classification: classify + scout + checklist
    #[cqs_cmd(group = "b", batch = "daemon")]
    Plan {
        #[command(flatten)]
        args: args::PlanArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// One-shot implementation context: scout + code + impact + placement + notes
    #[cqs_cmd(group = "b", batch = "daemon")]
    Task {
        #[command(flatten)]
        args: args::TaskArgs,
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Convert documents (PDF, HTML, CHM) to Markdown
    #[cfg(feature = "convert")]
    #[cqs_cmd(group = "a", batch = "cli")]
    Convert {
        /// File or directory to convert
        path: String,
        /// Output directory for .md files [default: same as input]
        ///
        /// Named `output_dir` to avoid colliding with the flattened
        /// `TextJsonArgs.output` envelope. The user-facing flag
        /// `-o`/`--output` is preserved via `long = "output"`.
        #[arg(short = 'o', long = "output")]
        output_dir: Option<String>,
        /// Overwrite existing .md files
        #[arg(long)]
        overwrite: bool,
        /// Preview conversions (default writes the .md files).
        ///
        /// Per the CONTRIBUTING "Dry-Run vs Apply" rule, side-effect commands
        /// (`index`, `convert`) default to mutating; analyser commands
        /// (`doctor`, `suggest`) default to read-only and require
        /// `--fix`/`--apply` to mutate.
        #[arg(long)]
        dry_run: bool,
        /// Cleaning rule tags (comma-separated, e.g. "aveva,generic") [default: all]
        #[arg(long)]
        clean_tags: Option<String>,
        /// Emit a structured JSON envelope summarizing conversions.
        /// Suppresses the per-file text rendering in favor of a
        /// `{converted: [...], skipped: [...], took_ms}` summary.
        ///
        /// Flattens shared `TextJsonArgs` — see `Init` above. The command's
        /// destination directory is `output_dir` to avoid colliding with the
        /// flattened envelope.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Export a HuggingFace model to ONNX format for use with cqs
    #[cqs_cmd(group = "a", batch = "cli")]
    ExportModel {
        /// HuggingFace model repo ID
        #[arg(long)]
        repo: String,
        /// Output directory
        #[arg(long, default_value = ".")]
        output: std::path::PathBuf,
        /// Embedding dimension override (auto-detected from config.json if omitted)
        ///
        /// `usize` aligns with `ModelConfig.dim`, `EmbeddingConfig.dim`,
        /// `VectorIndex::dim()` etc. — every other dim field in the codebase.
        #[arg(long)]
        dim: Option<usize>,
    },
    /// Generate training data for fine-tuning from git history
    #[cqs_cmd(group = "a", batch = "cli")]
    TrainData {
        /// Paths to git repositories to process
        #[arg(long, required = true, num_args = 1..)]
        repos: Vec<std::path::PathBuf>,
        /// Output JSONL file path
        #[arg(long)]
        output: std::path::PathBuf,
        /// Maximum commits to process per repo (omit for unlimited).
        ///
        /// `Option<usize>` (`None` = unlimited), matching `TrainPairs::limit`.
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
    #[cqs_cmd(group = "b", batch = "cli")]
    TrainPairs {
        /// Output JSONL file path.
        ///
        /// `PathBuf` so the same file-path concept uses one type across both
        /// training commands.
        #[arg(long)]
        output: std::path::PathBuf,
        /// Max pairs to extract (omit for unlimited)
        ///
        /// `-n` short flag for parity with other result-cap knobs across the
        /// CLI surface.
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
    #[cqs_cmd(group = "a", batch = "cli")]
    Cache {
        #[command(subcommand)]
        subcmd: CacheCommand,
    },
    /// Manage named slots — side-by-side full indexes under `.cqs/slots/<name>/`
    #[cqs_cmd(group = "a", batch = "cli")]
    Slot {
        #[command(subcommand)]
        subcmd: SlotCommand,
    },
    /// Daemon healthcheck — show daemon model, uptime, and counters
    ///
    /// Connects to the running daemon socket and prints its current state.
    /// Exits 1 if no daemon is running. Use `--json` for machine-readable
    /// output. Uses [`cqs::daemon_translate::daemon_ping`] so other tools
    /// (e.g. `cqs doctor --verbose`) can pull the same data.
    #[cqs_cmd(group = "a", batch = "cli")]
    Ping {
        /// Output as JSON.
        ///
        /// Flattens shared `TextJsonArgs` — see `Init` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Watch-mode freshness — show whether the index is caught up
    ///
    /// Connects to the running `cqs watch --serve` daemon and reports the
    /// latest [`cqs::watch_status::WatchSnapshot`]. Useful for
    /// agent loops that want to gate work on freshness (eval runners,
    /// pre-query checks). Exits 1 if no daemon is running.
    ///
    /// `--watch-fresh` is the canonical freshness flag; `--watch` adds
    /// the daemon's operational stats (queue depth, in-flight clients,
    /// dropped events, last-reindex latency, last error, per-slot
    /// freshness) — the journalctl-grep replacement. The two
    /// flags compose: both hit the same snapshot query.
    ///
    /// `--wait` polls until the snapshot reports `state == fresh` or the
    /// `--wait-secs` budget expires. Polling happens client-side so the
    /// daemon thread is never pinned by a long wait.
    #[cqs_cmd(group = "a", batch = "cli")]
    Status {
        /// Report watch-mode freshness.
        #[arg(long)]
        watch_fresh: bool,
        /// Report daemon operational stats: in-flight clients, queue
        /// depth, dropped events, last-reindex latency, reconcile
        /// state, last error, per-slot freshness. Composes with
        /// `--watch-fresh` (same snapshot, extra output block).
        #[arg(long)]
        watch: bool,
        /// Block until the snapshot reports `state == fresh` (or until the
        /// `--wait-secs` budget expires). Requires `--watch-fresh`.
        #[arg(long)]
        wait: bool,
        /// Maximum seconds `--wait` polls before giving up. Capped at 600
        /// so a runaway agent loop can't pin the daemon socket forever.
        ///
        /// Note: `cqs eval --require-fresh-secs` has the same semantics;
        /// default differs by use case (eval default = 600, status = 30).
        ///
        /// `--require-fresh-secs` is a visible alias so an agent that learned
        /// the eval-side spelling can use it on `cqs status` too. Both
        /// spellings parse to the same field; prefer `--wait-secs` in CI
        /// scripts (shorter).
        #[arg(long, visible_alias = "require-fresh-secs", default_value_t = 30)]
        wait_secs: u64,
        /// Output as JSON. Without this, prints a one-line human summary.
        ///
        /// Flattens shared `TextJsonArgs` — see `Init` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Invalidate daemon caches and re-open the Store
    ///
    /// Exposes the `BatchCmd::Refresh` handler at the top-level CLI so agents
    /// can trigger `ctx.invalidate()` without the `cqs batch` JSON dance.
    /// No-op when no daemon is running — a fresh CLI process has no caches to
    /// invalidate.
    #[command(visible_alias = "invalidate")]
    #[cqs_cmd(group = "a", batch = "daemon")]
    Refresh {
        /// Emit a structured JSON envelope summarizing the refresh outcome,
        /// for parity with the rest of the CLI's `--json` contract so
        /// JSON-driven agents can detect whether the daemon was running and
        /// got refreshed.
        ///
        /// Flattens shared `TextJsonArgs` — see `Init` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// First-class eval harness: run query set against current index, print R@K
    #[cqs_cmd(group = "b", batch = "cli")]
    Eval {
        #[command(flatten)]
        args: super::commands::EvalCmdArgs,
    },
    /// Manage cqs git hooks: install/uninstall/fire/status.
    ///
    /// Hooks live in `.git/hooks/post-{checkout,merge,rewrite}` and post a
    /// `reconcile` socket message to the running `cqs watch --serve` daemon
    /// after every git operation that moves the working tree. When the
    /// daemon isn't running, the hook touches `.cqs/.dirty` as a fallback;
    /// the daemon promotes that marker into a one-shot reconcile on next
    /// start.
    #[cqs_cmd(group = "a", batch = "cli")]
    Hook {
        #[command(subcommand)]
        subcmd: HookCommand,
    },
    /// Show / list / swap the embedding model recorded in the index
    #[cqs_cmd(group = "a", batch = "cli")]
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
    #[cqs_cmd(group = "a", batch = "cli")]
    Serve {
        /// TCP port to bind. Default 8080.
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Bind address. Default 127.0.0.1.
        ///
        /// Non-loopback binds are gated by the per-launch auth token —
        /// every request without the token is 401'd. With `--no-auth`, this
        /// exposes an un-authenticated server beyond localhost (loud-warning
        /// banner on boot).
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Open the system browser on start.
        ///
        /// The launched URL includes the auth token as a query
        /// parameter; the browser hands it off to a cookie on the
        /// post-auth redirect, so reload + bookmark both work.
        #[arg(long)]
        open: bool,
        /// Disable the per-launch auth token.
        ///
        /// Default behavior generates a fresh 256-bit token per
        /// launch, prints the paste-ready URL on stdout, and rejects
        /// every request without it. This flag opts out for
        /// scripted-automation back-compat — pair with `--bind
        /// 127.0.0.1` to avoid exposing an un-auth'd server beyond
        /// localhost.
        #[arg(long)]
        no_auth: bool,
    },
}

// Re-export the subcommand types used in Commands variants
pub(super) use super::commands::{
    CacheCommand, HookCommand, ModelCommand, NotesCommand, ProjectCommand, RefCommand, SlotCommand,
};

impl Commands {
    /// The effective output format this invocation requests, for the
    /// daemon-forward text-vs-JSON gate.
    ///
    /// Returns `Some(format)` for every variant carrying an `output`
    /// (`OutputArgs` / `TextJsonArgs`) flag group, resolving `--json` over
    /// `--format`. Returns `None` for variants with no output-format flag
    /// (lifecycle / mutation / subcommand-dispatch variants), which are all
    /// CLI-only and never reach the daemon-forward gate.
    ///
    /// The daemon wire shape is structured JSON; the CLI surface renders
    /// prose for text mode through each command's own renderer. The gate
    /// uses this to keep text-mode invocations on the CLI path so output is
    /// surface-independent — a daemon never serves the JSON payload where the
    /// CLI would have rendered text.
    ///
    /// Every daemon-dispatchable variant is pinned to return `Some` by
    /// `daemon_dispatchable_variants_report_an_output_format` in the dispatch
    /// tests, so a new daemon command without an arm here fails at test time
    /// rather than leaking a JSON payload daemon-up.
    pub(crate) fn effective_output_format(&self) -> Option<OutputFormat> {
        match self {
            Commands::Stats { output }
            | Commands::Health { output }
            | Commands::Refresh { output } => Some(output.effective_format()),
            Commands::Blame { output, .. }
            | Commands::Deps { output, .. }
            | Commands::Callers { output, .. }
            | Commands::Callees { output, .. }
            | Commands::Onboard { output, .. }
            | Commands::Explain { output, .. }
            | Commands::Similar { output, .. }
            | Commands::ImpactDiff { output, .. }
            | Commands::Review { output, .. }
            | Commands::Ci { output, .. }
            | Commands::TestMap { output, .. }
            | Commands::Context { output, .. }
            | Commands::Dead { output, .. }
            | Commands::Gather { output, .. }
            | Commands::Stale { output, .. }
            | Commands::Suggest { output, .. }
            | Commands::Read { output, .. }
            | Commands::Related { output, .. }
            | Commands::Where { output, .. }
            | Commands::Scout { output, .. }
            | Commands::Plan { output, .. }
            | Commands::Task { output, .. } => Some(output.effective_format()),
            Commands::Impact { output, .. } | Commands::Trace { output, .. } => {
                Some(output.effective_format())
            }
            // `notes` carries its `--json` on the inner subcommand. Only
            // `notes list` is daemon-dispatchable; delegating keeps its
            // text-mode invocation on the CLI path (the JSON payload would
            // otherwise leak) while `notes list --json` forwards.
            Commands::Notes { subcmd } => Some(subcmd.effective_output_format()),
            _ => None,
        }
    }

    /// `true` when this invocation reads its diff from `--stdin`.
    ///
    /// `review` / `ci` / `impact-diff` are daemon-marked, but the daemon
    /// reads the diff itself (it runs git in the *server* process and has no
    /// client stdin on the wire). Forwarding a `--stdin` invocation would
    /// silently drop the piped diff and analyze the daemon's working tree
    /// instead — the wrong diff, with no error. The daemon-forward gate uses
    /// this to keep `--stdin` invocations on the CLI path, the same bypass
    /// shape text-mode invocations use. (`affected` also carries `--stdin` but
    /// is already classified CLI-only, so it never reaches the gate.)
    pub(crate) fn reads_diff_from_stdin(&self) -> bool {
        match self {
            Commands::Review { args, .. } => args.stdin,
            Commands::Ci { args, .. } => args.stdin,
            Commands::ImpactDiff { args, .. } => args.stdin,
            _ => false,
        }
    }

    /// The worktree-overlay tri-state `(overlay, no_overlay)` for the seed-
    /// overlaid graph-adjacent commands (`scout` / `gather` / `task`,
    /// Part A), or `None` for every other command. The `search` overlay flags
    /// live on the top-level `Cli` (the default `cqs "query"` form), so the
    /// dispatch-forward path reads those directly; this accessor covers only the
    /// subcommand-bound seed overlays. Returns the raw flags — the caller folds
    /// them through `resolve_overlay_active` with worktree eligibility.
    pub(crate) fn overlay_tristate(&self) -> Option<(bool, bool)> {
        match self {
            Commands::Scout { args, .. } => Some((args.overlay.overlay, args.overlay.no_overlay)),
            Commands::Gather { args, .. } => Some((args.overlay.overlay, args.overlay.no_overlay)),
            Commands::Task { args, .. } => Some((args.overlay.overlay, args.overlay.no_overlay)),
            _ => None,
        }
    }

    /// `true` when this invocation mutates the resolved `.cqs/` index
    /// (or its slots / cache / refs) — the set the parent-index write
    /// guard gates. Pure reads and daemon-forwarded queries return
    /// `false` so the guard never fires on the worktree→main read path.
    ///
    /// Subcommand-bearing variants (`notes` / `cache` / `slot` / `ref` /
    /// `model`) classify per inner subcommand: `list` / `stats` /
    /// `active` / `show` are reads; `add` / `update` / `remove` /
    /// `prune` / `clear` / `compact` / `create` / `promote` / `swap`
    /// mutate. The match is intentionally explicit (no `..` wildcard on
    /// the inner enums) so a new mutating subcommand fails to compile
    /// here rather than silently escaping the guard.
    pub(crate) fn mutates_index(&self) -> bool {
        use crate::cli::commands::{
            CacheCommand, ModelCommand, NotesCommand, RefCommand, SlotCommand,
        };
        match self {
            // Always-mutating top-level commands.
            Commands::Init { .. }
            | Commands::Index { .. }
            | Commands::Watch { .. }
            | Commands::Gc { .. } => true,
            // `notes add|update|remove` write notes.toml + reindex; `list` reads.
            Commands::Notes { subcmd } => match subcmd {
                NotesCommand::Add { .. }
                | NotesCommand::Update { .. }
                | NotesCommand::Remove { .. } => true,
                NotesCommand::List { .. } => false,
            },
            // `cache clear|prune|compact` mutate the cache DB; `stats` reads.
            Commands::Cache { subcmd } => match subcmd {
                CacheCommand::Clear { .. }
                | CacheCommand::Prune { .. }
                | CacheCommand::Compact { .. } => true,
                CacheCommand::Stats { .. } => false,
            },
            // `slot create|promote|remove` mutate the slot tree; `list`/`active` read.
            Commands::Slot { subcmd } => match subcmd {
                SlotCommand::Create { .. }
                | SlotCommand::Promote { .. }
                | SlotCommand::Remove { .. } => true,
                SlotCommand::List { .. } | SlotCommand::Active { .. } => false,
            },
            // `ref add|update|remove` write the reference registry / dirs; `list` reads.
            Commands::Ref { subcmd } => match subcmd {
                RefCommand::Add { .. } | RefCommand::Update { .. } | RefCommand::Remove { .. } => {
                    true
                }
                RefCommand::List { .. } => false,
            },
            // `model swap` backs up + reindexes; `show`/`list` read.
            Commands::Model { subcmd } => match subcmd {
                ModelCommand::Swap { .. } => true,
                ModelCommand::Show { .. } | ModelCommand::List { .. } => false,
            },
            // Everything else is a read, a daemon-forwarded query, or a
            // non-index-mutating utility (doctor, completions, eval, …).
            _ => false,
        }
    }
}

/// Classifier used by `try_daemon_query` to decide whether a CLI command can
/// be forwarded to the batch daemon.
///
/// Every `Commands` variant must classify itself here, the `match` is
/// exhaustive (no wildcard), and adding a new CLI variant without picking a
/// classification fails to compile.
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

// `Commands::batch_support()`, `Commands::variant_name()`,
// `dispatch_group_a()`, and `dispatch_group_b()` are emitted by the
// `cqs_macros::CqsCommands` derive on the enum above. Per-variant attributes
// carry the dispatch metadata.
//
// Runtime batch_support helpers for variants whose support level depends
// on the inner subcommand:

/// `cqs notes list` is daemon-dispatchable (read-only); mutations
/// (`add`/`update`/`remove`) must hit the CLI so the filesystem reindex
/// fires. Wired in via `#[cqs_cmd(batch = "runtime")]` on the `Notes`
/// variant.
#[allow(dead_code)] // referenced by name from the derive-generated match
pub(crate) fn notes_batch_support(cmd: &Commands) -> BatchSupport {
    match cmd {
        Commands::Notes { subcmd } => match subcmd {
            crate::cli::commands::NotesCommand::List { .. } => BatchSupport::Daemon,
            _ => BatchSupport::Cli,
        },
        _ => unreachable!("notes_batch_support called on non-Notes variant"),
    }
}

/// `cqs suggest --apply` rewrites notes.toml + reindexes (write); the
/// dry-run path is read-only and daemon-dispatchable.
#[allow(dead_code)]
pub(crate) fn suggest_batch_support(cmd: &Commands) -> BatchSupport {
    match cmd {
        Commands::Suggest { args, .. } => {
            if args.apply {
                BatchSupport::Cli
            } else {
                BatchSupport::Daemon
            }
        }
        _ => unreachable!("suggest_batch_support called on non-Suggest variant"),
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

    /// Verify clap default_value strings match the f32 constants.
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

    // parse_finite_f32 accepts finite values and rejects NaN/±Inf.
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

    // `parse_unit_f32` is `parse_finite_f32` bounded to [0.0, 1.0] so flags
    // like `--name-boost 1.5` surface a clap parse error instead of silently
    // degrading scoring.
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

    // Spot-check the batch-support classifier. The exhaustive match in
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
        // notes add/update/remove reindex the filesystem; list mode is
        // daemon-safe. The classifier must distinguish them.
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

    /// Part A: `cqs scout|gather|task <q> --overlay` binds the overlay
    /// flag to the SUBCOMMAND (the flattened `OverlayArgs`), not the top-level
    /// `Cli.overlay`, and `overlay_tristate()` reads it back. Non-overlay
    /// commands return `None`.
    #[test]
    fn overlay_tristate_reads_subcommand_flags() {
        use clap::Parser;
        for cmd in ["scout", "gather", "task"] {
            let cli = Cli::try_parse_from(["cqs", cmd, "q", "--overlay"])
                .unwrap_or_else(|e| panic!("{cmd} --overlay must parse: {e}"));
            // The flag bound to the subcommand, NOT the top-level default.
            assert!(
                !cli.overlay,
                "{cmd}: --overlay must bind to the subcommand, not top-level Cli.overlay"
            );
            assert_eq!(
                cli.command.as_ref().unwrap().overlay_tristate(),
                Some((true, false)),
                "{cmd}: overlay_tristate must report the subcommand flags"
            );
        }
        // A non-overlay command has no tri-state.
        let cli = Cli::try_parse_from(["cqs", "impact", "foo"]).unwrap();
        assert_eq!(cli.command.unwrap().overlay_tristate(), None);
    }

    /// `mutates_index()` gates the parent-index write guard. Pin the
    /// policy for the destructive set and a few read-only neighbors so an
    /// accidental flip (e.g. classifying `notes list` as a write, or
    /// dropping `index`) fails here rather than silently changing which
    /// commands the worktree guard fires for.
    #[test]
    fn mutates_index_classifies_writes_vs_reads() {
        use clap::Parser;
        let parse = |argv: &[&str]| {
            let mut full = vec!["cqs"];
            full.extend_from_slice(argv);
            Cli::try_parse_from(full)
                .unwrap_or_else(|e| panic!("argv {argv:?} must parse: {e}"))
                .command
                .expect("argv must produce a subcommand")
        };

        // Writes — must be guarded.
        for argv in [
            &["init"][..],
            &["index"][..],
            &["gc"][..],
            &["watch"][..],
            &["notes", "add", "n"][..],
            &["notes", "update", "n"][..],
            &["notes", "remove", "n"][..],
            &["cache", "prune"][..],
            &["cache", "clear"][..],
            &["cache", "compact"][..],
            &["slot", "create", "s"][..],
            &["slot", "promote", "s"][..],
            &["slot", "remove", "s"][..],
            &["ref", "add", "name", "/src"][..],
            &["ref", "remove", "name"][..],
            &["model", "swap", "bge-large"][..],
        ] {
            assert!(
                parse(argv).mutates_index(),
                "{argv:?} must be classified as an index mutation"
            );
        }

        // Reads / non-mutating — must NOT be guarded (the worktree→main
        // read path stays silent).
        for argv in [
            &["notes", "list"][..],
            &["cache", "stats"][..],
            &["slot", "list"][..],
            &["slot", "active"][..],
            &["ref", "list"][..],
            &["model", "show"][..],
            &["model", "list"][..],
            &["scout", "foo"][..],
            &["impact", "foo"][..],
            &["read", "src/lib.rs"][..],
            &["doctor"][..],
            &["stats"][..],
        ] {
            assert!(
                !parse(argv).mutates_index(),
                "{argv:?} must NOT be classified as an index mutation"
            );
        }
    }

    /// `--parent-index` and `-q`/`--quiet` are `global = true`: they parse
    /// after a subcommand. Pins the globalization so a regression
    /// (dropping `global = true`) fails here rather than at agent runtime.
    #[test]
    fn quiet_and_parent_index_are_global_flags() {
        use clap::Parser;
        // `-q` after the subcommand.
        let cli = Cli::try_parse_from(["cqs", "index", "-q"]).expect("`index -q` must parse");
        assert!(
            cli.quiet,
            "-q must set quiet when placed after the subcommand"
        );
        // `--parent-index` after the subcommand.
        let cli = Cli::try_parse_from(["cqs", "index", "--parent-index"])
            .expect("`index --parent-index` must parse");
        assert!(
            cli.parent_index,
            "--parent-index must parse after the subcommand"
        );
        // Both still parse before the subcommand too.
        let cli =
            Cli::try_parse_from(["cqs", "--quiet", "init"]).expect("`--quiet init` must parse");
        assert!(cli.quiet);
    }

    /// Top-level `--expand-parent` (bool, parent context) and
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
            Some(Commands::Gather { ref args, .. }) => assert_eq!(args.depth, 2),
            _ => panic!("expected Gather command"),
        }
    }

    /// `cqs affected --stdin` accepts a captured diff piped in, matching
    /// `review`/`ci`/`impact-diff`. Pinning the parse here so the flag stays
    /// valid.
    #[test]
    fn cli_affected_accepts_stdin_flag() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["cqs", "affected", "--stdin"]).unwrap();
        match cli.command {
            Some(Commands::Affected { stdin, .. }) => assert!(stdin),
            _ => panic!("expected Affected command"),
        }
    }

    /// `Commands::variant_name()` is derive-generated. Pin a few high-traffic
    /// command labels so a typo in the registration string surfaces as a test
    /// failure rather than rotting in tracing output. Exhaustiveness already
    /// prevents missing rows; this test is the second line of defense for
    /// typos.
    #[test]
    fn variant_name_pins_critical_command_labels() {
        use clap::Parser;
        let cases = [
            (vec!["cqs", "init"], "init"),
            (vec!["cqs", "scout", "foo"], "scout"),
            (vec!["cqs", "impact", "foo"], "impact"),
            (vec!["cqs", "impact-diff"], "impact-diff"),
            (vec!["cqs", "test-map", "foo"], "test-map"),
            (vec!["cqs", "audit-mode"], "audit-mode"),
            (vec!["cqs", "export-model", "--repo", "x"], "export-model"),
            (
                vec!["cqs", "train-data", "--repos", ".", "--output", "x"],
                "train-data",
            ),
            (vec!["cqs", "refresh"], "refresh"),
        ];
        for (argv, expected) in cases {
            let cli = Cli::try_parse_from(&argv).unwrap();
            assert_eq!(
                cli.command.unwrap().variant_name(),
                expected,
                "variant_name mismatch for {argv:?}"
            );
        }
    }

    /// Regression guard pinning the **set** of top-level CLI subcommands.
    /// With per-variant `#[cqs_cmd(...)]` attributes driving dispatch,
    /// accidentally renaming or dropping a `Commands` variant compiles
    /// cleanly because kebab-case auto-derives. Without this guard, the help
    /// text could silently drift out from under agents that have command-name
    /// muscle memory baked into prompts.
    ///
    /// Adding a variant means appending to `EXPECTED_SUBCOMMANDS`
    /// below — the intentional friction. Description / arg-flag
    /// churn doesn't invalidate the snapshot because we compare
    /// names only, not full help text.
    #[test]
    fn cli_subcommand_set_unchanged() {
        use clap::CommandFactory;

        // Sorted, kebab-case. Cfg-gated commands (`serve`, `convert`)
        // are listed because the default-feature build (which CI
        // uses) enables them.
        const EXPECTED_SUBCOMMANDS: &[&str] = &[
            "affected",
            "audit-mode",
            "batch",
            "blame",
            "brief",
            "cache",
            "callees",
            "callers",
            "chat",
            "ci",
            "completions",
            "context",
            "convert",
            "dead",
            "deps",
            "diff",
            "doctor",
            "drift",
            "eval",
            "explain",
            "export-model",
            "gather",
            "gc",
            "health",
            "hook",
            "impact",
            "impact-diff",
            "index",
            "init",
            "model",
            "neighbors",
            "notes",
            "onboard",
            "ping",
            "plan",
            "project",
            "read",
            "reconstruct",
            "ref",
            "refresh",
            "related",
            "review",
            "scout",
            "serve",
            "similar",
            "slot",
            "stale",
            "stats",
            "status",
            "suggest",
            "task",
            "telemetry",
            "test-map",
            "trace",
            "train-data",
            "train-pairs",
            "watch",
            "where",
        ];

        let cmd = Cli::command();
        let mut actual: Vec<String> = cmd
            .get_subcommands()
            .map(|sc| sc.get_name().to_string())
            .collect();
        actual.sort();
        let expected: Vec<String> = EXPECTED_SUBCOMMANDS.iter().map(|s| s.to_string()).collect();

        if actual != expected {
            let only_in_actual: Vec<&String> =
                actual.iter().filter(|n| !expected.contains(n)).collect();
            let only_in_expected: Vec<&String> =
                expected.iter().filter(|n| !actual.contains(n)).collect();
            panic!(
                "CLI subcommand set drifted from EXPECTED_SUBCOMMANDS in \
                 src/cli/definitions.rs::tests. Update the array. \
                 Added (in CLI, not in test): {only_in_actual:?}; \
                 removed (in test, not in CLI): {only_in_expected:?}. \
                 Full actual list: {actual:?}"
            );
        }
    }
}
