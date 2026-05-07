//! Reference index commands for cqs
//!
//! Manages reference indexes for multi-index search.
//! References are read-only indexes of external codebases.
//!
//! Core struct is [`RefListEntry`]; built in `cmd_ref_list`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use cqs::config::{add_reference_to_config, remove_reference_from_config, ReferenceConfig};
use cqs::reference;
use cqs::{ModelInfo, Parser as CqParser, Store};

use crate::cli::commands::index::build_hnsw_index;
use crate::cli::definitions::TextJsonArgs;
use crate::cli::{enumerate_files, find_project_root, run_index_pipeline, Cli};

// ---------------------------------------------------------------------------
// Output struct
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct RefListEntry {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub weight: f32,
    pub chunks: u64,
}

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// Reference subcommands
#[derive(clap::Subcommand)]
pub(crate) enum RefCommand {
    /// Add a reference index from an external codebase
    Add {
        /// Reference name (used in results and commands)
        name: String,
        /// Path to the source codebase to index
        source: PathBuf,
        /// Score weight multiplier (0.0-1.0, default 0.8)
        // AC-V1.29-5: bounded at parse time via `parse_unit_f32`. The
        // after-the-fact range check in `cmd_ref_add` still guards the
        // config-file loader path, so we keep belt-and-braces here.
        #[arg(long, default_value = "0.8", value_parser = crate::cli::definitions::parse_unit_f32)]
        weight: f32,
        /// API-V1.29-2: shared `--json` arg — without it, `cqs --json ref add`
        /// still printed colored text and broke downstream JSON parsers.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// List configured references
    List {
        /// API-V1.22-2: shared `--json` arg (was inline `json: bool`).
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Remove a reference index
    Remove {
        /// Name of the reference to remove
        name: String,
        /// API-V1.29-2: shared `--json` arg.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Update a reference index from its source.
    ///
    /// API-V1.36-3 (#1459): exposed as both `update` and the
    /// `reindex` visible alias so cross-command muscle memory
    /// transfers from `cqs index --force` (the project-side
    /// equivalent verb).
    ///
    /// #1459 item 3 (deep parity arm): mirrors `cqs index`'s
    /// `--llm-summaries` / `--improve-docs` / `--hyde-queries`
    /// flag set so ref refresh has the same enrichment surface
    /// as the project-side reindex. `--apply` (in-place doc
    /// rewrite of source files) is intentionally NOT exposed
    /// for refs — references typically point at vendored or
    /// external code, and silently writing back into someone
    /// else's tree is the wrong default. `--improve-docs`
    /// always writes proposed patches to
    /// `<ref-dir>/proposed-docs/` for review.
    #[command(visible_alias = "reindex")]
    Update {
        /// Name of the reference to update
        name: String,
        /// Generate LLM summaries for functions (requires ANTHROPIC_API_KEY)
        #[cfg(feature = "llm-summaries")]
        #[arg(long)]
        llm_summaries: bool,
        /// Generate doc comments for undocumented functions in this reference.
        ///
        /// Always writes proposed edits as unified-diff patches to
        /// `<ref-dir>/proposed-docs/<rel>.patch` — refs cannot use `--apply`
        /// (refs typically point at external code that must not be silently
        /// rewritten). Requires `--llm-summaries`.
        #[cfg(feature = "llm-summaries")]
        #[arg(long)]
        improve_docs: bool,
        /// Regenerate doc comments for all functions, even those with existing docs (requires --improve-docs)
        #[cfg(feature = "llm-summaries")]
        #[arg(long)]
        improve_all: bool,
        /// Maximum number of functions to generate docs for (used with --improve-docs)
        #[cfg(feature = "llm-summaries")]
        #[arg(long)]
        max_docs: Option<usize>,
        /// Generate hyde query predictions for functions (requires ANTHROPIC_API_KEY)
        #[cfg(feature = "llm-summaries")]
        #[arg(long)]
        hyde_queries: bool,
        /// Maximum number of functions to generate hyde predictions for
        #[cfg(feature = "llm-summaries")]
        #[arg(long)]
        max_hyde: Option<usize>,
        /// API-V1.29-2: shared `--json` arg.
        #[command(flatten)]
        output: TextJsonArgs,
    },
}

pub(crate) fn cmd_ref(cli: &Cli, subcmd: &RefCommand) -> Result<()> {
    let _span = tracing::info_span!("cmd_ref").entered();
    match subcmd {
        RefCommand::Add {
            name,
            source,
            weight,
            output,
        } => cmd_ref_add(cli, name, source, *weight, cli.json || output.json),
        RefCommand::List { output } => cmd_ref_list(cli, output.json),
        RefCommand::Remove { name, output } => cmd_ref_remove(name, cli.json || output.json),
        RefCommand::Update {
            name,
            #[cfg(feature = "llm-summaries")]
            llm_summaries,
            #[cfg(feature = "llm-summaries")]
            improve_docs,
            #[cfg(feature = "llm-summaries")]
            improve_all,
            #[cfg(feature = "llm-summaries")]
            max_docs,
            #[cfg(feature = "llm-summaries")]
            hyde_queries,
            #[cfg(feature = "llm-summaries")]
            max_hyde,
            output,
        } => {
            #[cfg(feature = "llm-summaries")]
            let opts = RefUpdateLlmOpts {
                llm_summaries: *llm_summaries,
                improve_docs: *improve_docs,
                improve_all: *improve_all,
                max_docs: *max_docs,
                hyde_queries: *hyde_queries,
                max_hyde: *max_hyde,
            };
            #[cfg(not(feature = "llm-summaries"))]
            let opts = RefUpdateLlmOpts;
            cmd_ref_update(cli, name, cli.json || output.json, opts)
        }
    }
}

/// #1459 item 3: opts bundle for `cqs ref reindex`'s LLM/HyDE flag parity
/// with `cqs index`. Empty marker struct under
/// `#[cfg(not(feature = "llm-summaries"))]` so the dispatcher signature
/// stays identical regardless of feature gate.
#[cfg(feature = "llm-summaries")]
struct RefUpdateLlmOpts {
    llm_summaries: bool,
    improve_docs: bool,
    improve_all: bool,
    max_docs: Option<usize>,
    hyde_queries: bool,
    max_hyde: Option<usize>,
}

#[cfg(not(feature = "llm-summaries"))]
struct RefUpdateLlmOpts;

/// Add a reference: validate name/weight, index source, update config.
/// * If the source path does not exist or cannot be resolved
/// * If the reference storage directory cannot be created
fn cmd_ref_add(
    cli: &Cli,
    name: &str,
    source: &std::path::Path,
    weight: f32,
    json: bool,
) -> Result<()> {
    // Validate name first — fast-fail before any I/O
    cqs::reference::validate_ref_name(name)
        .map_err(|e| anyhow::anyhow!("Invalid reference name '{}': {}", name, e))?;

    // Validate weight
    if !(0.0..=1.0).contains(&weight) {
        bail!("Weight must be between 0.0 and 1.0 (got {})", weight);
    }

    let root = find_project_root();
    let config = cqs::config::Config::load(&root);

    // Check for duplicate
    if config.references.iter().any(|r| r.name == name) {
        bail!(
            "Reference '{}' already exists. Use 'cqs ref update {}' to re-index.",
            name,
            name
        );
    }

    // Validate source
    let source_input = source.to_path_buf();
    let source = dunce::canonicalize(source)
        .map_err(|e| anyhow::anyhow!("Source path '{}' not found: {}", source.display(), e))?;

    // SEC-V1.30.1-6 (#1222): if `dunce::canonicalize` redirected the
    // user-supplied path through a symlink, surface it. The submitted
    // index will live at the *resolved* path; an operator who
    // symlinks `vendored-monorepo-pull/` → `~/work/customer-A-private/`
    // and runs `cqs ref add foo vendored-monorepo-pull/` deserves a
    // loud notice that they just indexed customer-A-private content.
    //
    // Comparison strategy: lexically normalize the absolute form of
    // the user input (resolve `..`, `.`, repeated separators without
    // touching the filesystem) and compare to the canonical path. A
    // mismatch means a symlink was followed somewhere in the chain.
    // Lexical normalization is intentionally cheap and conservative
    // — false positives are acceptable (the warning is informational)
    // but false negatives (silent symlink redirect) are not.
    let symlink_warning = match symlink_redirect_warning(&source_input, &source) {
        Ok(w) => w,
        Err(e) => {
            tracing::debug!(
                source = %source_input.display(),
                error = %e,
                "Could not compute absolute form of --source; skipping symlink-redirect check"
            );
            None
        }
    };
    if let Some(ref msg) = symlink_warning {
        tracing::warn!(
            user_source = %source_input.display(),
            resolved = %source.display(),
            "Source path resolved via symlink"
        );
        if !json && !cli.quiet {
            eprintln!("WARN: {msg}");
        }
    }

    // Create reference directory with restrictive permissions.
    // SEC-V1.30.1-9: walk every parent that `create_dir_all` may have
    // freshly created and chmod each to 0o700. Without this, the
    // `~/.local/share/cqs/refs/` chain inherits the user's umask
    // (typically 0o022 → 0o755), so `~/.local/share/cqs/refs/` itself
    // is world-readable and a co-located user can `ls` the names of
    // every reference index. The leaf `ref_dir` was already chmod-ed;
    // this extends the same guarantee one level up.
    let ref_dir = reference::ref_path(name)
        .ok_or_else(|| anyhow::anyhow!("Could not determine reference storage directory"))?;
    std::fs::create_dir_all(&ref_dir)
        .with_context(|| format!("Failed to create {}", ref_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&ref_dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::debug!(path = %ref_dir.display(), error = %e, "Failed to set file permissions");
        }
        // SEC-V1.30.1-9: also chmod `~/.local/share/cqs/refs/` so the
        // index *names* (one per ref subdir) aren't readable by other
        // users on a multi-user host.
        if let Some(refs_root) = ref_dir.parent() {
            if let Err(e) =
                std::fs::set_permissions(refs_root, std::fs::Permissions::from_mode(0o700))
            {
                tracing::debug!(
                    path = %refs_root.display(),
                    error = %e,
                    "Failed to chmod refs root to 0o700",
                );
            }
        }
    }
    let db_path = ref_dir.join(cqs::INDEX_DB_FILENAME);

    // Enumerate files
    let parser = CqParser::new()?;
    let files = enumerate_files(&source, &parser, false)?;

    if files.is_empty() {
        bail!("No supported source files found in '{}'", source.display());
    }

    if !cli.quiet && !json {
        println!(
            "Indexing {} files from '{}'...",
            files.len(),
            source.display()
        );
    }

    // Open store, initialize schema, and run indexing pipeline (shared Store via Arc)
    let store = Arc::new(
        Store::open(&db_path)
            .with_context(|| format!("Failed to open reference store at {}", db_path.display()))?,
    );
    let mc = cli.try_model_config()?;
    store.init(&ModelInfo::new(&mc.repo, mc.dim))?;
    let stats = run_index_pipeline(
        &source,
        files,
        Arc::clone(&store),
        false,
        cli.quiet,
        cli.try_model_config()?.clone(),
        // #1452: ref add does not run the LLM summary pass, so first-pass
        // embed is the only embed — never skip it for refs.
        false,
    )?;

    if !cli.quiet && !json {
        println!("  Embedded: {} chunks", stats.total_embedded);
    }

    // Build HNSW index
    if let Some(count) = build_hnsw_index(&store, &ref_dir)? {
        if !cli.quiet && !json {
            println!("  HNSW: {} vectors", count);
        }
    }

    // SEC-V1.30.1-10: chmod 0o600 on every file in `ref_dir` (DB, WAL,
    // SHM, HNSW snapshot). Mirrors the `cqs export-model` pattern for
    // `model.toml`. Without this, the per-user umask leaks the index
    // contents to other users on a multi-user host. Best-effort —
    // failures are logged at debug, not surfaced; the directory is
    // already 0o700 from the parent block, so file-mode failures
    // can't widen exposure beyond the per-user default.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(&ref_dir).into_iter().flatten().flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    let path = entry.path();
                    if let Err(e) =
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    {
                        tracing::debug!(
                            path = %path.display(),
                            error = %e,
                            "Failed to chmod reference file to 0o600",
                        );
                    }
                }
            }
        }
    }

    // Add to config
    let ref_config = ReferenceConfig {
        name: name.to_string(),
        path: ref_dir,
        source: Some(source),
        weight,
    };
    let config_path = root.join(".cqs.toml");
    add_reference_to_config(&config_path, &ref_config)?;

    if json {
        let mut payload = serde_json::json!({
            "status": "added",
            "name": name,
            "weight": weight,
        });
        if let Some(msg) = symlink_warning {
            payload
                .as_object_mut()
                .expect("payload is an object literal above")
                .insert("warnings".to_string(), serde_json::json!([msg]));
        }
        crate::cli::json_envelope::emit_json(&payload)?;
    } else if !cli.quiet {
        println!("Reference '{}' added.", name);
    }
    Ok(())
}

/// SEC-V1.30.1-6 (#1222): detect whether `dunce::canonicalize` redirected
/// `source_input` through a symlink. Returns `Ok(Some(message))` on
/// redirect, `Ok(None)` when the input lexically matches the canonical
/// path, and `Err` only when the absolute form of `source_input` cannot
/// be computed.
///
/// The lexical-normalize step resolves `..`, `.`, and duplicate
/// separators without touching the filesystem so that user input like
/// `/home/me/../me/projects/foo` doesn't trip a false positive. Symlink
/// resolution still happens via `dunce::canonicalize` upstream — this
/// helper only compares the result.
fn symlink_redirect_warning(
    source_input: &std::path::Path,
    canonical: &std::path::Path,
) -> std::io::Result<Option<String>> {
    let absolute = std::path::absolute(source_input)?;
    let normalized = lexical_normalize(&absolute);
    if normalized == canonical {
        Ok(None)
    } else {
        Ok(Some(format!(
            "source path '{}' resolved via symlink to '{}'",
            normalized.display(),
            canonical.display()
        )))
    }
}

/// Lexically normalize a path by resolving `..` and `.` components
/// without consulting the filesystem. Used by the symlink-redirect
/// check so that purely-syntactic differences in the user's input
/// (e.g. `./foo`, `bar/../foo`) do not look like symlink redirects.
fn lexical_normalize(p: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                // Pop only if the result already has a non-root tail;
                // popping at root keeps the path well-formed.
                if !out.pop() {
                    out.push(component);
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn cmd_ref_list(cli: &Cli, json: bool) -> Result<()> {
    let root = find_project_root();
    let config = cqs::config::Config::load(&root);
    let want_json = json || cli.json;

    if config.references.is_empty() {
        if want_json {
            // API-V1.29-2 + P2.15: emit `{references: []}` envelope so list
            // commands share a uniform `data.<plural>` accessor across
            // ref/model/project/slot/notes — matches the standard
            // documented in `docs/audit-fix-prompts.md::P2.15`.
            crate::cli::json_envelope::emit_json(&serde_json::json!({
                "references": Vec::<RefListEntry>::new(),
            }))?;
        } else {
            println!("No references configured.");
        }
        return Ok(());
    }

    if want_json {
        let refs: Vec<_> = config
            .references
            .iter()
            .map(|r| {
                let chunks = Store::open_readonly(&r.path.join(cqs::INDEX_DB_FILENAME))
                    .map_err(|e| {
                        tracing::warn!(
                            name = %r.name,
                            path = %r.path.display(),
                            error = %e,
                            "Failed to open reference store, showing 0 chunks"
                        );
                        e
                    })
                    .ok()
                    .and_then(|s| {
                        s.chunk_count().map_err(|e| {
                            tracing::warn!(name = %r.name, error = %e, "Failed to count chunks in reference store");
                        }).ok()
                    })
                    .unwrap_or(0);
                RefListEntry {
                    name: r.name.clone(),
                    path: cqs::normalize_path(&r.path),
                    source: r.source.as_ref().map(|p| cqs::normalize_path(p)),
                    weight: r.weight,
                    chunks,
                }
            })
            .collect();
        // P2.15: wrap in `{references: [...]}` so the list shape matches
        // slot/project/notes envelopes.
        crate::cli::json_envelope::emit_json(&serde_json::json!({
            "references": refs,
        }))?;
        return Ok(());
    }

    println!("{:<15} {:<8} {:<10} SOURCE", "NAME", "WEIGHT", "CHUNKS");
    println!("{}", "─".repeat(60));

    for r in &config.references {
        let chunks = Store::open(&r.path.join(cqs::INDEX_DB_FILENAME))
            .map_err(|e| {
                tracing::warn!(
                    name = %r.name,
                    path = %r.path.display(),
                    error = %e,
                    "Failed to open reference store, showing 0 chunks"
                );
                e
            })
            .ok()
            .and_then(|s| {
                s.chunk_count().map_err(|e| {
                    tracing::warn!(name = %r.name, error = %e, "Failed to count chunks in reference store");
                }).ok()
            })
            .unwrap_or(0);
        let source_str = r
            .source
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(none)".to_string());
        println!(
            "{:<15} {:<8.2} {:<10} {}",
            r.name, r.weight, chunks, source_str
        );
    }

    Ok(())
}

/// Remove a reference: delete from config and remove its directory.
fn cmd_ref_remove(name: &str, json: bool) -> Result<()> {
    let root = find_project_root();
    let config_path = root.join(".cqs.toml");
    let removed = remove_reference_from_config(&config_path, name)?;

    if !removed {
        // API-V1.29-2: in JSON mode, surface `not_found` as a structured
        // envelope error instead of an anyhow bail that would serialize as
        // plain text on stderr.
        if json {
            crate::cli::json_envelope::emit_json_error(
                crate::cli::json_envelope::error_codes::NOT_FOUND,
                &format!("Reference '{}' not found in config.", name),
            )?;
            return Ok(());
        }
        bail!("Reference '{}' not found in config.", name);
    }

    // Delete reference directory — only via canonical ref_path() to prevent
    // config-supplied paths from deleting arbitrary directories
    if let Some(refs_root) = reference::refs_dir() {
        let ref_dir = refs_root.join(name);
        if ref_dir.exists() {
            // Verify the path is actually inside the refs directory
            if let (Ok(canonical_dir), Ok(canonical_root)) = (
                dunce::canonicalize(&ref_dir),
                dunce::canonicalize(&refs_root),
            ) {
                if canonical_dir.starts_with(&canonical_root) {
                    std::fs::remove_dir_all(&canonical_dir)
                        .with_context(|| format!("Failed to remove {}", ref_dir.display()))?;
                } else {
                    tracing::warn!(
                        path = %canonical_dir.display(),
                        "Refusing to delete reference directory outside refs root"
                    );
                }
            }
        }
    }

    if json {
        crate::cli::json_envelope::emit_json(&serde_json::json!({
            "status": "removed",
            "name": name,
        }))?;
    } else {
        println!("Reference '{}' removed.", name);
    }
    Ok(())
}

/// Re-index a reference from its source directory.
fn cmd_ref_update(cli: &Cli, name: &str, json: bool, opts: RefUpdateLlmOpts) -> Result<()> {
    // #1459 item 3: enforce flag-dependency invariants up front so misuse
    // bails before the (potentially long) index pipeline runs. Mirrors
    // `cmd_index`'s pre-flight at `src/cli/commands/index/build.rs`.
    #[cfg(feature = "llm-summaries")]
    {
        if opts.improve_docs && !opts.llm_summaries {
            anyhow::bail!("--improve-docs requires --llm-summaries");
        }
        if opts.improve_all && !opts.improve_docs {
            anyhow::bail!("--improve-all requires --improve-docs");
        }
    }

    let root = find_project_root();
    let config = cqs::config::Config::load(&root);

    let ref_config = match config.references.iter().find(|r| r.name == name) {
        Some(r) => r,
        None => {
            // API-V1.29-2: structured envelope error in JSON mode.
            if json {
                crate::cli::json_envelope::emit_json_error(
                    crate::cli::json_envelope::error_codes::NOT_FOUND,
                    &format!("Reference '{}' not found in config.", name),
                )?;
                return Ok(());
            }
            return Err(anyhow::anyhow!("Reference '{}' not found in config.", name));
        }
    };

    let source = ref_config
        .source
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Reference '{}' has no source path configured.", name))?;

    if !source.exists() {
        bail!(
            "Source path '{}' does not exist. Update the config or remove and re-add the reference.",
            source.display()
        );
    }

    // SEC-V1.38-4 (#1463): mirror cmd_ref_add's symlink-redirect warn so
    // an operator who pointed `vendored-deps/ → ~/work/customer-A-private/`
    // after the original `cqs ref add` sees a loud notice on the next
    // update / reindex. The check is cheap (lexical normalize +
    // canonicalize) and surfaces the same threat — vendored content
    // swap — that SEC-V1.30.1-6 was filed for.
    if let Ok(canonical) = dunce::canonicalize(source) {
        match symlink_redirect_warning(source, &canonical) {
            Ok(Some(msg)) => {
                tracing::warn!(
                    name = %name,
                    user_source = %source.display(),
                    resolved = %canonical.display(),
                    "Source path resolved via symlink during ref update — re-indexing the resolved target"
                );
                if !json && !cli.quiet {
                    eprintln!("WARN: {msg}");
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(
                    source = %source.display(),
                    error = %e,
                    "Could not compute absolute form of source for symlink-redirect check during update"
                );
            }
        }
    }

    let db_path = ref_config.path.join(cqs::INDEX_DB_FILENAME);
    let ref_dir = &ref_config.path;

    // Get current chunk count before modifying anything
    let existing_chunks = if db_path.exists() {
        Store::open(&db_path)
            .map_err(|e| {
                tracing::warn!(error = %e, "Failed to open reference store for chunk count");
            })
            .ok()
            .and_then(|s| {
                s.chunk_count()
                    .map_err(|e| {
                        tracing::warn!(error = %e, "Failed to count chunks in reference store");
                    })
                    .ok()
            })
            .unwrap_or(0)
    } else {
        0
    };

    // Enumerate files
    let parser = CqParser::new()?;
    let files = enumerate_files(source, &parser, false)?;

    // Guard: if the binary finds 0 files but the index has chunks, abort.
    // This happens when the binary doesn't support languages in the index.
    if files.is_empty() && existing_chunks > 0 {
        bail!(
            "No supported files found in '{}', but the index has {} chunks.\n\
             This usually means the binary doesn't support the language(s) in the index.\n\
             Rebuild with a binary that supports the required languages, or use \
             'cqs ref remove {name}' and re-add.",
            source.display(),
            existing_chunks,
        );
    }

    if !cli.quiet && !json {
        println!("Updating reference '{}' ({} files)...", name, files.len());
    }

    // Open store and run incremental indexing pipeline (shared Store via Arc)
    let store = Arc::new(
        Store::open(&db_path)
            .with_context(|| format!("Failed to open reference store at {}", db_path.display()))?,
    );
    let stats = run_index_pipeline(
        source,
        files.clone(),
        Arc::clone(&store),
        false,
        cli.quiet,
        cli.try_model_config()?.clone(),
        // #1452: ref update does not run the LLM summary pass, so
        // first-pass embed is the only embed — never skip it for refs.
        false,
    )?;

    if !cli.quiet && !json {
        let newly = stats.total_embedded - stats.total_cached;
        println!(
            "  Chunks: {} ({} cached, {} embedded)",
            stats.total_embedded, stats.total_cached, newly
        );
    }

    // Prune chunks for deleted files
    let existing_files: HashSet<_> = files.into_iter().collect();
    let pruned = store.prune_missing(&existing_files, source)?;

    // Guard: if pruning would remove >50% of existing chunks, warn loudly
    if pruned > 0 && existing_chunks > 0 {
        let remaining = existing_chunks.saturating_sub(pruned as u64);
        if remaining == 0 {
            tracing::warn!(
                pruned,
                name,
                "All chunks were pruned. The index is now empty. \
                 If this was unintentional, re-index with 'cqs ref update'.",
            );
        } else if (pruned as u64) > existing_chunks / 2 {
            tracing::warn!(
                pruned,
                existing_chunks,
                pct = (pruned as f64 / existing_chunks as f64) * 100.0,
                "Pruned over 50% of chunks. Verify source path is correct.",
            );
        }
    }

    if !cli.quiet && !json && pruned > 0 {
        println!("  Pruned: {} (deleted files)", pruned);
    }

    // #1459 item 3 (deep parity arm): mirror `cmd_index`'s post-pipeline
    // LLM / doc-comment / HyDE / enrichment passes against the ref store.
    // Passes are gated behind their own flags so the default `cqs ref
    // reindex` behavior is byte-identical to the pre-#1505 path.
    //
    // Rationale for matching `cmd_index`:
    //   - LLM summaries are keyed by content_hash and shared across
    //     stores, so a `--llm-summaries` ref reindex populates the same
    //     cache that the project-side index reads. No recomputation cost
    //     when both run for the same source.
    //   - enrichment_pass is the gate that clears `needs_embedding=1`
    //     rows; with `--llm-summaries` we skip the first-pass embed in
    //     the pipeline (#1452), so without enrichment the ref index would
    //     ship full of zero-vec sentinels.
    //   - `--improve-docs` writes patches to `<ref-dir>/proposed-docs/`,
    //     never directly to source — refs typically point at vendored
    //     code that must not be silently rewritten. `--apply` is not
    //     wired through for `cqs ref reindex` for the same reason.
    #[cfg(feature = "llm-summaries")]
    if !crate::cli::check_interrupted() && opts.llm_summaries {
        if !cli.quiet && !json {
            println!("Generating LLM summaries...");
        }
        match cqs::llm::llm_summary_pass(&store, cli.quiet || json, &config, Some(ref_dir)) {
            Ok(count) => {
                if !cli.quiet && !json && count > 0 {
                    println!("  LLM summaries: {} new", count);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "LLM summary pass failed for ref; continuing");
            }
        }
    }

    #[cfg(feature = "llm-summaries")]
    if !crate::cli::check_interrupted() && opts.improve_docs {
        if !cli.quiet && !json {
            println!("Generating doc comments...");
        }
        match cqs::llm::doc_comment_pass(
            &store,
            &config,
            opts.max_docs.unwrap_or(0),
            opts.improve_all,
            Some(ref_dir),
        ) {
            Ok(doc_results) => {
                if !doc_results.is_empty() {
                    use std::collections::HashMap;
                    let mut by_file: HashMap<std::path::PathBuf, Vec<_>> = HashMap::new();
                    for r in doc_results {
                        by_file.entry(r.file.clone()).or_default().push(r);
                    }
                    let doc_parser = CqParser::new()?;
                    let patch_dir = ref_dir.join("proposed-docs");
                    let mut written = 0usize;
                    let mut skipped = 0usize;
                    for (path, edits) in &by_file {
                        match cqs::doc_writer::rewriter::write_proposed_patch(
                            path,
                            source,
                            edits,
                            &doc_parser,
                            &patch_dir,
                        ) {
                            Ok(true) => written += 1,
                            Ok(false) => skipped += 1,
                            Err(e) => tracing::warn!(
                                file = %path.display(),
                                error = %e,
                                "Doc patch write failed"
                            ),
                        }
                    }
                    if !cli.quiet && !json {
                        if written > 0 {
                            println!(
                                "  Doc comments: {} proposed update(s) written to {}",
                                written,
                                patch_dir.display()
                            );
                        } else if skipped > 0 {
                            println!(
                                "  Doc comments: {} candidate file(s) produced no diff",
                                skipped
                            );
                        }
                    }
                } else if !cli.quiet && !json {
                    println!("  Doc comments: 0 candidates");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Doc comment pass failed for ref; continuing");
            }
        }
    }

    #[cfg(feature = "llm-summaries")]
    if !crate::cli::check_interrupted() && opts.hyde_queries {
        if !cli.quiet && !json {
            println!("Generating hyde query predictions...");
        }
        match cqs::llm::hyde_query_pass(
            &store,
            cli.quiet || json,
            &config,
            opts.max_hyde.unwrap_or(0),
            Some(ref_dir),
        ) {
            Ok(count) => {
                if !cli.quiet && !json && count > 0 {
                    println!("  Hyde predictions: {} new", count);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Hyde query pass failed for ref; continuing");
            }
        }
    }

    // enrichment_pass: clears `needs_embedding=1` rows that
    // `--llm-summaries` left behind by skipping the first-pass embed,
    // and re-embeds chunks with caller/callee context when call edges
    // were extracted. Mirror `cmd_index`'s trigger: any unembedded
    // chunks OR any call edges → run.
    let needs_embed_count = match store.needs_embedding_count() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count needs_embedding chunks for ref; assuming zero");
            0
        }
    };
    if !crate::cli::check_interrupted() && (stats.total_calls > 0 || needs_embed_count > 0) {
        use crate::cli::enrichment_pass;
        if !cli.quiet && !json {
            if needs_embed_count > 0 {
                println!(
                    "Enriching embeddings with call graph context ({} chunks awaiting first embedding)...",
                    needs_embed_count
                );
            } else {
                println!("Enriching embeddings with call graph context...");
            }
        }
        let model_config = cli.try_model_config()?.clone();
        match cqs::Embedder::new(model_config.clone()) {
            Ok(embedder) => {
                match enrichment_pass(&store, &embedder, &model_config, cli.quiet || json) {
                    Ok(count) => {
                        if !cli.quiet && !json && count > 0 {
                            println!("  Enriched: {} chunks", count);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Enrichment pass failed for ref; continuing");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create embedder for ref enrichment");
            }
        }
    }

    // Belt-and-braces summary queue flush mirrors cmd_index.
    #[cfg(feature = "llm-summaries")]
    if let Err(e) = store.flush_pending_summaries() {
        tracing::warn!(error = %e, "cmd_ref_update: final flush of summary queue failed");
    }

    // Rebuild HNSW
    if let Some(count) = build_hnsw_index(&store, ref_dir)? {
        if !cli.quiet && !json {
            println!("  HNSW: {} vectors", count);
        }
    }

    if json {
        crate::cli::json_envelope::emit_json(&serde_json::json!({
            "status": "updated",
            "name": name,
            "weight": ref_config.weight,
            "chunks": stats.total_embedded,
            "pruned": pruned,
        }))?;
    } else if !cli.quiet {
        println!("Reference '{}' updated.", name);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ref_list_entry_serialization() {
        let entry = RefListEntry {
            name: "stdlib".into(),
            path: "/home/user/.cqs/refs/stdlib".into(),
            source: Some("/usr/src/rust/library".into()),
            weight: 0.8,
            chunks: 1234,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["name"], "stdlib");
        assert_eq!(json["path"], "/home/user/.cqs/refs/stdlib");
        assert_eq!(json["source"], "/usr/src/rust/library");
        assert_eq!(json["weight"], 0.8f32 as f64);
        assert_eq!(json["chunks"], 1234);
    }

    #[test]
    fn test_ref_list_entry_no_source() {
        let entry = RefListEntry {
            name: "external".into(),
            path: "/home/user/.cqs/refs/external".into(),
            source: None,
            weight: 0.5,
            chunks: 0,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("source").is_none());
        assert_eq!(json["chunks"], 0);
    }

    // SEC-V1.30.1-6 (#1222) — symlink-redirect detection.

    #[test]
    fn lexical_normalize_resolves_dot_and_dotdot() {
        let cases = [
            ("/a/b/./c", "/a/b/c"),
            ("/a/b/../c", "/a/c"),
            ("/a/./b/../c", "/a/c"),
            ("/a/b/c/../..", "/a"),
            ("/a", "/a"),
            ("/", "/"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                lexical_normalize(std::path::Path::new(input)),
                std::path::PathBuf::from(expected),
                "lexical_normalize({input}) should be {expected}"
            );
        }
    }

    #[test]
    fn symlink_redirect_warning_returns_none_for_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("mkdir real");
        let canonical = dunce::canonicalize(&real).expect("canonicalize real");

        // User passed the real path; no redirect.
        let warning = symlink_redirect_warning(&real, &canonical).expect("warn ok");
        assert!(
            warning.is_none(),
            "no symlink → no warning, got {warning:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_redirect_warning_fires_on_symlinked_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("mkdir real");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let canonical = dunce::canonicalize(&link).expect("canonicalize link");
        let warning = symlink_redirect_warning(&link, &canonical).expect("warn ok");
        let msg = warning.expect("symlink should produce a warning");
        assert!(
            msg.contains("symlink"),
            "warning text should mention symlink: {msg}"
        );
        assert!(
            msg.contains(real.to_str().unwrap()),
            "warning should name the resolved target: {msg}"
        );
    }

    #[test]
    fn symlink_redirect_warning_ignores_purely_syntactic_dotdot() {
        // User typed `<dir>/sub/../sub/` — lexically equivalent to
        // `<dir>/sub`, no symlink involved. This must not warn.
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("sub");
        std::fs::create_dir(&real).expect("mkdir sub");
        let weird_input = dir.path().join("sub").join("..").join("sub");
        let canonical = dunce::canonicalize(&weird_input).expect("canonicalize");

        let warning = symlink_redirect_warning(&weird_input, &canonical).expect("warn ok");
        assert!(
            warning.is_none(),
            "purely syntactic `..` must not look like a symlink, got {warning:?}"
        );
    }
}
