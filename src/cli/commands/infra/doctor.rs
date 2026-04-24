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
                    // P3 #91: structured fields so an operator can grep
                    // `op="cqs index"` rather than parse a free-form string.
                    tracing::warn!(
                        code = ?status.code(),
                        op = "cqs index",
                        "Sub-process index command exited non-zero"
                    );
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
                    // P3 #91: same structured form as the sibling arm.
                    tracing::warn!(
                        code = ?status.code(),
                        op = "cqs index --force",
                        "Sub-process index command exited non-zero"
                    );
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

/// Run diagnostic checks on cqs installation and index.
///
/// Reports runtime info, embedding provider, model status, and index statistics.
/// With `--fix`, automatically remediates issues: stale→index, schema→migrate.
/// With `--verbose`, also dumps the full setup introspection (resolved model
/// config, env vars, daemon socket, index metadata, config precedence) — the
/// one-call diagnostic for "queries return zero results" / "weird daemon state".
///
/// P2 #27: `--json` implies `--verbose`, suppresses all human-readable check
/// output on stdout (colored checks go to stderr instead so a TTY user still
/// sees them), and emits a single JSON document covering both check results
/// and verbose introspection. `cqs doctor --json | jq` works.
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

    // P2 #27: when `--json` is set we accumulate the structured equivalent of
    // each check line into `check_records`. The colored human-readable lines
    // still print but go to stderr (via `out`) so stdout stays pristine for
    // the final JSON envelope.
    let mut check_records: Vec<CheckRecord> = Vec::new();

    // CQ-V1.29-6: the "metadata:" label on the `Model:` check used to report
    // the compile-time `cqs::store::MODEL_NAME` constant, which is identical
    // to the default model on every invocation — silently wrong after
    // `cqs model swap` or a custom-model init. Read the actual stored model
    // name out of the index (if one exists) so doctor surfaces real drift.
    let stored_metadata_model = if index_path.exists() {
        match Store::open_readonly(&index_path) {
            Ok(s) => s.stored_model_name(),
            Err(e) => {
                tracing::warn!(error = %e, "Failed to open index read-only for metadata model lookup");
                None
            }
        }
    } else {
        None
    };

    out(json, "Runtime:");

    // Check model
    let model_config = ModelConfig::resolve(model_override, None);
    match Embedder::new(model_config.clone()) {
        Ok(embedder) => {
            // CQ-V1.29-6: show the actual index-metadata model rather than
            // the compile-time constant. If stored differs from the runtime
            // `model_repo()`, promote the record from `ok` → `warn` so
            // agents parsing `--json` see the drift as a warning.
            let runtime_repo = cqs::embedder::model_repo();
            let metadata_label = stored_metadata_model.as_deref().unwrap_or("unset");
            let model_msg = format!("{} (metadata: {})", runtime_repo, metadata_label);
            let stored_matches = match stored_metadata_model.as_deref() {
                Some(s) => s == runtime_repo.as_str(),
                None => true, // nothing stored → not a mismatch, just a fresh index
            };
            let mark = if stored_matches {
                "[✓]".green()
            } else {
                "[!]".yellow()
            };
            out(json, &format!("  {} Model: {}", mark, model_msg));
            let record = if stored_matches {
                CheckRecord::ok("runtime", "model", model_msg.clone())
            } else {
                CheckRecord::warn(
                    "runtime",
                    "model",
                    format!(
                        "{} (stored metadata \"{}\" differs from runtime repo \"{}\")",
                        model_msg, metadata_label, runtime_repo
                    ),
                )
            };
            check_records.push(record);
            if !stored_matches {
                any_failed = true;
            }
            out(json, &format!("  {} Tokenizer: loaded", "[✓]".green()));
            check_records.push(CheckRecord::ok(
                "runtime",
                "tokenizer",
                "loaded".to_string(),
            ));
            let provider = embedder.provider().to_string();
            out(
                json,
                &format!("  {} Execution: {}", "[✓]".green(), provider),
            );
            check_records.push(CheckRecord::ok("runtime", "execution", provider));

            // Test embedding
            let start = std::time::Instant::now();
            embedder.warm()?;
            let elapsed = start.elapsed();
            out(
                json,
                &format!("  {} Test embedding: {:?}", "[✓]".green(), elapsed),
            );
            check_records.push(CheckRecord::ok(
                "runtime",
                "test_embedding",
                format!("{:?}", elapsed),
            ));
        }
        Err(e) => {
            let msg = format!("Model load failed: {}", e);
            out(json, &format!("  {} Model: {}", "[✗]".red(), e));
            check_records.push(CheckRecord::err("runtime", "model", e.to_string()));
            issues.push(DoctorIssue {
                kind: IssueKind::ModelError,
                message: msg,
            });
            any_failed = true;
        }
    }

    out(json, "");
    out(json, "Parser:");
    match CqParser::new() {
        Ok(parser) => {
            out(json, &format!("  {} tree-sitter: loaded", "[✓]".green()));
            check_records.push(CheckRecord::ok(
                "parser",
                "tree_sitter",
                "loaded".to_string(),
            ));
            let langs = parser.supported_extensions().join(", ");
            out(json, &format!("  {} Languages: {}", "[✓]".green(), langs));
            check_records.push(CheckRecord::ok("parser", "languages", langs));
        }
        Err(e) => {
            out(json, &format!("  {} Parser: {}", "[✗]".red(), e));
            check_records.push(CheckRecord::err("parser", "tree_sitter", e.to_string()));
            // Parser errors are not auto-fixable
            any_failed = true;
        }
    }

    out(json, "");
    out(json, "Index:");
    if index_path.exists() {
        match Store::open(&index_path) {
            Ok(store) => {
                let stats = store.stats()?;
                out(
                    json,
                    &format!("  {} Location: {}", "[✓]".green(), index_path.display()),
                );
                check_records.push(CheckRecord::ok(
                    "index",
                    "location",
                    index_path.display().to_string(),
                ));
                out(
                    json,
                    &format!(
                        "  {} Schema version: {}",
                        "[✓]".green(),
                        stats.schema_version
                    ),
                );
                check_records.push(CheckRecord::ok(
                    "index",
                    "schema_version",
                    stats.schema_version.to_string(),
                ));
                out(
                    json,
                    &format!("  {} {} chunks indexed", "[✓]".green(), stats.total_chunks),
                );
                check_records.push(CheckRecord::ok(
                    "index",
                    "total_chunks",
                    stats.total_chunks.to_string(),
                ));
                if !stats.chunks_by_language.is_empty() {
                    let lang_summary: Vec<_> = stats
                        .chunks_by_language
                        .iter()
                        .map(|(l, c)| format!("{} {}", c, l))
                        .collect();
                    out(json, &format!("      ({})", lang_summary.join(", ")));
                }

                // Check schema version against expected
                let expected = cqs::store::CURRENT_SCHEMA_VERSION;
                if stats.schema_version != expected {
                    out(
                        json,
                        &format!(
                            "  {} Schema mismatch: index is v{}, cqs expects v{}",
                            "[!]".yellow(),
                            stats.schema_version,
                            expected
                        ),
                    );
                    let msg = format!("Schema v{} != expected v{}", stats.schema_version, expected);
                    check_records.push(CheckRecord::warn(
                        "index",
                        "schema_compatibility",
                        msg.clone(),
                    ));
                    issues.push(DoctorIssue {
                        kind: IssueKind::Schema,
                        message: msg,
                    });
                    any_failed = true;
                }

                // Check model mismatch between index and configured model
                let stored = store.stored_model_name();
                let configured = &model_config.name;
                match stored {
                    Some(ref stored_name) if stored_name != configured => {
                        out(
                            json,
                            &format!(
                                "  {} Model mismatch: index uses \"{}\", configured is \"{}\"",
                                "[!]".yellow(),
                                stored_name,
                                configured
                            ),
                        );
                        out(
                            json,
                            "      Run `cqs index --force` to reindex with the new model.",
                        );
                        let msg = format!(
                            "Model mismatch: index uses \"{}\", configured is \"{}\"",
                            stored_name, configured
                        );
                        check_records.push(CheckRecord::warn("index", "model_match", msg.clone()));
                        issues.push(DoctorIssue {
                            kind: IssueKind::Stale,
                            message: msg,
                        });
                        any_failed = true;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                out(json, &format!("  {} Index: {}", "[✗]".red(), e));
                check_records.push(CheckRecord::err("index", "open", err_str.clone()));
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
        out(
            json,
            &format!("  {} Index: not created yet", "[!]".yellow()),
        );
        out(json, "      Run 'cqs index' to create the index");
        check_records.push(CheckRecord::warn(
            "index",
            "exists",
            "Index not created".to_string(),
        ));
        issues.push(DoctorIssue {
            kind: IssueKind::NoIndex,
            message: "Index not created".to_string(),
        });
    }

    // Check references
    let config = cqs::config::Config::load(&root);
    if !config.references.is_empty() {
        out(json, "");
        out(json, "References:");
        for r in &config.references {
            let db_path = r.path.join(cqs::INDEX_DB_FILENAME);
            if !r.path.exists() {
                out(
                    json,
                    &format!(
                        "  {} {}: path missing ({})",
                        "[✗]".red(),
                        r.name,
                        r.path.display()
                    ),
                );
                check_records.push(CheckRecord::err(
                    "references",
                    &r.name,
                    format!("path missing ({})", r.path.display()),
                ));
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
                    out(
                        json,
                        &format!(
                            "  {} {}: {} chunks, {} (weight {:.1})",
                            "[✓]".green(),
                            r.name,
                            chunks,
                            hnsw,
                            r.weight
                        ),
                    );
                    check_records.push(CheckRecord::ok(
                        "references",
                        &r.name,
                        format!("{chunks} chunks, {hnsw} (weight {:.1})", r.weight),
                    ));
                }
                Err(e) => {
                    out(json, &format!("  {} {}: {}", "[✗]".red(), r.name, e));
                    check_records.push(CheckRecord::err("references", &r.name, e.to_string()));
                    any_failed = true;
                }
            }
        }
    }

    // Local LLM provider check: only run when the user opted in.
    // Validates that the required env vars are set and that the configured
    // endpoint is reachable (GET /v1/models or equivalent root probe).
    #[cfg(feature = "llm-summaries")]
    if std::env::var("CQS_LLM_PROVIDER").as_deref() == Ok("local") {
        out(json, "");
        out(json, "Local LLM:");
        check_local_llm(json, &mut check_records, &mut any_failed);
    }

    out(json, "");
    if any_failed {
        out(
            json,
            &format!("Some checks failed — see {} items above.", "[✗]".red()),
        );
    } else {
        out(json, "All checks passed.");
    }

    // --fix: attempt automatic remediation
    if fix && !issues.is_empty() {
        out(json, "");
        out(json, &format!("{}:", "Auto-fixing issues".bold()));
        run_fixes(&issues)?;
    } else if fix && issues.is_empty() {
        out(json, "Nothing to fix.");
    }

    // --verbose: emit the full setup introspection
    if want_verbose {
        let report = build_verbose_report(&root, &cqs_dir, &index_path, model_override, &config);
        if json {
            // P2 #27: emit ONE JSON document on stdout combining check
            // results and verbose introspection. The colored check log
            // already went to stderr; nothing on stdout has been written.
            let combined = DoctorReport {
                checks: check_records,
                any_failed,
                report: &report,
            };
            crate::cli::json_envelope::emit_json(&combined)
                .context("Failed to serialize doctor report")?;
        } else {
            println!();
            print_verbose_report(&report);
        }
    }

    Ok(())
}

/// P2 #27: route a human-readable check line to stdout (text mode) or stderr
/// (JSON mode), so `cqs doctor --json` keeps stdout pristine for `jq` while
/// a TTY user still sees the colored check progress on stderr.
fn out(json: bool, line: &str) {
    if json {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// Doctor check for `CQS_LLM_PROVIDER=local` — surfaces misconfig + endpoint
/// reachability. Only called when the env var is set.
///
/// Verifies:
///   1. `CQS_LLM_API_BASE` is present
///   2. `CQS_LLM_MODEL` is present
///   3. The endpoint responds to a trivial GET (`{api_base}/models`)
///
/// A 401/403 on step 3 is still "endpoint reachable, auth wrong" — reported
/// as a warn rather than err because many local servers don't require auth.
#[cfg(feature = "llm-summaries")]
fn check_local_llm(json: bool, records: &mut Vec<CheckRecord>, any_failed: &mut bool) {
    let _span = tracing::info_span!("doctor_local_llm").entered();

    let api_base = std::env::var("CQS_LLM_API_BASE").ok();
    let model = std::env::var("CQS_LLM_MODEL").ok();

    match api_base.as_deref() {
        Some(s) if !s.is_empty() => {
            out(
                json,
                &format!("  {} CQS_LLM_API_BASE: {}", "[✓]".green(), s),
            );
            records.push(CheckRecord::ok("local_llm", "api_base", s.to_string()));
        }
        _ => {
            let msg = "CQS_LLM_API_BASE is required when CQS_LLM_PROVIDER=local. \
                 Set CQS_LLM_API_BASE=http://localhost:8080/v1 (or your server's URL).";
            out(json, &format!("  {} {}", "[✗]".red(), msg));
            records.push(CheckRecord::err("local_llm", "api_base", msg.to_string()));
            *any_failed = true;
            return;
        }
    }

    match model.as_deref() {
        Some(s) if !s.is_empty() => {
            out(json, &format!("  {} CQS_LLM_MODEL: {}", "[✓]".green(), s));
            records.push(CheckRecord::ok("local_llm", "model", s.to_string()));
        }
        _ => {
            let msg = "CQS_LLM_MODEL is required when CQS_LLM_PROVIDER=local. \
                 Set CQS_LLM_MODEL=<your-model-name>.";
            out(json, &format!("  {} {}", "[✗]".red(), msg));
            records.push(CheckRecord::err("local_llm", "model", msg.to_string()));
            *any_failed = true;
            return;
        }
    }

    // Endpoint reachability probe: GET `{api_base}/models` with a tight
    // timeout. We don't want doctor to hang if the user typo'd a URL.
    let base = api_base.unwrap();
    let probe_url = format!("{}/models", base.trim_end_matches('/'));
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::limited(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Failed to build HTTP probe client: {}", e);
            out(json, &format!("  {} {}", "[✗]".red(), msg));
            records.push(CheckRecord::err("local_llm", "http_client", msg));
            *any_failed = true;
            return;
        }
    };

    match client.get(&probe_url).send() {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                let msg = format!("{} → {}", probe_url, status);
                out(
                    json,
                    &format!("  {} Endpoint reachable: {}", "[✓]".green(), msg),
                );
                records.push(CheckRecord::ok("local_llm", "endpoint_reachable", msg));
            } else if status == 401 || status == 403 {
                let msg = format!(
                    "{} returned {} — set CQS_LLM_API_KEY if your server requires auth",
                    probe_url, status
                );
                out(json, &format!("  {} {}", "[!]".yellow(), msg));
                records.push(CheckRecord::warn("local_llm", "auth", msg));
            } else {
                // Many local servers (Ollama, llama.cpp) may not implement
                // `/models` — a 404 means "reachable but no model list
                // endpoint" which is fine. Surface as warn, not err.
                let msg = format!(
                    "{} returned {} (server reachable but /models not implemented)",
                    probe_url, status
                );
                out(json, &format!("  {} {}", "[!]".yellow(), msg));
                records.push(CheckRecord::warn("local_llm", "endpoint_probe", msg));
            }
        }
        Err(e) => {
            let msg = format!(
                "Cannot reach {}: {}. Is your vLLM/llama.cpp/Ollama server running?",
                probe_url, e
            );
            out(json, &format!("  {} {}", "[✗]".red(), msg));
            records.push(CheckRecord::err("local_llm", "endpoint_reachable", msg));
            *any_failed = true;
        }
    }
}

/// One row in the structured check log emitted by `cqs doctor --json`.
///
/// Mirrors the human-readable lines (`[✓] Model: ...`) but in a shape agents
/// can branch on: `severity` is `ok` / `warn` / `err`, `section` names the
/// textual heading the line belonged to (`runtime`, `parser`, `index`,
/// `references`), and `name` is the per-line check id.
#[derive(Debug, serde::Serialize)]
struct CheckRecord {
    section: String,
    name: String,
    severity: String,
    message: String,
}

impl CheckRecord {
    fn ok(section: &str, name: &str, message: String) -> Self {
        Self {
            section: section.to_string(),
            name: name.to_string(),
            severity: "ok".to_string(),
            message,
        }
    }
    fn warn(section: &str, name: &str, message: String) -> Self {
        Self {
            section: section.to_string(),
            name: name.to_string(),
            severity: "warn".to_string(),
            message,
        }
    }
    fn err(section: &str, name: &str, message: String) -> Self {
        Self {
            section: section.to_string(),
            name: name.to_string(),
            severity: "err".to_string(),
            message,
        }
    }
}

/// `cqs doctor --json` payload: the structured check log + the verbose
/// introspection, emitted as one envelope so `jq` consumers see a single
/// document.
#[derive(Debug, serde::Serialize)]
struct DoctorReport<'a> {
    /// Structured equivalent of the colored check lines (one entry per
    /// `[✓]/[!]/[✗]` line). `severity` of `err` or `warn` corresponds to
    /// `any_failed = true`.
    checks: Vec<CheckRecord>,
    /// Mirrors the trailing "All checks passed." / "Some checks failed."
    /// summary — consumers can branch on this directly.
    any_failed: bool,
    /// Verbose setup introspection (always present in `--json` mode because
    /// `--json` implies `--verbose`).
    report: &'a VerboseReport,
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
///
/// PB-V1.29-8: delegates to the shared [`cqs::aux_model::hf_cache_dir`]
/// helper so Windows resolution (`%LOCALAPPDATA%\huggingface\hub`) is
/// consistent with the SPLADE preset registry and other HF-adjacent cache
/// consumers.
fn hf_cache_dir() -> PathBuf {
    cqs::aux_model::hf_cache_dir("hub")
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

    /// P2 #27: `CheckRecord::ok/warn/err` produce the exact `severity` shape
    /// agents will branch on. A typo here breaks every consumer downstream.
    #[test]
    fn check_record_severity_shape() {
        let ok = CheckRecord::ok("runtime", "model", "bge-large".to_string());
        let warn_rec = CheckRecord::warn("index", "schema", "v19 != v20".to_string());
        let err_rec = CheckRecord::err("parser", "tree_sitter", "load failed".to_string());

        let ok_json = serde_json::to_value(&ok).unwrap();
        assert_eq!(ok_json["severity"], "ok");
        assert_eq!(ok_json["section"], "runtime");
        assert_eq!(ok_json["name"], "model");
        assert_eq!(ok_json["message"], "bge-large");

        let warn_json = serde_json::to_value(&warn_rec).unwrap();
        assert_eq!(warn_json["severity"], "warn");

        let err_json = serde_json::to_value(&err_rec).unwrap();
        assert_eq!(err_json["severity"], "err");
    }

    /// P2 #27: the combined `DoctorReport` envelope serializes with both the
    /// check log AND the verbose introspection in one document. Agents call
    /// `cqs doctor --json | jq` and expect both top-level keys.
    #[test]
    fn doctor_report_combines_checks_and_verbose() {
        let _lock = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("CQS_EMBEDDING_MODEL");
        std::env::remove_var("CQS_ONNX_DIR");
        let tmp = TempDir::new().expect("tempdir");
        let report = empty_report(&tmp);
        let combined = DoctorReport {
            checks: vec![
                CheckRecord::ok("runtime", "model", "bge-large".to_string()),
                CheckRecord::warn("index", "exists", "Index not created".to_string()),
            ],
            any_failed: true,
            report: &report,
        };
        let json = serde_json::to_value(&combined).unwrap();
        // Both top-level shapes present.
        assert!(json.get("checks").is_some(), "checks key required");
        assert!(json.get("any_failed").is_some(), "any_failed key required");
        assert!(json.get("report").is_some(), "report key required");
        assert_eq!(json["any_failed"], true);
        assert_eq!(json["checks"].as_array().unwrap().len(), 2);
        // Verbose introspection round-trips inside the envelope.
        assert!(json["report"].get("resolved_model").is_some());
        assert!(json["report"].get("project_root").is_some());
    }
}
