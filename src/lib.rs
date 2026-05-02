#![allow(clippy::doc_lazy_continuation)] // Bulk doc comment cleanup left some continuation formatting
//! # cqs - Code Intelligence and RAG for AI Agents
//!
//! Semantic search, call graph analysis, impact tracing, type dependencies, and smart
//! context assembly — all in single tool calls. Local ML embeddings, GPU-accelerated.
//!
//! ## Features
//!
//! - **Semantic search**: Hybrid RRF (keyword + vector) with configurable embedding models (BGE-large default; E5-base, nomic-coderank-137M, and custom ONNX presets). 90.9% Recall@1 on 296-query expanded eval.
//! - **Call graphs**: Callers, callees, transitive impact, shortest-path tracing between functions
//! - **Impact analysis**: What breaks if you change X? Callers + affected tests + risk scoring
//! - **Type dependencies**: Who uses this type? What types does this function use?
//! - **Smart context assembly**: `gather` (search + BFS expansion), `task` (scout + gather + impact + placement), `scout` (pre-investigation dashboard)
//! - **Diff review & CI**: Structured risk analysis, dead code detection in diffs, gating pipeline
//! - **Batch & chat modes**: Persistent session with pipeline syntax (`search "error" | callers | test-map`)
//! - **Notes with sentiment**: Unified memory system for AI collaborators
//! - **Multi-language**: 54 languages + L5X/L5K PLC exports, with multi-grammar injection (HTML→JS/CSS, Svelte, Vue, Razor, etc.)
//! - **Type-aware embeddings**: Full signatures appended to NL descriptions for richer type discrimination
//! - **Doc comment generation**: `--improve-docs` generates and writes doc comments to source files via LLM
//! - **HyDE query predictions**: `--hyde-queries` generates synthetic search queries per function for improved recall
//! - **Training data generation**: `train-data` command generates fine-tuning triplets from git history
//! - **GPU acceleration**: CUDA/TensorRT with CPU fallback (CoreML and ROCm scaffolding present, wiring deferred per #956 Phase B/C)
//! - **Document conversion**: PDF, HTML, CHM, Web Help → cleaned Markdown (optional `convert` feature)
//!
//! ## Quick Start
//!
//! ```no_run
//! use cqs::{Embedder, Parser, Store};
//! use cqs::embedder::ModelConfig;
//! use cqs::store::SearchFilter;
//!
//! # fn main() -> anyhow::Result<()> {
//! // Initialize components. `resolve_index_db` honours the slot layout
//! // (post-#1105: `.cqs/slots/<active>/index.db`) and falls back to the
//! // pre-migration `.cqs/index.db` path on unmigrated projects.
//! let parser = Parser::new()?;
//! let embedder = Embedder::new(ModelConfig::resolve(None, None))?;
//! let cqs_dir = cqs::resolve_index_dir(std::path::Path::new("."));
//! let store = Store::open(&cqs::resolve_index_db(&cqs_dir))?;
//!
//! // Parse and embed a file
//! let chunks = parser.parse_file(std::path::Path::new("src/main.rs"))?;
//! let embeddings = embedder.embed_documents(
//!     &chunks.iter().map(|c| c.content.as_str()).collect::<Vec<_>>()
//! )?;
//!
//! // Search for similar code (hybrid RRF search)
//! let query_embedding = embedder.embed_query("parse configuration file")?;
//! let filter = SearchFilter {
//!     enable_rrf: true,
//!     query_text: "parse configuration file".to_string(),
//!     ..Default::default()
//! };
//! let results = store.search_filtered(&query_embedding, &filter, 5, 0.3)?;
//! # Ok(())
//! # }
//! ```
//!
// RB-V1.29-6: cqs routinely narrows `u64` row counts and SQLite `i64` row IDs
// to `usize` (e.g. `chunk_count as usize` in `cli/store.rs`, HNSW ID map
// loads, store batch readers). On a 32-bit target those casts would silently
// truncate once a corpus exceeds ~4 billion elements. Rather than sprinkle
// per-site checked casts everywhere, gate the whole crate at compile time —
// every target cqs ships for is 64-bit anyway.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("cqs requires a 64-bit target (target_pointer_width = \"64\")");

// Public library API modules
pub mod audit;
pub mod aux_model;
pub mod cache;
pub mod config;
pub mod convert;
pub mod embedder;
pub mod fs;
pub mod hnsw;
pub mod index;
pub mod language;
pub mod note;
pub mod parser;
pub mod reference;
pub mod splade;
pub mod store;
pub mod train_data;
pub mod vendored;
pub mod worktree;

pub mod ci;
pub mod eval;
pub mod health;
pub mod reranker;
#[cfg(feature = "serve")]
pub mod serve;
pub mod slot;
pub mod suggest;

// Internal modules - not part of public library API
// These are pub(crate) to hide implementation details, but specific items are
// re-exported below for use by the binary crate (CLI) and integration tests.
pub(crate) mod diff;
pub(crate) mod diff_parse;
pub use diff_parse::{parse_unified_diff, DiffHunk};
pub mod drift;
pub use drift::{detect_drift, DriftEntry, DriftResult};
pub(crate) mod focused_read;
pub(crate) mod gather;
pub(crate) mod impact;
pub(crate) mod limits;
pub(crate) mod math;
pub(crate) mod nl;
pub(crate) mod onboard;
pub(crate) mod ort_helpers;
pub(crate) mod project;
pub(crate) mod related;
pub(crate) mod review;
pub use review::{review_diff, ReviewNoteEntry, ReviewResult, ReviewedFunction, RiskSummary};
#[cfg(feature = "llm-summaries")]
pub mod doc_writer;
#[cfg(feature = "llm-summaries")]
pub mod llm;
pub mod plan;
pub(crate) mod scout;
pub mod search;
pub(crate) mod structural;
pub(crate) mod task;
pub(crate) mod where_to_add;

// #972: pure arg-shaping translator for daemon-forward mode. Lives in the
// library (not `src/cli/`) so integration tests can exercise it without
// reaching into the binary-only `cli` module tree.
pub mod daemon_translate;

// #1182: freshness snapshot exposed via the daemon socket so `cqs status
// --watch-fresh` can answer "is the index fresh?". Watch loop publishes
// every cycle; daemon socket handler reads. Lives in lib so the CLI's
// `cqs status` command and the `daemon_status` socket helper share the
// wire shape.
pub mod watch_status;

#[cfg(test)]
pub mod test_helpers;

#[cfg(feature = "cuda-index")]
pub mod cagra;

pub use audit::parse_duration;
pub use embedder::{Embedder, Embedding};
pub use hnsw::HnswIndex;
pub use index::{IndexResult, VectorIndex};
pub use note::{
    parse_notes, path_matches_mention, rewrite_notes_file, NoteEntry, NoteError, NoteFile,
    NOTES_HEADER,
};
pub use parser::{Chunk, Parser};
pub use reranker::{LlmReranker, NoopReranker, OnnxReranker, Reranker};
pub use store::{HnswKind, ModelInfo, SearchFilter, Store};

// Re-exports for binary crate (CLI) - these are NOT part of the public library API
// but need to be accessible to src/cli/* and tests/.
// Wildcard re-exports: no external users, so name conflicts are compiler-caught.
pub use diff::*;
pub use focused_read::COMMON_TYPES;
pub use gather::*;
/// Cross-project call graph types and context.
pub mod cross_project {
    pub use crate::impact::cross_project::{
        analyze_impact_cross, trace_cross, CrossProjectHop, CrossProjectTraceResult,
    };
    pub use crate::store::calls::cross_project::{
        CrossProjectCallee, CrossProjectCaller, CrossProjectContext, CrossProjectTestChunk,
        NamedStore,
    };
}
pub use impact::*;
pub use nl::{
    generate_nl_description, generate_nl_description_with_seq_len,
    generate_nl_with_call_context_and_summary, generate_nl_with_template,
    generate_nl_with_template_and_seq_len, normalize_for_fts, tokenize_identifier, CallContext,
    NlTemplate,
};
pub use onboard::*;
pub use project::*;
pub use related::*;
pub use scout::*;
pub use search::*;
pub use structural::Pattern;
pub use task::*;
pub use where_to_add::*;

#[cfg(feature = "cuda-index")]
pub use cagra::CagraIndex;

use std::path::PathBuf;

/// Unified error type for analysis operations (scout, where-to-add, etc.)
///
/// Replaces the former `ScoutError` and `SuggestError` which were near-identical.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error("embedding failed: {0}")]
    Embedder(#[from] embedder::EmbedderError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{phase} phase failed: {message}")]
    Phase {
        phase: &'static str,
        message: String,
    },
}

/// Name of the per-project index directory (created by `cqs init`).
pub const INDEX_DIR: &str = ".cqs";

/// Filename of the SQLite index database inside [`INDEX_DIR`].
pub const INDEX_DB_FILENAME: &str = "index.db";

/// Legacy index directory name (pre-v0.9.7). Used for auto-migration.
const LEGACY_INDEX_DIR: &str = ".cq";

/// Resolve the index directory for a project, migrating from `.cq/` to `.cqs/` if needed.
///
/// If the legacy `.cq/` exists and `.cqs/` does not, renames it automatically.
/// Falls back gracefully if the rename fails (e.g., permissions).
///
/// **Worktree fallback (#1254):** when neither `<project_root>/.cqs/` nor
/// the legacy `<project_root>/.cq/` exists AND `project_root` is a git
/// worktree (per [`crate::worktree::resolve_main_project_dir`]), this
/// function returns the main project's `.cqs/` if that one exists, and
/// records the redirect via [`crate::worktree::record_worktree_stale`].
/// JSON envelope writers consult [`crate::worktree::is_worktree_stale`]
/// and add `worktree_stale: true` + `worktree_name` to `_meta` so
/// consuming agents know the served index reflects main's branch, not
/// uncommitted edits in this worktree. When the main project is also
/// uninitialised the function returns `<project_root>/.cqs/` unchanged
/// so the caller's "no index found" error message points at the
/// expected layout.
pub fn resolve_index_dir(project_root: &Path) -> PathBuf {
    let new_dir = project_root.join(INDEX_DIR);
    let old_dir = project_root.join(LEGACY_INDEX_DIR);

    if old_dir.exists() && !new_dir.exists() && std::fs::rename(&old_dir, &new_dir).is_ok() {
        tracing::info!("Migrated index directory from .cq/ to .cqs/");
    }

    if new_dir.exists() {
        return new_dir;
    }
    if old_dir.exists() {
        return old_dir;
    }

    // #1254: worktree fallback. `git worktree add` doesn't copy
    // `.cqs/`; pre-fix this returned the empty path and every cqs
    // command in the worktree errored, which led agents to fall back
    // to absolute paths under main's tree.
    match crate::worktree::lookup_main_cqs_dir(project_root) {
        crate::worktree::MainIndexLookup::WorktreeUseMain {
            worktree_root,
            main_cqs,
            ..
        } => {
            crate::worktree::record_worktree_stale(&worktree_root);
            tracing::info!(
                worktree = %worktree_root.display(),
                main_cqs = %main_cqs.display(),
                "Worktree has no .cqs/; serving from main's index (responses tagged worktree_stale=true)"
            );
            return main_cqs;
        }
        crate::worktree::MainIndexLookup::WorktreeMainEmpty {
            worktree_root,
            main_root,
        } => {
            // Both the worktree and main are uninitialised. The
            // caller will error with "no index found" against the
            // returned (worktree) path; the warn here makes both
            // candidate paths visible in the journal so the operator
            // knows running `cqs index` in main also fixes the
            // worktree.
            tracing::warn!(
                worktree = %worktree_root.display(),
                main = %main_root.display(),
                worktree_cqs = %new_dir.display(),
                main_cqs = %main_root.join(INDEX_DIR).display(),
                "No cqs index found in worktree OR in its main project — \
                 run `cqs index` in either to populate (main is preferred since \
                 worktrees pick up its index automatically)"
            );
        }
        crate::worktree::MainIndexLookup::OwnIndex { .. }
        | crate::worktree::MainIndexLookup::NotWorktree => {
            // OwnIndex is unreachable here (we only entered this
            // branch because new_dir didn't exist, which contradicts
            // OwnIndex). NotWorktree is the regular case — fall
            // through to return new_dir below.
        }
    }

    new_dir
}

/// Compute the slot directory path: `<project_cqs_dir>/slots/<slot_name>/`.
///
/// Convenience wrapper around [`crate::slot::slot_dir`] for callers that
/// already imported `cqs::resolve_index_dir`.
pub fn resolve_slot_dir(project_cqs_dir: &Path, slot_name: &str) -> PathBuf {
    crate::slot::slot_dir(project_cqs_dir, slot_name)
}

/// Resolve the active index.db path within a project's `.cqs/` dir.
///
/// Honors the slot resolution order (`CQS_SLOT` env > `.cqs/active_slot` file >
/// `"default"`) and falls back to the pre-migration `.cqs/index.db` layout for
/// unmigrated projects (cross-project search, external references).
///
/// Returns the slot path even when nothing exists, so the caller's
/// "not found" error message points at the forward-looking layout.
pub fn resolve_index_db(project_cqs_dir: &Path) -> PathBuf {
    if let Ok(resolved) = crate::slot::resolve_slot_name(None, project_cqs_dir) {
        let slot_path =
            crate::slot::slot_dir(project_cqs_dir, &resolved.name).join(INDEX_DB_FILENAME);
        if slot_path.exists() {
            return slot_path;
        }
    }
    let legacy = project_cqs_dir.join(INDEX_DB_FILENAME);
    if legacy.exists() {
        return legacy;
    }
    crate::slot::slot_dir(project_cqs_dir, crate::slot::DEFAULT_SLOT).join(INDEX_DB_FILENAME)
}

/// Default embedding dimension (1024, BGE-large-en-v1.5).
/// The actual dimension is detected at runtime from the model output.
/// Use `Embedder::embedding_dim()` for the runtime value.
/// Derived from `ModelConfig::default_model().dim`.
pub const EMBEDDING_DIM: usize = embedder::DEFAULT_DIM;

/// EX-V1.30.1-7 (P3-EX-2): test whether a string is one of the canonical
/// "off" tokens the cqs CLI accepts in `CQS_*` env vars.
///
/// Mirrors the dispatch the audit found duplicated across ~30 sites:
/// `"0"`, `"false"`, `"no"`, `"off"` — case-insensitive, whitespace-trimmed.
/// Centralised here so the next migration pass can swap a hand-rolled
/// match for a single call without re-debating the spelling list.
///
/// Companion `env_truthy` is intentionally not added today — the audit's
/// 30-site backlog only matters once we start migrating the rest, and
/// the truthy spelling list (e.g. should `"y"` work?) deserves its own
/// pass.
#[inline]
pub fn env_falsy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// DS2-10: Convert a [`std::time::Duration`] to milliseconds as `i64` for
/// storage in SQLite `INTEGER` mtime columns.
///
/// The underlying `as_millis()` returns `u128`, and all 13 prior call sites
/// lost the overflow check by casting with `as i64` — a duration past
/// `~292M years` since the Unix epoch would silently wrap to a negative
/// value. `i64::try_from` returns an error which we collapse to
/// `i64::MAX` (the closest representable "far future" mtime); the alternative
/// — truncating wrap — would invert monotonic ordering and break freshness
/// comparisons. Real-world mtimes are never anywhere near this cap, so the
/// saturation is functionally equivalent to the prior cast on every valid
/// input.
#[inline]
pub fn duration_to_mtime_millis(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// RB-3 / RB-10: Defensive `SystemTime::now() → Unix seconds as i64`.
///
/// Returns `None` when the clock is before epoch (RTC mis-set, hypervisor
/// pause, NTP-pre-sync boot) and emits a `tracing::warn!` once per process
/// so journalctl operators can correlate stale snapshots / timestamps with
/// bad-clock conditions. Use everywhere instead of bare
/// `SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64)`
/// which silently returns `None` on overflow and `0` on epoch errors.
pub fn unix_secs_i64() -> Option<i64> {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    match std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(_) => {
            WARNED.get_or_init(|| {
                tracing::warn!(
                    "system clock is before UNIX_EPOCH — timestamps will be None until NTP sync; check `timedatectl` / `chronyc tracking`",
                );
            });
            None
        }
    }
}

// # Batch Size Constants (#683)
//
// ~25 `const BATCH_SIZE` definitions across store/pipeline/search modules.
// Intentionally local — each is tuned for its SQL query shape:
//
// SQLite limit: max 999 bind parameters per statement. A query with N columns
// per row can batch `floor(999 / N)` rows.
//
// Common sizes:
//   500   — 1-2 param queries (chunks, embeddings, calls)
//   200   — 4-5 params per row (type edges, call graph)
//   132   — upsert_chunk (5 params, 132 × 5 = 660)
//   100   — staleness checks with path matching
//   20    — enrichment hash (many columns)
//
// Non-SQL:
//   EMBED_BATCH_SIZE = 64    — ONNX inference (CQS_EMBED_BATCH_SIZE)
//   FILE_BATCH_SIZE = 5000   — pipeline file processing (CQS_FILE_BATCH_SIZE)
//   HNSW_BATCH_SIZE = 10000  — HNSW insert
//   MAX_BATCH_SIZE = 10000   — Claude Batches API limit
//
// Do not centralize. If adding a batched SQL query: floor(999 / params_per_row).

/// Unified test-chunk detection heuristic.
///
/// Returns `true` if a chunk looks like a test based on its name or file path.
/// Used by scout, impact, and where_to_add to filter out tests from analysis.
///
/// **Not** used by `store::calls::find_dead_code`, which has its own SQL-based
/// detection (`TEST_NAME_PATTERNS`, `TEST_CONTENT_MARKERS`, `TEST_PATH_PATTERNS`)
/// that also checks content markers like `#[test]` and `@Test`.
pub fn is_test_chunk(name: &str, file: &str) -> bool {
    // Name-based patterns (language-agnostic).
    //
    // v1.22.0 audit AC-4: previously `name.starts_with("Test")` demoted
    // production types like TestRegistry, TestHarness, TestContext by 30%.
    // Tightened to require `Test` followed by underscore or end-of-name
    // (catches `test_foo`, `Test_bar`, but not `TestHarness`). The xUnit
    // `TestFoo` naming convention would still be caught but Go's and
    // Rust's `test_` prefix is the dominant pattern in practice.
    let name_match = name.starts_with("test_")
        || name.starts_with("Test_")
        || name == "Test"
        || name.starts_with("spec_")
        || name.ends_with("_test")
        || name.ends_with("_spec")
        || name.contains("_test_")
        || name.contains(".test");
    if name_match {
        return true;
    }
    // Path-based patterns from the language registry (all 54 languages).
    // Patterns use SQL LIKE syntax: `%` = any chars, `\_` = literal underscore.
    // Normalize backslashes to forward slashes for cross-platform matching.
    let normalized = file.replace('\\', "/");
    for pattern in language::REGISTRY.all_test_path_patterns() {
        if sql_like_matches(&normalized, pattern) {
            return true;
        }
        // Registry patterns assume a prefix (e.g. `%/tests/%` needs something
        // before `/tests/`). Relative paths like `tests/foo.rs` lack that prefix,
        // so also try with a synthetic `/` prepended.
        if !normalized.starts_with('/') {
            let prefixed = format!("/{normalized}");
            if sql_like_matches(&prefixed, pattern) {
                return true;
            }
        }
    }
    false
}

/// Match a path against a SQL `LIKE` pattern.
///
/// Supports `%` (any sequence of characters) and `\_` (literal underscore).
/// All other characters are matched literally. Used by `is_test_chunk` to
/// evaluate the registry's `test_path_patterns`.
fn sql_like_matches(path: &str, pattern: &str) -> bool {
    // Convert SQL LIKE pattern to segments split on `%`.
    // `\_` is an escaped literal underscore in SQL LIKE — unescape it first.
    let unescaped = pattern.replace("\\_", "_");
    let parts: Vec<&str> = unescaped.split('%').collect();

    // Single segment (no wildcards) — exact match.
    if parts.len() == 1 {
        return path == parts[0];
    }

    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // First segment must be a prefix.
            if !path.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            // Last segment must be a suffix, and must not overlap with what we've consumed.
            if !path[pos..].ends_with(part) {
                return false;
            }
        } else {
            // Interior segment — find it after `pos`.
            match path[pos..].find(part) {
                Some(offset) => pos += offset + part.len(),
                None => return false,
            }
        }
    }
    true
}

use std::path::Path;

/// Normalize a path to a string with forward slashes.
///
/// Converts `Path`/`PathBuf` to `String`, replacing backslashes with forward slashes
/// for cross-platform consistency (WSL, Windows paths in JSON output).
///
/// P3 #142: strips the Windows `\\?\` UNC prefix (and `\\?\UNC\`) before
/// slash conversion so JSON output and chunk IDs don't carry the verbatim
/// path marker. `dunce::canonicalize` strips most of these at ingest, but
/// callers passing already-canonicalized `&Path` deserve symmetric behavior.
pub fn normalize_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let stripped = strip_windows_verbatim_prefix(&raw);
    stripped.replace('\\', "/")
}

/// Normalize backslashes to forward slashes in a string path.
///
/// For already-stringified paths. Strips Windows `\\?\` / `\\?\UNC\` verbatim
/// prefix (P3 #142) before slash conversion. Returns the input unchanged on
/// non-Windows-flavored strings.
pub fn normalize_slashes(path: &str) -> String {
    strip_windows_verbatim_prefix(path).replace('\\', "/")
}

/// Strip the Windows verbatim path prefix before slash normalization.
///
/// Recognized prefixes:
/// - `\\?\C:\foo`  → `C:\foo`
/// - `\\?\UNC\server\share` → `\\server\share`
fn strip_windows_verbatim_prefix(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        // `\\?\UNC\server\share` → `\\server\share`
        let mut out = String::with_capacity(rest.len() + 2);
        out.push_str(r"\\");
        out.push_str(rest);
        out
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// Generate an unpredictable u64 suffix for temporary file names.
///
/// Uses [`std::collections::hash_map::RandomState`] (seeded by the OS on each
/// process start) to produce a value that is different every run and resists
/// symlink-based TOCTOU attacks on temp-file paths.
pub fn temp_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// Extract a human-readable message from a thread panic payload.
///
/// Handles `&str` and `String` payloads (the two common forms produced by
/// `panic!`); falls back to `"unknown panic"` for any other type.
pub fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Serde serializer for `PathBuf` fields: forward-slash normalized.
///
/// Use as `#[serde(serialize_with = "crate::serialize_path_normalized")]`
pub fn serialize_path_normalized<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&normalize_path(path))
}

/// Relativize a path against a root and normalize separators for display.
///
/// Strips `root` prefix if present, converts backslashes to forward slashes.
pub fn rel_display(path: &Path, root: &Path) -> String {
    normalize_path(path.strip_prefix(root).unwrap_or(path))
}

// ============ Note Indexing Helper ============

/// Index notes into the database (store without embeddings)
///
/// Shared logic used by CLI commands.
/// Stores notes in the database for mention-based lookup (SQ-9: note embeddings removed).
///
/// # Arguments
/// * `notes` - Notes to index
/// * `notes_path` - Path to notes file (for mtime tracking)
/// * `store` - Store for persisting notes
///
/// # Returns
/// Number of notes indexed
pub fn index_notes(
    notes: &[note::Note],
    notes_path: &Path,
    store: &Store<store::ReadWrite>,
) -> anyhow::Result<usize> {
    let _span =
        tracing::info_span!("index_notes", path = %notes_path.display(), count = notes.len())
            .entered();

    if notes.is_empty() {
        return Ok(0);
    }

    // Get file mtime
    let file_mtime = notes_path
        .metadata()
        .and_then(|m| m.modified())
        .map_err(|e| {
            tracing::trace!(path = %notes_path.display(), error = %e, "Failed to get file mtime");
            e
        })
        .ok()
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| {
                    tracing::trace!(path = %notes_path.display(), error = %e, "File mtime before Unix epoch");
                })
                .ok()
        })
        .map(duration_to_mtime_millis)
        .unwrap_or(0);

    // Atomically replace notes (delete old + insert new in single transaction)
    store.replace_notes_for_file(notes, notes_path, file_mtime)?;

    Ok(notes.len())
}

// ============ File Enumeration ============

/// Default maximum file size to index (1MB). Generated code (`bindings.rs`
/// blobs, compiled TypeScript, migrations) can exceed this and is silently
/// skipped; tune via `CQS_MAX_FILE_SIZE` (bytes) when that happens.
const DEFAULT_MAX_FILE_SIZE: u64 = 1_048_576;

/// Resolve the per-file size cap, checking `CQS_MAX_FILE_SIZE` first. Zero
/// falls back to the default — disabling the cap entirely would OOM on
/// multi-GB artifacts.
fn max_file_size() -> u64 {
    std::env::var("CQS_MAX_FILE_SIZE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_FILE_SIZE)
}

/// Enumerate files to index in a project directory.
///
/// Respects `.gitignore` and `.cqsignore` (additive on top of `.gitignore`,
/// both disabled by `no_ignore=true`); skips hidden files and files larger
/// than `CQS_MAX_FILE_SIZE` bytes (default 1 MiB — generated code can
/// exceed this). Returns relative paths from the project root.
pub fn enumerate_files(
    root: &Path,
    extensions: &[&str],
    no_ignore: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let _span = tracing::debug_span!("enumerate_files", root = %root.display()).entered();
    use anyhow::Context;
    use ignore::WalkBuilder;

    let root = dunce::canonicalize(root).context("Failed to canonicalize root")?;

    // `.cqsignore` layers on top of `.gitignore` for cqs-specific exclusions
    // (vendored minified JS, large data fixtures, etc.) — files we want
    // committed to git but don't want indexed. Same gitignore syntax,
    // hierarchical (per-directory), and respected by both `cqs index` (here)
    // and `cqs watch` (see `cli/watch.rs::build_gitignore_matcher`).
    // `--no-ignore` disables it alongside .gitignore.
    let mut wb = WalkBuilder::new(&root);
    if !no_ignore {
        wb.add_custom_ignore_filename(".cqsignore");
    }
    let walker = wb
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .ignore(!no_ignore)
        .hidden(!no_ignore)
        .follow_links(false)
        .filter_entry(|entry| {
            // Skip nested git worktrees. A linked worktree's `.git` is a file
            // (not a directory) that contains a `gitdir: ...` pointer. Indexing
            // the worktree would duplicate the entire source tree under a
            // different prefix — this is the root cause of `.claude/worktrees/`
            // pollution in the index.
            if entry.file_type().is_some_and(|ft| ft.is_dir())
                && entry.path().join(".git").is_file()
            {
                return false;
            }
            true
        })
        .build();

    let size_cap = max_file_size();
    let files: Vec<PathBuf> = walker
        .filter_map(|e| {
            e.map_err(|err| {
                tracing::debug!(error = %err, "Failed to read directory entry during walk");
            })
            .ok()
        })
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| match e.metadata() {
            Ok(m) => {
                let len = m.len();
                if len > size_cap {
                    // SHL-V1.25-11: surface skipped oversize files at info
                    // so users debugging "why doesn't my symbol show up"
                    // discover CQS_MAX_FILE_SIZE instead of silent drop.
                    tracing::info!(
                        path = %e.path().display(),
                        size = len,
                        cap = size_cap,
                        "Skipping oversize file (CQS_MAX_FILE_SIZE)"
                    );
                    false
                } else {
                    true
                }
            }
            Err(_) => false,
        })
        .filter(|e| {
            // P3 #141: `to_ascii_lowercase` allocated a fresh `String` per
            // candidate file. `eq_ignore_ascii_case` compares case-insensitively
            // without an allocation.
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
                .unwrap_or(false)
        })
        .filter_map({
            let failure_count = std::sync::atomic::AtomicUsize::new(0);
            move |e| {
                let path = match dunce::canonicalize(e.path()) {
                    Ok(p) => p,
                    Err(err) => {
                        let count =
                            failure_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if count < 3 {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "Failed to canonicalize path, skipping"
                            );
                        } else {
                            tracing::debug!(
                                path = %e.path().display(),
                                error = %err,
                                "Failed to canonicalize path, skipping"
                            );
                        }
                        return None;
                    }
                };
                if path.starts_with(&root) {
                    // RB-6: `starts_with` and `strip_prefix` can disagree on
                    // case-insensitive filesystems (NTFS, HFS+) — `Cqs` vs
                    // `cqs` matches under `starts_with` but `strip_prefix`
                    // does byte-equal segment compare and refuses. The old
                    // `unwrap_or(&path).to_path_buf()` then silently leaked
                    // the absolute path into the relative-path workflow,
                    // breaking every downstream lookup keyed by relative
                    // origin. Skipping with a warn surfaces the
                    // disagreement so the operator can fix the case skew
                    // (or re-canonicalize the project root).
                    match path.strip_prefix(&root) {
                        Ok(rel) => Some(rel.to_path_buf()),
                        Err(_) => {
                            tracing::warn!(
                                path = %path.display(),
                                root = %root.display(),
                                "enumerate_files: starts_with passed but strip_prefix failed (case-insensitive filesystem?) — skipping"
                            );
                            None
                        }
                    }
                } else {
                    tracing::warn!(path = %e.path().display(), "Skipping path outside project");
                    None
                }
            }
        })
        .collect();

    tracing::info!(file_count = files.len(), "File enumeration complete");

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_chunk_name_patterns() {
        // Positive: name-based
        assert!(is_test_chunk("test_foo", "src/lib.rs"));
        assert!(is_test_chunk("Test_suite", "src/lib.rs"));
        assert!(is_test_chunk("Test", "src/lib.rs")); // bare "Test" name
        assert!(is_test_chunk("foo_test", "src/lib.rs"));
        assert!(is_test_chunk("foo_test_bar", "src/lib.rs"));
        assert!(is_test_chunk("foo.test", "src/lib.rs"));
        // Negative: name-based
        // v1.22.0 audit AC-4: "TestSuite", "TestHarness", "TestRegistry" etc.
        // are NOT tests — they're test-framework API types that should not be
        // demoted 30% in search results. Only `test_` and `Test_` prefixes
        // (with underscore) are treated as test chunks by name.
        assert!(!is_test_chunk("TestSuite", "src/lib.rs"));
        assert!(!is_test_chunk("TestHarness", "src/lib.rs"));
        assert!(!is_test_chunk("TestRegistry", "src/lib.rs"));
        assert!(!is_test_chunk("search_filtered", "src/lib.rs"));
        assert!(!is_test_chunk("testing_util", "src/lib.rs"));
    }

    #[test]
    fn test_is_test_chunk_path_patterns() {
        // Positive: path-based
        assert!(is_test_chunk("helper", "tests/helper.rs"));
        assert!(is_test_chunk("helper", "src/tests/helper.rs"));
        assert!(is_test_chunk("helper", "search_test.rs"));
        assert!(is_test_chunk("helper", "search.test.ts"));
        assert!(is_test_chunk("helper", "search.spec.js"));
        assert!(is_test_chunk("helper", "search_test.go"));
        assert!(is_test_chunk("helper", "search_test.py"));
        // Negative: path-based
        assert!(!is_test_chunk("helper", "src/lib.rs"));
        assert!(!is_test_chunk("helper", "src/search.rs"));
    }

    #[test]
    fn test_is_test_chunk_combined() {
        // Both name and path match
        assert!(is_test_chunk("test_helper", "tests/helper.rs"));
        // Name matches, path doesn't
        assert!(is_test_chunk("test_search", "src/search.rs"));
        // Path matches, name doesn't
        assert!(is_test_chunk("setup_fixtures", "tests/fixtures.rs"));
    }

    // ─── rel_display tests ──────────────────────────────────────────────────

    #[test]
    fn test_rel_display_relative_path_within_base() {
        let root = Path::new("/home/user/project");
        let path = Path::new("/home/user/project/src/main.rs");
        assert_eq!(rel_display(path, root), "src/main.rs");
    }

    #[test]
    fn test_rel_display_path_outside_base() {
        let root = Path::new("/home/user/project");
        let path = Path::new("/tmp/other/file.rs");
        // Path outside root — returns full path with normalized separators
        assert_eq!(rel_display(path, root), "/tmp/other/file.rs");
    }

    #[test]
    fn test_rel_display_exact_base_path() {
        let root = Path::new("/home/user/project");
        let path = Path::new("/home/user/project");
        // Exact match — strip_prefix returns ""
        assert_eq!(rel_display(path, root), "");
    }

    #[test]
    fn test_rel_display_backslash_normalization() {
        // Simulate a Windows-style path stored as a PathBuf
        let root = Path::new("/home/user/project");
        let path = PathBuf::from("/home/user/project/src\\cli\\mod.rs");
        assert_eq!(rel_display(&path, root), "src/cli/mod.rs");
    }

    #[test]
    fn test_rel_display_no_common_prefix() {
        let root = Path::new("/opt/tools");
        let path = Path::new("/var/log/app.log");
        assert_eq!(rel_display(path, root), "/var/log/app.log");
    }

    // ─── normalize_path / normalize_slashes verbatim-prefix tests (P3 #142) ─

    #[test]
    fn test_normalize_path_strips_windows_verbatim_prefix() {
        // \\?\C:\foo\bar → C:/foo/bar
        let p = Path::new(r"\\?\C:\foo\bar");
        assert_eq!(normalize_path(p), "C:/foo/bar");
    }

    #[test]
    fn test_normalize_path_strips_windows_unc_verbatim_prefix() {
        // \\?\UNC\server\share\file → //server/share/file
        let p = Path::new(r"\\?\UNC\server\share\file");
        assert_eq!(normalize_path(p), "//server/share/file");
    }

    #[test]
    fn test_normalize_path_passthrough_without_verbatim() {
        let p = Path::new("/usr/local/bin/cqs");
        assert_eq!(normalize_path(p), "/usr/local/bin/cqs");
    }

    #[test]
    fn test_normalize_slashes_strips_verbatim_prefix() {
        assert_eq!(normalize_slashes(r"\\?\C:\foo"), "C:/foo");
        assert_eq!(normalize_slashes(r"\\?\UNC\srv\sh"), "//srv/sh");
        assert_eq!(normalize_slashes("plain/unix/path"), "plain/unix/path");
    }

    // ─── index_notes tests ──────────────────────────────────────────────────

    use crate::test_helpers::setup_store;

    /// Creates a notes file in the specified directory with the given content.
    ///
    /// # Arguments
    ///
    /// * `dir` - The directory path where the notes file will be created
    /// * `content` - The content to write to the notes.toml file
    ///
    /// # Returns
    ///
    /// Returns a `PathBuf` pointing to the created notes.toml file.
    ///
    /// # Panics
    ///
    /// Panics if the file write operation fails.
    fn make_notes_file(dir: &std::path::Path, content: &str) -> PathBuf {
        let path = dir.join("notes.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_index_notes_empty_returns_zero() {
        let (store, dir) = setup_store();
        let notes_path = make_notes_file(dir.path(), "# empty notes file\n");
        let notes: Vec<note::Note> = Vec::new();

        let count = index_notes(&notes, &notes_path, &store).unwrap();
        assert_eq!(count, 0);

        // Verify no notes in store
        let summaries = store.list_notes_summaries().unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_index_notes_stores_notes() {
        let (store, dir) = setup_store();
        let notes_path = make_notes_file(
            dir.path(),
            r#"
[[note]]
text = "Always use RRF search, not raw embedding"
sentiment = -0.5
mentions = ["search.rs"]

[[note]]
text = "Batch queries are fast"
sentiment = 0.5
mentions = ["store.rs"]
"#,
        );

        let notes = vec![
            note::Note {
                id: "note:0".to_string(),
                text: "Always use RRF search, not raw embedding".to_string(),
                sentiment: -0.5,
                mentions: vec!["search.rs".to_string()],
                kind: None,
            },
            note::Note {
                id: "note:1".to_string(),
                text: "Batch queries are fast".to_string(),
                sentiment: 0.5,
                mentions: vec!["store.rs".to_string()],
                kind: None,
            },
        ];

        let count = index_notes(&notes, &notes_path, &store).unwrap();
        assert_eq!(count, 2);

        // Verify notes are stored
        let summaries = store.list_notes_summaries().unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(
            summaries[0].text,
            "Always use RRF search, not raw embedding"
        );
        assert!((summaries[0].sentiment - (-0.5)).abs() < f32::EPSILON);
        assert_eq!(summaries[1].text, "Batch queries are fast");
    }

    #[test]
    fn test_index_notes_stores_note_sentiment() {
        let (store, dir) = setup_store();
        let notes_path = make_notes_file(dir.path(), "");

        let notes = vec![note::Note {
            id: "note:0".to_string(),
            text: "Serious issue with error handling".to_string(),
            sentiment: -1.0,
            mentions: vec!["lib.rs".to_string()],
            kind: None,
        }];

        let count = index_notes(&notes, &notes_path, &store).unwrap();
        assert_eq!(count, 1);

        // Verify the note is retrievable via list_notes_summaries
        let summaries = store.list_notes_summaries().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].text, "Serious issue with error handling");
        assert!((summaries[0].sentiment - (-1.0)).abs() < f32::EPSILON);
    }

    // ─── resolve_index_dir tests (TC-4) ──────────────────────────────────

    #[test]
    fn test_resolve_index_dir_only_legacy_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let legacy = dir.path().join(LEGACY_INDEX_DIR);
        std::fs::create_dir(&legacy).unwrap();

        let result = resolve_index_dir(dir.path());

        // Legacy .cq/ should have been renamed to .cqs/
        assert!(
            !legacy.exists(),
            ".cq/ should no longer exist after migration"
        );
        assert_eq!(result, dir.path().join(INDEX_DIR));
        assert!(result.exists(), ".cqs/ should exist after migration");
    }

    #[test]
    fn test_resolve_index_dir_both_exist() {
        let dir = tempfile::TempDir::new().unwrap();
        let legacy = dir.path().join(LEGACY_INDEX_DIR);
        let new = dir.path().join(INDEX_DIR);
        std::fs::create_dir(&legacy).unwrap();
        std::fs::create_dir(&new).unwrap();

        let result = resolve_index_dir(dir.path());

        // Both exist: should return .cqs/ without renaming (legacy stays)
        assert_eq!(result, new);
        assert!(legacy.exists(), ".cq/ should still exist when both present");
        assert!(new.exists(), ".cqs/ should still exist");
    }

    #[test]
    fn test_resolve_index_dir_neither_exists() {
        let dir = tempfile::TempDir::new().unwrap();

        let result = resolve_index_dir(dir.path());

        // Neither exists: should return .cqs/ path (not created, just the path)
        assert_eq!(result, dir.path().join(INDEX_DIR));
        assert!(
            !result.exists(),
            ".cqs/ should not be created, only returned as path"
        );
    }

    // ─── enumerate_files tests (TC-9) ────────────────────────────────────

    #[test]
    fn test_enumerate_files_finds_supported_extensions() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();

        // Create some Rust files
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn lib() {}").unwrap();
        // Create a non-Rust file (should be filtered out)
        std::fs::write(src.join("readme.txt"), "hello").unwrap();

        let files = enumerate_files(dir.path(), &["rs"], false).unwrap();

        assert_eq!(files.len(), 2, "Should find exactly 2 .rs files");
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"lib.rs".to_string()));
    }

    #[test]
    fn test_enumerate_files_respects_cqsignore() {
        // .cqsignore should exclude matching paths from indexing, layered on
        // top of .gitignore. Verifies the indexer respects the cqs-specific
        // ignore mechanism added for vendored bundles + similar artefacts.
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        let vendor = src.join("assets").join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();

        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(vendor.join("three.min.js"), "/* huge minified */").unwrap();
        std::fs::write(vendor.join("regular.js"), "console.log('ok');").unwrap();

        // Without .cqsignore: both .js files are visible.
        let before = enumerate_files(dir.path(), &["js"], false).unwrap();
        assert_eq!(
            before.len(),
            2,
            "expected both js files visible pre-cqsignore"
        );

        // With .cqsignore excluding *.min.js, only regular.js survives.
        std::fs::write(dir.path().join(".cqsignore"), "**/*.min.js\n").unwrap();
        let after = enumerate_files(dir.path(), &["js"], false).unwrap();
        let names: Vec<String> = after
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(after.len(), 1, "*.min.js should be filtered: got {names:?}");
        assert!(names.contains(&"regular.js".to_string()));

        // --no-ignore disables .cqsignore (same semantics as .gitignore).
        let bypassed = enumerate_files(dir.path(), &["js"], true).unwrap();
        assert_eq!(bypassed.len(), 2, "--no-ignore must also bypass .cqsignore");
    }

    #[test]
    fn test_enumerate_files_empty_for_unsupported() {
        let dir = tempfile::TempDir::new().unwrap();

        // Create files with unsupported extensions only
        std::fs::write(dir.path().join("notes.txt"), "some text").unwrap();
        std::fs::write(dir.path().join("data.csv"), "a,b,c").unwrap();

        let files = enumerate_files(dir.path(), &["rs", "py"], false).unwrap();

        assert!(
            files.is_empty(),
            "Should return empty for directory with no supported files"
        );
    }
    /// Verifies that the `is_test_chunk` function correctly identifies test files based on filename patterns.
    ///
    /// # Arguments
    ///
    /// This function takes no arguments.
    ///
    /// # Returns
    ///
    /// Returns nothing. This is a test function that validates the behavior of `is_test_chunk` through assertions.
    ///
    /// # Panics
    ///
    /// Panics if any assertion fails, indicating that `is_test_chunk` does not correctly identify test files or non-test files according to expected patterns.

    #[test]
    fn is_test_chunk_spec_patterns() {
        assert!(is_test_chunk("spec_helper", "src/spec_helper.rb"));
        assert!(is_test_chunk("user_spec", "spec/user_spec.rb"));
        assert!(is_test_chunk("normal_fn", "tests/test_main.py"));
        assert!(!is_test_chunk("inspector", "src/inspect.rs"));
    }

    // TC-6: _tests.rs suffix and nested /tests/ path
    #[test]
    fn is_test_chunk_tests_suffix_and_nested_path() {
        // File with _test suffix
        assert!(is_test_chunk("normal_fn", "src/search_test.rs"));
        assert!(is_test_chunk("normal_fn", "src/search_test.py"));
        // Nested /tests/ directory
        assert!(is_test_chunk("normal_fn", "src/store/tests/calls_test.rs"));
        assert!(is_test_chunk("normal_fn", "tests/integration.rs"));
        // .test. suffix (JS/TS)
        assert!(is_test_chunk("normal_fn", "src/search.test.ts"));
        assert!(is_test_chunk("normal_fn", "src/search.test.js"));
        // _test.go suffix
        assert!(is_test_chunk("normal_fn", "pkg/search_test.go"));
        // Should NOT match
        assert!(!is_test_chunk("normal_fn", "src/testing_utils.rs"));
        assert!(!is_test_chunk("normal_fn", "src/attest.rs"));
    }
}
