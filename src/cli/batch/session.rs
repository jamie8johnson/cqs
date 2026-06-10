//! Batch session entry points: `create_context`, `cmd_batch` (the stdin
//! JSONL line-loop), and the test-only context factory.
//!
//! Split out of the former monolithic `cli/batch/mod.rs` (issue #1691).

use super::*;

// ─── Main loop ───────────────────────────────────────────────────────────────

/// Create a shared batch context: open store, prepare lazy caches.
///
/// Used by both `cmd_batch` and `cmd_chat`.
pub(crate) fn create_context() -> Result<BatchContext> {
    create_context_with_runtime(None)
}

/// Variant that reuses a caller-supplied tokio runtime so the daemon
/// (`watch_and_serve`) can build one `Arc<Runtime>` at process start and hand
/// the same handle to both its outer read-write Store and the batch context's
/// read-only Store. Subsequent `EmbeddingCache` / `QueryCache` opens through
/// [`BatchContext::warm`] pick up the same runtime via [`cqs::Store::runtime`].
/// When `runtime` is `None`, constructs its own current-thread runtime for the
/// read-only Store.
pub(crate) fn create_context_with_runtime(
    runtime: Option<std::sync::Arc<tokio::runtime::Runtime>>,
) -> Result<BatchContext> {
    let root = crate::cli::config::find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs::resolve_index_db(&cqs_dir);
    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }
    let store = if let Some(rt) = runtime {
        Store::open_readonly_pooled_with_runtime(&index_path, rt).map_err(|e| {
            anyhow::anyhow!("Failed to open index at {}: {}", index_path.display(), e)
        })?
    } else {
        let (s, _root, _cqs_dir) = open_project_store_readonly()?;
        s
    };
    // Cache the store's runtime Arc so subsequent re-opens and lazily-opened
    // caches stay on the same pool.
    let runtime = std::sync::Arc::clone(store.runtime());

    // Capture initial index.db identity (inode/size/mtime on unix).
    // Stat the slot-aware index path, not `cqs_dir/index.db` directly: a
    // slot-migrated project keeps the live DB at
    // `.cqs/slots/<active>/index.db`, where a literal `cqs_dir/index.db` join
    // points at a path that doesn't exist, `from_path` returns None, and the
    // daemon's mutable caches never invalidate when the operator runs
    // `cqs index`. `resolve_index_db` honors slot resolution and falls back
    // cleanly on legacy projects.
    let index_id = DbFileIdentity::from_path(&cqs::resolve_index_db(&cqs_dir));
    if index_id.is_none() {
        tracing::debug!("Could not stat index.db — staleness detection will be skipped until first successful stat");
    }

    // Index-aware model resolution: prefer the model recorded in the store
    // metadata over CQS_EMBEDDING_MODEL / config / default. Without this,
    // running `CQS_EMBEDDING_MODEL=foo` against a `bar`-model index gives
    // silent zero-result queries (the dim mismatch only surfaces as a
    // tracing::warn! deep in the index backend). See ROADMAP.md "Embedder
    // swap workflow".
    let stored_model = store.stored_model_name();
    let project_config = cqs::config::Config::load(&root);
    let model_config = ModelConfig::resolve_for_query(
        stored_model.as_deref(),
        None,
        project_config.embedding.as_ref(),
    )
    .apply_env_overrides();

    Ok(BatchContext {
        // Mutex<Arc<Store>> so `checkout_view` can clone the Arc out cheaply.
        store: Mutex::new(Arc::new(store)),
        runtime,
        embedder: Arc::new(OnceLock::new()),
        config: RefCell::new(None),
        reranker: Arc::new(OnceLock::new()),
        audit_state: RefCell::new(None),
        hnsw: RefCell::new(None),
        base_hnsw: RefCell::new(None),
        call_graph: RefCell::new(None),
        test_chunks: RefCell::new(None),
        file_set: RefCell::new(None),
        notes_cache: RefCell::new(None),
        splade_encoder: Arc::new(OnceLock::new()),
        splade_index: Arc::new(Mutex::new(None)),
        refs: Arc::new(Mutex::new(lru::LruCache::new(refs_lru_size()))),
        root,
        cqs_dir,
        model_config,
        index_id: Cell::new(index_id),
        // None means the first check runs unconditionally; the 100ms
        // rate-limit kicks in only after the first successful stat.
        last_staleness_check: Cell::new(None),
        error_count: Arc::new(AtomicU64::new(0)),
        last_command_time: Cell::new(Instant::now()),
        // `started_at` is captured here so `uptime_secs` in the ping response
        // measures from BatchContext creation — the meaningful event for the
        // daemon (the embedder load may be later).
        started_at: Instant::now(),
        query_count: Arc::new(AtomicU64::new(0)),
        // `cmd_batch` and one-shot `create_context` callers don't run a watch
        // loop, so the snapshot stays at `unknown` for their whole lifetime.
        // `watch_and_serve` clones this Arc into the watch loop and overwrites
        // it on every tick.
        watch_snapshot: cqs::watch_status::shared_unknown(),
        // Outside `cqs watch --serve` no listener is plugged in, so flipping
        // this from a stray client is harmless (the watch loop that would
        // consume the signal simply isn't running).
        reconcile_signal: cqs::watch_status::shared_reconcile_signal(),
        // Default no-op notifier. Outside `cqs watch --serve` the watch loop
        // never calls `set_fresh`, so a stray `wait_fresh` request hits the
        // caller's deadline naturally.
        fresh_notifier: cqs::watch_status::shared_fresh_notifier(),
    })
}

/// Create a BatchContext for testing with a temporary store.
///
/// Visibility: `pub(in crate::cli)` under `#[cfg(test)]` so both
/// `batch::handlers::*` tests (search.rs / dispatch_tests.rs) and
/// `cli::watch` adversarial tests can reuse the same fixture wiring.
///
/// The store is opened RO at the SQLite connection level via
/// [`Store::open_readonly_after_init`] — the DB is expected to be
/// pre-initialized by `setup_test_store` so the closure is a no-op, but
/// the constructor path matches production code that may need fixture setup.
#[cfg(test)]
pub(in crate::cli) fn create_test_context(cqs_dir: &std::path::Path) -> Result<BatchContext> {
    let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);
    // open_readonly_after_init returns Store<ReadOnly> directly.
    let store = Store::<ReadOnly>::open_readonly_after_init(&index_path, |_| Ok(()))
        .map_err(|e| anyhow::anyhow!("Failed to open test store: {e}"))?;
    let root = cqs_dir.parent().unwrap_or(cqs_dir).to_path_buf();
    let index_id = DbFileIdentity::from_path(&index_path);
    // Cache the runtime Arc so test contexts re-open on the same pool.
    let runtime = std::sync::Arc::clone(store.runtime());

    Ok(BatchContext {
        store: Mutex::new(Arc::new(store)),
        runtime,
        embedder: Arc::new(OnceLock::new()),
        config: RefCell::new(None),
        reranker: Arc::new(OnceLock::new()),
        audit_state: RefCell::new(None),
        hnsw: RefCell::new(None),
        base_hnsw: RefCell::new(None),
        call_graph: RefCell::new(None),
        test_chunks: RefCell::new(None),
        file_set: RefCell::new(None),
        notes_cache: RefCell::new(None),
        splade_encoder: Arc::new(OnceLock::new()),
        splade_index: Arc::new(Mutex::new(None)),
        refs: Arc::new(Mutex::new(lru::LruCache::new(refs_lru_size()))),
        root,
        cqs_dir: cqs_dir.to_path_buf(),
        model_config: ModelConfig::resolve(None, None).apply_env_overrides(),
        index_id: Cell::new(index_id),
        last_staleness_check: Cell::new(None),
        error_count: Arc::new(AtomicU64::new(0)),
        last_command_time: Cell::new(Instant::now()),
        // Same fields as production constructor — keep parity so ping-handler
        // tests against `create_test_context` see realistic counter / uptime
        // values.
        started_at: Instant::now(),
        query_count: Arc::new(AtomicU64::new(0)),
        // Tests get the same `unknown` initial snapshot. Tests that exercise
        // the freshness API replace it via the field directly.
        watch_snapshot: cqs::watch_status::shared_unknown(),
        // Tests get an unwired reconcile signal too. Tests that need to assert
        // the daemon flipped it pull the field clone before invoking dispatch.
        reconcile_signal: cqs::watch_status::shared_reconcile_signal(),
        // Tests get a fresh notifier; freshness-API tests pull the field clone
        // and call `set_fresh` directly.
        fresh_notifier: cqs::watch_status::shared_fresh_notifier(),
    })
}

/// Entry point for `cqs batch`.
pub(crate) fn cmd_batch() -> Result<()> {
    let _span = tracing::info_span!("cmd_batch").entered();

    let ctx = create_context()?;
    ctx.warm(); // Pre-warm embedder so first query doesn't pay ~500ms ONNX init
                // Clone the error-count Arc out before wrapping ctx in
                // `Arc<Mutex<...>>`. The pre-dispatch error paths (line-too-long,
                // tokenize-fail, NUL-byte) bump it without holding the mutex.
    let error_count = Arc::clone(&ctx.error_count);
    // Wrap the BatchContext in Arc<Mutex> so the same view-based dispatch
    // path used by the daemon also drives `cqs batch`. The shell is
    // single-threaded so contention is zero; the wrapper is a couple of
    // pointer indirections per command.
    let ctx = Arc::new(Mutex::new(ctx));

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = std::io::BufReader::new(stdin.lock());

    // read_line allocates incrementally (8KB chunks) until newline or EOF.
    // A multi-GB line without newlines could OOM before the post-hoc check below.
    // Accepted risk: batch input is from a controlling process (AI agent or pipe),
    // not from untrusted network input. The post-hoc cap prevents processing, not
    // allocation. The cap matches `MAX_DIFF_BYTES` (50 MiB) so piped `--stdin`
    // diffs that clear the CLI path aren't silently rejected by the batch/daemon
    // path. Override via `CQS_BATCH_MAX_LINE_LEN`.
    let max_line_len = crate::cli::limits::batch_max_line_len();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "Failed to read stdin line");
                break;
            }
        };

        // Reject lines exceeding the configured cap to prevent further processing.
        if line.len() > max_line_len {
            error_count.fetch_add(1, Ordering::Relaxed);
            // Error is written as a JSON envelope so the agent can pick up the
            // (code, message) pair. Mentioning the env var lets operators bump
            // the cap without grepping source.
            let msg = format!(
                "Batch line exceeds CQS_BATCH_MAX_LINE_LEN ({} bytes); got {} bytes",
                max_line_len,
                line.len(),
            );
            let _ = write_envelope_error(
                &mut stdout,
                crate::cli::json_envelope::error_codes::INVALID_INPUT,
                &msg,
            );
            let _ = stdout.flush();
            continue;
        }

        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Quit/exit
        if trimmed.eq_ignore_ascii_case("quit") || trimmed.eq_ignore_ascii_case("exit") {
            break;
        }

        // Tokenize the line
        let tokens = match shell_words::split(trimmed) {
            Ok(t) => t,
            Err(e) => {
                error_count.fetch_add(1, Ordering::Relaxed);
                let msg = format!("Parse error: {}", e);
                tracing::warn!(
                    code = crate::cli::json_envelope::error_codes::PARSE_ERROR,
                    error = %msg,
                    "Batch cmd_batch: tokenization failed"
                );
                if write_envelope_error(
                    &mut stdout,
                    crate::cli::json_envelope::error_codes::PARSE_ERROR,
                    &msg,
                )
                .is_err()
                {
                    break;
                }
                let _ = stdout.flush();
                continue;
            }
        };

        if tokens.is_empty() {
            continue;
        }

        // NUL byte rejection via shared helper. Both this stdin loop and
        // `BatchContext::dispatch_line` (daemon socket handler) share the same
        // downstream commands and must share the same input validation.
        if let Err(msg) = reject_null_tokens(&tokens) {
            error_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                code = crate::cli::json_envelope::error_codes::INVALID_INPUT,
                error = msg,
                "Batch cmd_batch: NUL byte in tokens"
            );
            if write_envelope_error(
                &mut stdout,
                crate::cli::json_envelope::error_codes::INVALID_INPUT,
                msg,
            )
            .is_err()
            {
                break;
            }
            continue;
        }

        // Build a snapshot view (briefly locks ctx, runs idle sweep and clones
        // the snapshot Arcs). The shell loop is single-threaded so the lock is
        // uncontended; we still go through the same path as the daemon to keep
        // one dispatch shape across surfaces.
        let view = checkout_view_from_arc(&ctx);

        // Refresh shortcut — same shape as the daemon path. Need to do this
        // here because pipelines can't carry Refresh and the dispatch path
        // for Refresh re-locks the BatchContext mutex via outer_lock.
        if let Ok(parsed) = commands::BatchInput::try_parse_from(&tokens) {
            if matches!(parsed.cmd, commands::BatchCmd::Refresh) {
                match ctx.lock().unwrap_or_else(|p| p.into_inner()).invalidate() {
                    Ok(()) => {
                        let _ = write_json_line(
                            &mut stdout,
                            &serde_json::json!({
                                "status": "ok",
                                "message": "Caches invalidated, Store re-opened",
                            }),
                        );
                    }
                    Err(e) => {
                        error_count.fetch_add(1, Ordering::Relaxed);
                        let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                        let _ = write_envelope_error(&mut stdout, code.as_str(), &msg);
                    }
                }
                let _ = stdout.flush();
                continue;
            }
        }

        // Pipeline detection: if tokens contain a standalone `|`, route to pipeline
        if pipeline::has_pipe_token(&tokens) {
            match pipeline::execute_pipeline(&view, &tokens, trimmed) {
                Ok(value) => {
                    if write_json_line(&mut stdout, &value).is_err() {
                        break;
                    }
                }
                Err(pe) => {
                    error_count.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        code = pe.code,
                        error = %pe.message,
                        "Batch cmd_batch: pipeline failed"
                    );
                    if write_envelope_error(&mut stdout, pe.code, &pe.message).is_err() {
                        break;
                    }
                }
            }
        } else {
            // Single command — existing path
            match commands::BatchInput::try_parse_from(&tokens) {
                Ok(input) => match commands::dispatch(&view, input.cmd) {
                    Ok(value) => {
                        if write_json_line(&mut stdout, &value).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        error_count.fetch_add(1, Ordering::Relaxed);
                        // redact_error walks the source chain and emits a stable
                        // (code, message) pair instead of echoing the raw anyhow
                        // chain. Full unredacted chain is logged via
                        // tracing::warn! inside redact_error for operator
                        // correlation.
                        let (code, msg) = crate::cli::json_envelope::redact_error(&e);
                        if write_envelope_error(&mut stdout, code.as_str(), &msg).is_err() {
                            break;
                        }
                    }
                },
                Err(e) => {
                    error_count.fetch_add(1, Ordering::Relaxed);
                    let msg = format!("{e:#}");
                    tracing::warn!(
                        code = crate::cli::json_envelope::error_codes::PARSE_ERROR,
                        error = %msg,
                        "Batch cmd_batch: clap parse failed"
                    );
                    if write_envelope_error(
                        &mut stdout,
                        crate::cli::json_envelope::error_codes::PARSE_ERROR,
                        &msg,
                    )
                    .is_err()
                    {
                        break;
                    }
                }
            }
        }

        let _ = stdout.flush();
    }

    Ok(())
}
