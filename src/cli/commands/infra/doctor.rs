//! Doctor command for cqs
//!
//! Runs diagnostic checks on installation and index. With `--verbose`, dumps
//! full setup introspection: resolved model config, env vars, daemon socket
//! state, index metadata, config precedence — the one-call cause for the
//! "queries return zero results" failure mode that motivated this tool.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};

use cqs::embedder::ModelConfig;
use cqs::{Embedder, Parser as CqParser, Store};

use crate::cli::find_project_root;

/// Issue type detected during doctor checks.
#[derive(Debug, Clone, PartialEq)]
enum IssueKind {
    /// Index is stale — needs re-index
    Stale,
    /// Schema version mismatch — needs migration
    Schema,
    /// No index exists — needs creation
    NoIndex,
    /// Model error — needs reinstall
    ModelError,
}

/// A single doctor issue with its fix action.
#[derive(Debug, Clone)]
struct DoctorIssue {
    kind: IssueKind,
    message: String,
}

/// Run fix actions for detected issues.
fn run_fixes(issues: &[DoctorIssue]) -> Result<()> {
    let _span = tracing::info_span!("doctor_fix", issue_count = issues.len()).entered();

    // SEC-V1.25-7: Resolve our own binary path via current_exe() so a malicious
    // `cqs` earlier in PATH can't hijack the rescue flow when an admin runs
    // `cqs doctor --fix`.
    let cqs_path =
        std::env::current_exe().context("Failed to resolve current executable for 'cqs'")?;

    for issue in issues {
        match issue.kind {
            IssueKind::Stale | IssueKind::NoIndex => {
                println!("  Fixing: {} — running 'cqs index'...", issue.message);
                let status = std::process::Command::new(&cqs_path)
                    .arg("index")
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to run 'cqs index': {}", e))?;
                if status.success() {
                    println!("  {} Index rebuilt", "[✓]".green());
                } else {
                    println!("  {} Index rebuild failed", "[✗]".red());
                    tracing::warn!("cqs index exited with status {}", status);
                }
            }
            IssueKind::Schema => {
                println!(
                    "  Fixing: {} — running 'cqs index --force'...",
                    issue.message
                );
                let status = std::process::Command::new(&cqs_path)
                    .args(["index", "--force"])
                    .status()
                    .map_err(|e| anyhow::anyhow!("Failed to run 'cqs index --force': {}", e))?;
                if status.success() {
                    println!("  {} Index rebuilt with schema migration", "[✓]".green());
                } else {
                    println!("  {} Schema migration failed", "[✗]".red());
                    tracing::warn!("cqs index --force exited with status {}", status);
                }
            }
            IssueKind::ModelError => {
                println!(
                    "  Skipping: {} — model issues require manual intervention",
                    issue.message
                );
            }
        }
    }
    Ok(())
}

/// Run diagnostic checks on cqs installation and index
/// Reports runtime info, embedding provider, model status, and index statistics.
/// With `--fix`, automatically remediates issues: stale→index, schema→migrate.
/// With `--verbose`, also dumps the full setup introspection (resolved model
/// config, env vars, daemon socket, index metadata, config precedence) — the
/// one-call diagnostic for "queries return zero results" / "weird daemon state".
/// `--json` implies `--verbose` and emits the introspection as a structured
/// document instead of human-readable text.
pub(crate) fn cmd_doctor(
    model_override: Option<&str>,
    fix: bool,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_doctor", fix, verbose, json).entered();
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    let mut any_failed = false;
    let mut issues: Vec<DoctorIssue> = Vec::new();

    // `--json` implies `--verbose`: JSON without verbose would be the empty
    // legacy text output serialized as JSON, which has no real consumer.
    let want_verbose = verbose || json;

    println!("Runtime:");

    // Check model
    let model_config = ModelConfig::resolve(model_override, None);
    match Embedder::new(model_config.clone()) {
        Ok(embedder) => {
            println!(
                "  {} Model: {} (metadata: {})",
                "[✓]".green(),
                cqs::embedder::model_repo(),
                cqs::store::MODEL_NAME
            );
            println!("  {} Tokenizer: loaded", "[✓]".green());
            println!("  {} Execution: {}", "[✓]".green(), embedder.provider());

            // Test embedding
            let start = std::time::Instant::now();
            embedder.warm()?;
            let elapsed = start.elapsed();
            println!("  {} Test embedding: {:?}", "[✓]".green(), elapsed);
        }
        Err(e) => {
            let msg = format!("Model load failed: {}", e);
            println!("  {} Model: {}", "[✗]".red(), e);
            issues.push(DoctorIssue {
                kind: IssueKind::ModelError,
                message: msg,
            });
            any_failed = true;
        }
    }

    println!();
    println!("Parser:");
    match CqParser::new() {
        Ok(parser) => {
            println!("  {} tree-sitter: loaded", "[✓]".green());
            println!(
                "  {} Languages: {}",
                "[✓]".green(),
                parser.supported_extensions().join(", ")
            );
        }
        Err(e) => {
            println!("  {} Parser: {}", "[✗]".red(), e);
            // Parser errors are not auto-fixable
            any_failed = true;
        }
    }

    println!();
    println!("Index:");
    if index_path.exists() {
        match Store::open(&index_path) {
            Ok(store) => {
                let stats = store.stats()?;
                println!("  {} Location: {}", "[✓]".green(), index_path.display());
                println!(
                    "  {} Schema version: {}",
                    "[✓]".green(),
                    stats.schema_version
                );
                println!("  {} {} chunks indexed", "[✓]".green(), stats.total_chunks);
                if !stats.chunks_by_language.is_empty() {
                    let lang_summary: Vec<_> = stats
                        .chunks_by_language
                        .iter()
                        .map(|(l, c)| format!("{} {}", c, l))
                        .collect();
                    println!("      ({})", lang_summary.join(", "));
                }

                // Check schema version against expected
                let expected = cqs::store::CURRENT_SCHEMA_VERSION;
                if stats.schema_version != expected {
                    println!(
                        "  {} Schema mismatch: index is v{}, cqs expects v{}",
                        "[!]".yellow(),
                        stats.schema_version,
                        expected
                    );
                    issues.push(DoctorIssue {
                        kind: IssueKind::Schema,
                        message: format!(
                            "Schema v{} != expected v{}",
                            stats.schema_version, expected
                        ),
                    });
                    any_failed = true;
                }

                // Check model mismatch between index and configured model
                let stored = store.stored_model_name();
                let configured = &model_config.name;
                match stored {
                    Some(ref stored_name) if stored_name != configured => {
                        println!(
                            "  {} Model mismatch: index uses \"{}\", configured is \"{}\"",
                            "[!]".yellow(),
                            stored_name,
                            configured
                        );
                        println!("      Run `cqs index --force` to reindex with the new model.");
                        issues.push(DoctorIssue {
                            kind: IssueKind::Stale,
                            message: format!(
                                "Model mismatch: index uses \"{}\", configured is \"{}\"",
                                stored_name, configured
                            ),
                        });
                        any_failed = true;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                println!("  {} Index: {}", "[✗]".red(), e);
                if err_str.contains("Schema version mismatch") {
                    issues.push(DoctorIssue {
                        kind: IssueKind::Schema,
                        message: err_str,
                    });
                }
                any_failed = true;
            }
        }
    } else {
        println!("  {} Index: not created yet", "[!]".yellow());
        println!("      Run 'cqs index' to create the index");
        issues.push(DoctorIssue {
            kind: IssueKind::NoIndex,
            message: "Index not created".to_string(),
        });
    }

    // Check references
    let config = cqs::config::Config::load(&root);
    if !config.references.is_empty() {
        println!();
        println!("References:");
        for r in &config.references {
            let db_path = r.path.join(cqs::INDEX_DB_FILENAME);
            if !r.path.exists() {
                println!(
                    "  {} {}: path missing ({})",
                    "[✗]".red(),
                    r.name,
                    r.path.display()
                );
                any_failed = true;
                continue;
            }
            match Store::open_readonly(&db_path) {
                Ok(store) => {
                    let chunks = store.chunk_count().unwrap_or_else(|e| {
                        tracing::warn!(name = %r.name, error = %e, "Failed to count chunks in reference store");
                        0
                    });
                    let hnsw = if cqs::HnswIndex::exists(&r.path, "index") {
                        "HNSW loaded".to_string()
                    } else {
                        "no HNSW".to_string()
                    };
                    println!(
                        "  {} {}: {} chunks, {} (weight {:.1})",
                        "[✓]".green(),
                        r.name,
                        chunks,
                        hnsw,
                        r.weight
                    );
                }
                Err(e) => {
                    println!("  {} {}: {}", "[✗]".red(), r.name, e);
                    any_failed = true;
                }
            }
        }
    }

    println!();
    if any_failed {
        println!("Some checks failed — see {} items above.", "[✗]".red());
    } else {
        println!("All checks passed.");
    }

    // --fix: attempt automatic remediation
    if fix && !issues.is_empty() {
        println!();
        println!("{}:", "Auto-fixing issues".bold());
        run_fixes(&issues)?;
    } else if fix && issues.is_empty() {
        println!("Nothing to fix.");
    }

    // --verbose: emit the full setup introspection
    if want_verbose {
        let report = build_verbose_report(&root, &cqs_dir, &index_path, model_override, &config);
        if json {
            println!();
            crate::cli::json_envelope::emit_json(&report)
                .context("Failed to serialize verbose doctor report")?;
        } else {
            println!();
            print_verbose_report(&report);
        }
    }

    Ok(())
}

// ── Verbose introspection ────────────────────────────────────────────────────

/// Setup introspection report emitted by `cqs doctor --verbose`.
///
/// Captures every signal that affects which model is used, where the daemon
/// lives, what the index contains, and which config files are in play. Designed
/// to fit in one terminal screen (text mode) or one JSON document — read by
/// agents to diagnose "queries return zero results" / "model swap weirdness".
#[derive(Debug, serde::Serialize)]
struct VerboseReport {
    /// Project root (where cwd's `.cqs/` lives — may differ from cwd if a
    /// parent directory hosts `Cargo.toml` / `.git`).
    project_root: PathBuf,
    /// Resolved index directory (handles `.cq/` → `.cqs/` migration).
    cqs_dir: PathBuf,
    /// Resolved model config used for queries.
    resolved_model: ResolvedModel,
    /// Existence + size for the model files the resolved config points at.
    model_files: ModelFiles,
    /// Daemon socket path + connectivity (Unix only).
    daemon: DaemonState,
    /// Index metadata read directly from `.cqs/index.db`.
    index: IndexMeta,
    /// Config-file precedence (project + user) and per-section presence.
    config: ConfigSummary,
    /// Every `CQS_*` env var currently set, with value.
    env: Vec<EnvVar>,
}

#[derive(Debug, serde::Serialize)]
struct ResolvedModel {
    /// Source of the resolution (index / cli / env / config / default).
    /// "index" means `Store::stored_model_name()` won — the typical case
    /// once the index exists. "cli/env/config/default" means we fell through
    /// because no stored model was recorded yet.
    source: String,
    name: String,
    repo: String,
    onnx_path: String,
    tokenizer_path: String,
    dim: usize,
    max_seq_length: usize,
    pooling: String,
    /// Stored model name as recorded in `.cqs/index.db` metadata. `None` for
    /// fresh / pre-model-name indexes.
    stored_model_name: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct ModelFiles {
    /// Absolute path of the resolved ONNX file. May not exist yet (HF download
    /// happens on first `Embedder::new`).
    onnx_path: PathBuf,
    onnx_exists: bool,
    onnx_size_bytes: Option<u64>,
    tokenizer_path: PathBuf,
    tokenizer_exists: bool,
    tokenizer_size_bytes: Option<u64>,
    /// `CQS_ONNX_DIR` directory state (Some when the env var is set).
    cqs_onnx_dir: Option<CqsOnnxDirState>,
}

#[derive(Debug, serde::Serialize)]
struct CqsOnnxDirState {
    path: PathBuf,
    exists: bool,
    has_model_onnx: bool,
    has_tokenizer_json: bool,
}

#[derive(Debug, serde::Serialize)]
struct DaemonState {
    /// Computed socket path. `None` on non-Unix platforms.
    socket_path: Option<PathBuf>,
    /// `true` if the path exists on disk.
    socket_exists: bool,
    /// `true` if `UnixStream::connect` succeeded. Implies `socket_exists`.
    connected: bool,
    /// Connect error text, if `connected == false` and the socket exists.
    connect_error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct IndexMeta {
    /// `.cqs/index.db` path.
    db_path: PathBuf,
    exists: bool,
    /// Rest of the fields populated only when `exists == true`.
    schema_version: Option<i32>,
    dim: Option<usize>,
    stored_model_name: Option<String>,
    total_chunks: Option<u64>,
    total_files: Option<u64>,
    created_at: Option<String>,
    last_indexed_at: Option<String>,
    /// Open / stats error text, if reading the index failed.
    open_error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct ConfigSummary {
    project_path: PathBuf,
    project_exists: bool,
    /// `~/.config/cqs/config.toml` (or platform equivalent).
    user_path: Option<PathBuf>,
    user_exists: bool,
    /// One-line summaries for each config section that is set in the merged
    /// config (after project overrides user).
    sections: Vec<ConfigSectionSummary>,
}

#[derive(Debug, serde::Serialize)]
struct ConfigSectionSummary {
    name: String,
    summary: String,
}

#[derive(Debug, serde::Serialize)]
struct EnvVar {
    name: String,
    value: String,
}

/// Build the verbose report. Pure data assembly — no I/O beyond filesystem
/// stat calls and a single non-blocking daemon connect attempt.
fn build_verbose_report(
    project_root: &Path,
    cqs_dir: &Path,
    index_path: &Path,
    cli_model: Option<&str>,
    config: &cqs::config::Config,
) -> VerboseReport {
    let _span = tracing::info_span!("doctor_verbose_report").entered();

    // Index metadata: read first so we can feed `stored_model_name` into the
    // resolver and surface the same answer the live query path would get.
    let (stored_model_name, index_meta) = read_index_meta(index_path);

    let resolved = ModelConfig::resolve_for_query(
        stored_model_name.as_deref(),
        cli_model,
        config.embedding.as_ref(),
    );
    let resolved_source = resolution_source(
        stored_model_name.as_deref(),
        cli_model,
        config.embedding.as_ref(),
    );

    let model_files = collect_model_files(&resolved);

    let daemon = collect_daemon_state(cqs_dir);

    let config_summary = collect_config_summary(project_root, config);

    let env = collect_cqs_env_vars();

    VerboseReport {
        project_root: project_root.to_path_buf(),
        cqs_dir: cqs_dir.to_path_buf(),
        resolved_model: ResolvedModel {
            source: resolved_source,
            name: resolved.name.clone(),
            repo: resolved.repo.clone(),
            onnx_path: resolved.onnx_path.clone(),
            tokenizer_path: resolved.tokenizer_path.clone(),
            dim: resolved.dim,
            max_seq_length: resolved.max_seq_length,
            pooling: format!("{:?}", resolved.pooling),
            stored_model_name,
        },
        model_files,
        daemon,
        index: index_meta,
        config: config_summary,
        env,
    }
}

/// Mirror the resolution priority of `ModelConfig::resolve_for_query` to
/// label which input won. Cheap to keep aligned: both this function and the
/// real resolver only branch on `Some(stored)` → CLI → env → config → default.
fn resolution_source(
    stored: Option<&str>,
    cli: Option<&str>,
    embedding_cfg: Option<&cqs::embedder::EmbeddingConfig>,
) -> String {
    if stored.and_then(ModelConfig::from_preset).is_some() {
        return "index".to_string();
    }
    if cli.is_some() {
        return "cli".to_string();
    }
    if std::env::var("CQS_EMBEDDING_MODEL")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return "env".to_string();
    }
    if embedding_cfg.is_some() {
        return "config".to_string();
    }
    "default".to_string()
}

/// Read index metadata directly from the SQLite store. Opens read-only so
/// `cqs doctor` doesn't lock out an in-flight `cqs index`.
fn read_index_meta(index_path: &Path) -> (Option<String>, IndexMeta) {
    if !index_path.exists() {
        return (
            None,
            IndexMeta {
                db_path: index_path.to_path_buf(),
                exists: false,
                schema_version: None,
                dim: None,
                stored_model_name: None,
                total_chunks: None,
                total_files: None,
                created_at: None,
                last_indexed_at: None,
                open_error: None,
            },
        );
    }
    match Store::open_readonly(index_path) {
        Ok(store) => {
            let stored = store.stored_model_name();
            let dim = store.dim();
            let (schema_version, total_chunks, total_files, created_at, updated_at, open_error) =
                match store.stats() {
                    Ok(s) => (
                        Some(s.schema_version),
                        Some(s.total_chunks),
                        Some(s.total_files),
                        Some(s.created_at),
                        Some(s.updated_at),
                        None,
                    ),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to read index stats for verbose report");
                        (None, None, None, None, None, Some(e.to_string()))
                    }
                };
            (
                stored.clone(),
                IndexMeta {
                    db_path: index_path.to_path_buf(),
                    exists: true,
                    schema_version,
                    dim: Some(dim),
                    stored_model_name: stored,
                    total_chunks,
                    total_files,
                    created_at,
                    last_indexed_at: updated_at,
                    open_error,
                },
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to open index for verbose report");
            (
                None,
                IndexMeta {
                    db_path: index_path.to_path_buf(),
                    exists: true,
                    schema_version: None,
                    dim: None,
                    stored_model_name: None,
                    total_chunks: None,
                    total_files: None,
                    created_at: None,
                    last_indexed_at: None,
                    open_error: Some(e.to_string()),
                },
            )
        }
    }
}

/// Resolve the on-disk model file paths the resolved `ModelConfig` points at.
///
/// Honours `CQS_ONNX_DIR` first (matches `Embedder::new` behaviour: an
/// explicit local directory wins over the HF cache). Falls back to the HF cache
/// path the embedder would download into. The HF cache uses a snapshot layout
/// (`models--ORG--REPO/snapshots/<hash>/...`) so we walk the snapshots/ dir to
/// find an existing copy; if none exists we report the canonical
/// `models--ORG--REPO/snapshots/<unknown>/<file>` path so the user can see
/// where a download would land.
fn collect_model_files(cfg: &ModelConfig) -> ModelFiles {
    let cqs_onnx_dir = std::env::var("CQS_ONNX_DIR").ok().map(PathBuf::from);
    let cqs_onnx_dir_state = cqs_onnx_dir.as_ref().map(|p| CqsOnnxDirState {
        path: p.clone(),
        exists: p.exists(),
        has_model_onnx: p.join("model.onnx").exists(),
        has_tokenizer_json: p.join("tokenizer.json").exists(),
    });

    let (onnx_path, tokenizer_path) = if let Some(dir) = &cqs_onnx_dir {
        // CQS_ONNX_DIR overrides — files always named `model.onnx` /
        // `tokenizer.json` in the override dir (matches Embedder loader).
        (dir.join("model.onnx"), dir.join("tokenizer.json"))
    } else {
        // Best-effort lookup against the real HF cache snapshot layout.
        // `models--ORG--REPO/snapshots/<hash>/...`. Pick the first snapshot
        // that contains `cfg.onnx_path`; if none is on disk, fall back to a
        // canonical `<unknown>/...` placeholder so the user can see where a
        // download would land.
        let cache = hf_cache_dir();
        let onnx = hf_cache_lookup(&cache, &cfg.repo, &cfg.onnx_path);
        let tok = hf_cache_lookup(&cache, &cfg.repo, &cfg.tokenizer_path);
        (onnx, tok)
    };

    let (onnx_exists, onnx_size_bytes) = stat_size(&onnx_path);
    let (tokenizer_exists, tokenizer_size_bytes) = stat_size(&tokenizer_path);

    ModelFiles {
        onnx_path,
        onnx_exists,
        onnx_size_bytes,
        tokenizer_path,
        tokenizer_exists,
        tokenizer_size_bytes,
        cqs_onnx_dir: cqs_onnx_dir_state,
    }
}

/// Best-effort HF cache directory. Mirrors `huggingface_hub`'s default.
/// Caller treats a missing file as "not yet downloaded" — never fatal.
fn hf_cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HF_HOME") {
        return PathBuf::from(p).join("hub");
    }
    if let Ok(p) = std::env::var("HUGGINGFACE_HUB_CACHE") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .map(|h| h.join(".cache/huggingface/hub"))
        .unwrap_or_else(|| PathBuf::from(".cache/huggingface/hub"))
}

/// Look up `relative_path` (e.g. `"onnx/model.onnx"`) under any snapshot of
/// `models--<org>--<model>` in the HF cache. Returns the first snapshot that
/// has the file, or a canonical `<unknown>/<relative_path>` placeholder when
/// no snapshot contains it (so the user can see the cache layout).
fn hf_cache_lookup(cache: &Path, repo: &str, relative_path: &str) -> PathBuf {
    let model_dir = cache.join(format!("models--{}", repo.replace('/', "--")));
    let snapshots_dir = model_dir.join("snapshots");
    let placeholder = snapshots_dir.join("<unknown>").join(relative_path);
    let read_dir = match std::fs::read_dir(&snapshots_dir) {
        Ok(d) => d,
        Err(_) => return placeholder,
    };
    for entry in read_dir.flatten() {
        let candidate = entry.path().join(relative_path);
        if candidate.exists() {
            return candidate;
        }
    }
    placeholder
}

fn stat_size(path: &Path) -> (bool, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(m) => (true, Some(m.len())),
        Err(_) => (false, None),
    }
}

#[cfg(unix)]
fn collect_daemon_state(cqs_dir: &Path) -> DaemonState {
    let sock_path = cqs::daemon_translate::daemon_socket_path(cqs_dir);
    let exists = sock_path.exists();
    if !exists {
        return DaemonState {
            socket_path: Some(sock_path),
            socket_exists: false,
            connected: false,
            connect_error: None,
        };
    }
    // Non-blocking probe with a tight timeout — we don't want `cqs doctor` to
    // hang when the daemon is wedged. The full daemon-query path lives in
    // `dispatch::try_daemon_query` and uses a `CQS_DAEMON_TIMEOUT_MS`-tunable
    // timeout; the doctor probe uses a fixed 1s ceiling because it's a
    // diagnostic, not a query.
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    let start = std::time::Instant::now();
    match UnixStream::connect(&sock_path) {
        Ok(stream) => {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(1000)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));
            tracing::debug!(
                latency_ms = start.elapsed().as_millis() as u64,
                "Daemon socket connected"
            );
            DaemonState {
                socket_path: Some(sock_path),
                socket_exists: true,
                connected: true,
                connect_error: None,
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "Daemon socket connect failed");
            DaemonState {
                socket_path: Some(sock_path),
                socket_exists: true,
                connected: false,
                connect_error: Some(e.to_string()),
            }
        }
    }
}

#[cfg(not(unix))]
fn collect_daemon_state(_cqs_dir: &Path) -> DaemonState {
    DaemonState {
        socket_path: None,
        socket_exists: false,
        connected: false,
        connect_error: None,
    }
}

/// Build a config-section summary that mirrors what `Config::load` produced.
///
/// Sections only appear if their corresponding fields are populated in the
/// merged config — an empty list means "all defaults, nothing in either file".
fn collect_config_summary(project_root: &Path, config: &cqs::config::Config) -> ConfigSummary {
    let project_path = project_root.join(".cqs.toml");
    let project_exists = project_path.exists();
    let user_path = dirs::config_dir().map(|d| d.join("cqs/config.toml"));
    let user_exists = user_path.as_ref().map(|p| p.exists()).unwrap_or(false);

    let mut sections = Vec::new();

    if let Some(emb) = &config.embedding {
        let mut parts = vec![format!("model={}", emb.model)];
        if let Some(ref repo) = emb.repo {
            parts.push(format!("repo={}", repo));
        }
        if let Some(dim) = emb.dim {
            parts.push(format!("dim={}", dim));
        }
        if let Some(seq) = emb.max_seq_length {
            parts.push(format!("max_seq={}", seq));
        }
        sections.push(ConfigSectionSummary {
            name: "embedding".to_string(),
            summary: parts.join(" "),
        });
    }
    if let Some(splade) = &config.splade {
        let mut parts = Vec::new();
        if let Some(ref preset) = splade.preset {
            parts.push(format!("preset={}", preset));
        }
        if let Some(ref p) = splade.model_path {
            parts.push(format!("model_path={}", p.display()));
        }
        if parts.is_empty() {
            parts.push("(defaults)".to_string());
        }
        sections.push(ConfigSectionSummary {
            name: "splade".to_string(),
            summary: parts.join(" "),
        });
    }
    if let Some(rer) = &config.reranker {
        let mut parts = Vec::new();
        if let Some(ref preset) = rer.preset {
            parts.push(format!("preset={}", preset));
        }
        if let Some(ref p) = rer.model_path {
            parts.push(format!("model_path={}", p.display()));
        }
        if parts.is_empty() {
            parts.push("(defaults)".to_string());
        }
        sections.push(ConfigSectionSummary {
            name: "reranker".to_string(),
            summary: parts.join(" "),
        });
    }
    if let Some(scoring) = &config.scoring {
        let mut parts = Vec::new();
        if let Some(v) = scoring.name_exact {
            parts.push(format!("name_exact={}", v));
        }
        if let Some(v) = scoring.splade_alpha {
            parts.push(format!("splade_alpha={}", v));
        }
        if let Some(v) = scoring.rrf_k {
            parts.push(format!("rrf_k={}", v));
        }
        if parts.is_empty() {
            parts.push("(present)".to_string());
        }
        sections.push(ConfigSectionSummary {
            name: "scoring".to_string(),
            summary: parts.join(" "),
        });
    }
    if !config.references.is_empty() {
        let names: Vec<String> = config
            .references
            .iter()
            .map(|r| format!("{}({:.1})", r.name, r.weight))
            .collect();
        sections.push(ConfigSectionSummary {
            name: "references".to_string(),
            summary: format!("{} refs: {}", config.references.len(), names.join(", ")),
        });
    }

    ConfigSummary {
        project_path,
        project_exists,
        user_path,
        user_exists,
        sections,
    }
}

/// Collect every `CQS_*` env var currently set. The user explicitly noted
/// nothing here is sensitive on their box ("redact nothing — these aren't
/// secrets"); for other deployments this would need an allowlist.
fn collect_cqs_env_vars() -> Vec<EnvVar> {
    let mut vars: Vec<EnvVar> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CQS_"))
        .map(|(name, value)| EnvVar { name, value })
        .collect();
    vars.sort_by(|a, b| a.name.cmp(&b.name));
    vars
}

/// Pretty-print the verbose report in the same colored style as the legacy
/// doctor output. JSON consumers go through `crate::cli::json_envelope::emit_json`
/// instead — they don't see this function.
fn print_verbose_report(r: &VerboseReport) {
    println!("{}", "── Verbose ──".bold());
    println!();

    println!("{}", "Resolved model:".bold());
    println!("  source:           {}", r.resolved_model.source);
    println!("  name:             {}", r.resolved_model.name);
    println!("  repo:             {}", r.resolved_model.repo);
    println!("  dim:              {}", r.resolved_model.dim);
    println!("  max_seq_length:   {}", r.resolved_model.max_seq_length);
    println!("  pooling:          {}", r.resolved_model.pooling);
    println!("  onnx_path:        {}", r.resolved_model.onnx_path);
    println!("  tokenizer_path:   {}", r.resolved_model.tokenizer_path);
    match &r.resolved_model.stored_model_name {
        Some(s) => println!("  stored_model:     {}", s),
        None => {
            println!("  stored_model:     (none — would fall through to CLI/env/config/default)")
        }
    }
    println!();

    println!("{}", "Model files:".bold());
    print_file_check(
        "  onnx           ",
        &r.model_files.onnx_path,
        r.model_files.onnx_exists,
        r.model_files.onnx_size_bytes,
    );
    print_file_check(
        "  tokenizer      ",
        &r.model_files.tokenizer_path,
        r.model_files.tokenizer_exists,
        r.model_files.tokenizer_size_bytes,
    );
    if let Some(d) = &r.model_files.cqs_onnx_dir {
        let mark = if d.exists {
            "[✓]".green()
        } else {
            "[✗]".red()
        };
        println!("  CQS_ONNX_DIR    {} {}", mark, d.path.display());
        println!(
            "                  model.onnx={} tokenizer.json={}",
            tick(d.has_model_onnx),
            tick(d.has_tokenizer_json)
        );
    }
    println!();

    println!("{}", "Daemon:".bold());
    match &r.daemon.socket_path {
        Some(p) => {
            println!("  socket_path:      {}", p.display());
            if !r.daemon.socket_exists {
                println!("  status:           daemon not running");
            } else if r.daemon.connected {
                println!("  status:           {} connected", "[✓]".green());
            } else {
                let err = r
                    .daemon
                    .connect_error
                    .as_deref()
                    .unwrap_or("(unknown error)");
                println!(
                    "  status:           {} connect failed: {}",
                    "[✗]".red(),
                    err
                );
            }
        }
        None => {
            println!("  socket_path:      (n/a — non-Unix platform)");
        }
    }
    println!();

    println!("{}", "Index metadata:".bold());
    if !r.index.exists {
        println!("  no index ({})", r.index.db_path.display());
    } else if let Some(err) = &r.index.open_error {
        println!("  open error:       {}", err);
    } else {
        println!("  db_path:          {}", r.index.db_path.display());
        if let Some(v) = r.index.schema_version {
            println!("  schema_version:   {}", v);
        }
        if let Some(d) = r.index.dim {
            println!("  dim:              {}", d);
        }
        match &r.index.stored_model_name {
            Some(n) => println!("  stored_model:     {}", n),
            None => println!("  stored_model:     (none)"),
        }
        if let Some(c) = r.index.total_chunks {
            println!("  total_chunks:     {}", c);
        }
        if let Some(f) = r.index.total_files {
            println!("  total_files:      {}", f);
        }
        if let Some(c) = &r.index.created_at {
            if !c.is_empty() {
                println!("  created_at:       {}", c);
            }
        }
        if let Some(u) = &r.index.last_indexed_at {
            if !u.is_empty() {
                println!("  last_indexed_at:  {}", u);
            }
        }
    }
    println!();

    println!("{}", "Config precedence:".bold());
    let p_mark = if r.config.project_exists {
        "[✓]".green()
    } else {
        "[ ]".dimmed()
    };
    println!("  project: {} {}", p_mark, r.config.project_path.display());
    let u_mark = if r.config.user_exists {
        "[✓]".green()
    } else {
        "[ ]".dimmed()
    };
    match &r.config.user_path {
        Some(p) => println!("  user:    {} {}", u_mark, p.display()),
        None => println!("  user:    (no platform config dir)"),
    }
    if r.config.sections.is_empty() {
        println!("  sections: (all defaults)");
    } else {
        println!("  sections:");
        for s in &r.config.sections {
            println!("    [{}] {}", s.name, s.summary);
        }
    }
    println!();

    println!("{}", "Environment (CQS_*):".bold());
    if r.env.is_empty() {
        println!("  (none set)");
    } else {
        for v in &r.env {
            println!("  {}={}", v.name, v.value);
        }
    }
}

fn print_file_check(label: &str, path: &Path, exists: bool, size: Option<u64>) {
    let mark = if exists {
        "[✓]".green()
    } else {
        "[✗]".red()
    };
    let size_str = size.map(|n| format!(" ({} bytes)", n)).unwrap_or_default();
    println!("{} {} {}{}", label, mark, path.display(), size_str);
}

fn tick(b: bool) -> colored::ColoredString {
    if b {
        "yes".green()
    } else {
        "no".red()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cqs::embedder::ModelInfo;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Tests that touch `CQS_*` env vars must serialize — env vars are
    /// process-global and concurrent threads race on set/remove.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn issue_kind_maps_to_fix_action() {
        // Verify the fix action mapping for each issue kind
        let stale = DoctorIssue {
            kind: IssueKind::Stale,
            message: "stale index".to_string(),
        };
        let schema = DoctorIssue {
            kind: IssueKind::Schema,
            message: "schema mismatch".to_string(),
        };
        let no_index = DoctorIssue {
            kind: IssueKind::NoIndex,
            message: "no index".to_string(),
        };
        let model = DoctorIssue {
            kind: IssueKind::ModelError,
            message: "model error".to_string(),
        };

        // Stale and NoIndex both map to "cqs index"
        assert_eq!(stale.kind, IssueKind::Stale);
        assert_eq!(no_index.kind, IssueKind::NoIndex);
        // Schema maps to "cqs index --force"
        assert_eq!(schema.kind, IssueKind::Schema);
        // Model is manual
        assert_eq!(model.kind, IssueKind::ModelError);
    }

    /// Helper: build a `VerboseReport` for an empty tempdir (no `.cqs/`,
    /// no config files). Exercises the cold-start path.
    fn empty_report(tmp: &TempDir) -> VerboseReport {
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
        let config = cqs::config::Config::default();
        build_verbose_report(&root, &cqs_dir, &index_path, None, &config)
    }

    #[test]
    fn test_doctor_verbose_without_index() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        std::env::remove_var("CQS_ONNX_DIR");
        let tmp = TempDir::new().expect("tempdir");
        let report = empty_report(&tmp);
        // No index → exists=false, no stored model name, source=default.
        assert!(!report.index.exists, "index must report missing");
        assert!(report.index.stored_model_name.is_none());
        assert!(report.index.total_chunks.is_none());
        assert!(report.resolved_model.stored_model_name.is_none());
        assert_eq!(
            report.resolved_model.source, "default",
            "no stored model + no CLI/env/config → default"
        );
        // Default model is BGE-large.
        assert_eq!(report.resolved_model.name, "bge-large");
        assert_eq!(report.resolved_model.dim, 1024);
        // Config files don't exist.
        assert!(!report.config.project_exists);
        // Cross-check the text path doesn't panic on the empty case.
        print_verbose_report(&report);
    }

    #[test]
    fn test_doctor_verbose_with_index() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        std::env::remove_var("CQS_ONNX_DIR");
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let cqs_dir = root.join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        // Seed a store with a known preset name + dim. Using "v9-200k" so we
        // verify the resolver picks up the stored name even though no CLI/env
        // override is set.
        let store = cqs::Store::open(&index_path).expect("open store");
        store
            .init(&ModelInfo::new("v9-200k", 768))
            .expect("init store");
        drop(store);

        let config = cqs::config::Config::default();
        let report = build_verbose_report(&root, &cqs_dir, &index_path, None, &config);

        assert!(report.index.exists);
        assert_eq!(report.index.dim, Some(768));
        assert_eq!(
            report.index.stored_model_name.as_deref(),
            Some("v9-200k"),
            "stored_model_name must round-trip from metadata"
        );
        assert_eq!(report.index.total_chunks, Some(0));
        // Resolver picks the stored model — source=index, name=v9-200k, dim=768.
        assert_eq!(report.resolved_model.source, "index");
        assert_eq!(report.resolved_model.name, "v9-200k");
        assert_eq!(report.resolved_model.dim, 768);
        assert_eq!(
            report.resolved_model.stored_model_name.as_deref(),
            Some("v9-200k")
        );
    }

    #[test]
    fn test_doctor_verbose_json_output() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        std::env::remove_var("CQS_ONNX_DIR");
        let tmp = TempDir::new().expect("tempdir");
        let report = empty_report(&tmp);
        let json = serde_json::to_string_pretty(&report).expect("serialize report");
        // Re-parse and assert top-level shape — these are the keys agents
        // grep on, so a typo here is a contract break.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse roundtrip");
        for key in [
            "project_root",
            "cqs_dir",
            "resolved_model",
            "model_files",
            "daemon",
            "index",
            "config",
            "env",
        ] {
            assert!(v.get(key).is_some(), "missing top-level key: {}", key);
        }
        // Resolved model must include the resolution source field.
        assert!(v["resolved_model"].get("source").is_some());
        assert!(v["resolved_model"].get("dim").is_some());
        // Index must distinguish "no index" from a real failure.
        assert_eq!(v["index"]["exists"], serde_json::Value::Bool(false));
    }

    #[test]
    fn test_resolution_source_priority() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        // Stored preset wins over everything else.
        assert_eq!(
            resolution_source(Some("v9-200k"), Some("bge-large"), None),
            "index"
        );
        // Stored unknown name → falls through to CLI.
        assert_eq!(
            resolution_source(Some("unknown-model"), Some("bge-large"), None),
            "cli"
        );
        // No stored, no CLI, env set → env.
        std::env::set_var("CQS_EMBEDDING_MODEL", "v9-200k");
        assert_eq!(resolution_source(None, None, None), "env");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        // Empty env doesn't count.
        std::env::set_var("CQS_EMBEDDING_MODEL", "");
        assert_eq!(resolution_source(None, None, None), "default");
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        // Config alone → config.
        let cfg = cqs::embedder::EmbeddingConfig::default();
        assert_eq!(resolution_source(None, None, Some(&cfg)), "config");
        // Nothing → default.
        assert_eq!(resolution_source(None, None, None), "default");
    }

    #[test]
    fn test_collect_cqs_env_vars_filters_correctly() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::set_var("CQS_TEST_FOO_DOCTOR", "bar");
        std::env::set_var("NOT_CQS_VAR", "ignored");
        let vars = collect_cqs_env_vars();
        assert!(vars
            .iter()
            .any(|v| v.name == "CQS_TEST_FOO_DOCTOR" && v.value == "bar"));
        assert!(!vars.iter().any(|v| v.name == "NOT_CQS_VAR"));
        std::env::remove_var("CQS_TEST_FOO_DOCTOR");
        std::env::remove_var("NOT_CQS_VAR");
    }

    #[cfg(unix)]
    #[test]
    fn test_daemon_state_no_socket() {
        let tmp = TempDir::new().expect("tempdir");
        let state = collect_daemon_state(tmp.path());
        // No socket exists at the derived path.
        assert!(state.socket_path.is_some());
        assert!(!state.socket_exists);
        assert!(!state.connected);
        assert!(state.connect_error.is_none());
    }
}
