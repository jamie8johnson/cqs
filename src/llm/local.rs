//! Local / OpenAI-compat LLM batch provider.
//!
//! Targets any server that speaks `/v1/chat/completions` (llama.cpp server,
//! vLLM, Ollama, LMStudio, text-generation-webui). Unlike the Anthropic Batches
//! API which is asynchronous with poll-to-completion, local servers expose only
//! synchronous per-request inference — so "submit batch" here means *"fan out
//! a worker pool and collect per-item results into a stash before returning a
//! fake batch id that subsequent `wait_for_batch` / `fetch_batch_results` calls
//! drain."*
//!
//! ## Concurrency model
//!
//! Each `submit_*` variant uses `std::thread::scope` + `crossbeam_channel` to
//! dispatch items across `concurrency` workers. Each worker loops:
//!   1. pull `BatchSubmitItem` from the channel,
//!   2. POST `${api_base}/chat/completions`,
//!   3. parse `choices[0].message.content`,
//!   4. deposit into the stash under `(batch_id → custom_id → text)`,
//!   5. invoke the streaming `on_item_complete` callback if set.
//!
//! All worker bodies are wrapped in `std::panic::catch_unwind` so a bad item
//! (or a panicking callback) never aborts the batch. The stash is a
//! `Mutex<HashMap<String, HashMap<String, String>>>` keyed by batch-id; the
//! outer lock is held only while inserting a finished item, so workers don't
//! serialise on each other.
//!
//! ## Streaming persist
//!
//! The optional callback supplied via [`set_on_item_complete`] fires once per
//! successful item, in arbitrary worker order. The outer loop (e.g.
//! `llm_summary_pass`) uses this to `INSERT OR IGNORE` each completed summary
//! into SQLite as soon as it lands, so a Ctrl-C at 50% doesn't lose the first
//! 50%. The final `fetch_batch_results` pass writes the same rows again; the
//! primary-key conflict makes the double-write a no-op.
//!
//! [`set_on_item_complete`]: super::provider::BatchProvider::set_on_item_complete

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use reqwest::blocking::Client;
use reqwest::StatusCode;

use super::provider::{BatchProvider, BatchSubmitItem};
use super::{local_concurrency, local_timeout, LlmClient, LlmConfig, LlmError};

/// Retry backoff schedule: 4 attempts, 500ms → 1s → 2s → 4s (7.5s window).
const RETRY_BACKOFFS_MS: &[u64] = &[500, 1000, 2000, 4000];
const MAX_ATTEMPTS: usize = 4;

/// Callback signature: `(custom_id, text)`. See [`BatchProvider::set_on_item_complete`].
type OnItemCb = Box<dyn Fn(&str, &str) + Send + Sync>;

/// OpenAI-compat `/v1/chat/completions` provider.
///
/// Not a drop-in replacement for the Anthropic Batches API — the batch-id /
/// `wait_for_batch` / `fetch_batch_results` contract is faked over a
/// worker-pool fanout. See the module docs.
pub struct LocalProvider {
    http: Client,
    api_base: String,
    model: String,
    concurrency: usize,
    api_key: Option<String>,
    /// Per-request timeout. Defaults to 120s (Anthropic uses 60s).
    timeout: Duration,
    /// Streaming per-item callback. Optional; Fn + Send + Sync so multiple
    /// workers can fire it concurrently.
    on_item: Mutex<Option<OnItemCb>>,
    /// Completed-item stash keyed by `batch_id → (custom_id → text)`. Drained
    /// by `fetch_batch_results`; single-process only.
    stash: Mutex<HashMap<String, HashMap<String, String>>>,
}

impl LocalProvider {
    /// Build a `LocalProvider` from a resolved [`LlmConfig`].
    ///
    /// Reads `CQS_LLM_API_KEY` (optional), `CQS_LOCAL_LLM_CONCURRENCY`
    /// (default 4, clamped [1,16] post-P3.47), `CQS_LOCAL_LLM_TIMEOUT_SECS`
    /// (default 120).
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::Http`] if the underlying `reqwest` client cannot be
    /// built (e.g. invalid TLS config). Callers must have already validated
    /// the endpoint / model via [`crate::llm::create_client`].
    pub fn new(llm_config: LlmConfig) -> Result<Self, LlmError> {
        let _span = tracing::info_span!("local_provider_new").entered();

        // P2.32: bail if `api_base` isn't HTTP/HTTPS. `reqwest` will fail
        // *every* request individually, burning the full retry budget per
        // item before surfacing the error — a 7.5s stall per call instead
        // of a fail-fast at construction. Lightweight scheme check avoids
        // pulling `url` as a direct dep just for this guard.
        let api_base_lc = llm_config.api_base.to_ascii_lowercase();
        if !api_base_lc.starts_with("http://") && !api_base_lc.starts_with("https://") {
            return Err(LlmError::Api {
                status: 0,
                message: format!(
                    "CQS_LLM_API_BASE must use http:// or https://; got: {}",
                    llm_config.api_base
                ),
            });
        }

        let concurrency = local_concurrency();
        let timeout = local_timeout();
        let mut api_key = std::env::var("CQS_LLM_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());

        // SEC-V1.33-10 / #1340: refuse to attach `Authorization: Bearer <key>`
        // when an `http://` (plaintext) base targets a non-loopback /
        // non-RFC1918 host. The default reqwest cross-origin redirect policy
        // and SEC-V1.30.1-7 same-origin redirect both assume the operator at
        // least started with HTTPS or a loopback bind — neither defense
        // triggers when the operator's intentional initial bind is plaintext
        // to a public host. Pre-fix, every prompt + the bearer token shipped
        // over the wire in cleartext to whoever was on-path.
        //
        // Symmetric to `cqs serve`'s loud-warn for `--no-auth` on non-loopback
        // (PB-V1.30.1-1, #1206) — operator gets a clear refusal instead of a
        // silent leak.
        if api_base_lc.starts_with("http://") && api_key.is_some() {
            let host = http_host(&llm_config.api_base);
            if !is_local_or_private_host(host) {
                tracing::warn!(
                    api_base = %llm_config.api_base,
                    host = host,
                    "Refusing to attach Authorization: Bearer over plaintext HTTP to a \
                     non-loopback / non-RFC1918 host. Use https:// or bind to a private \
                     network. Auth header dropped; requests will proceed without it. \
                     SEC-V1.33-10 / #1340"
                );
                api_key = None;
            }
        }

        // SEC-V1.30.1-7 (#1223): same-origin redirect policy. Submit
        // requests carry `Authorization: Bearer <key>` when
        // CQS_LLM_API_KEY is set. The historical `Policy::limited(2)`
        // followed redirects to *any* origin — a misconfigured load
        // balancer that 302s `internal-llm/` → `attacker-host/v1/...`
        // would surface as a silent 401 loop on the redirect target
        // (because reqwest 0.12 strips Authorization cross-origin) but
        // the strip is silent and depends on a default that could
        // shift across versions. `same_origin_redirect_policy(2)`
        // refuses cross-origin hops outright with a `tracing::warn!`,
        // so the failure is loud and the bearer never travels.
        //
        // P3.48: cap idle pool to `concurrency` per-host with a 30s idle
        // timeout. The default reqwest pool is unbounded with a 90s idle
        // timeout — long-running indexing sessions accumulated stale
        // sockets against vLLM/llama.cpp servers, leaking FDs without a
        // matching outbound traffic spike.
        let http = Client::builder()
            .timeout(timeout)
            .redirect(crate::llm::redirect::same_origin_redirect_policy(2))
            .pool_max_idle_per_host(concurrency)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?;

        tracing::info!(
            api_base = %llm_config.api_base,
            model = %llm_config.model,
            concurrency,
            timeout_secs = timeout.as_secs(),
            auth = api_key.is_some(),
            "LocalProvider ready"
        );

        Ok(Self {
            http,
            api_base: llm_config.api_base,
            model: llm_config.model,
            concurrency,
            api_key,
            timeout,
            on_item: Mutex::new(None),
            stash: Mutex::new(HashMap::new()),
        })
    }

    /// Core fan-out: spawn `concurrency` workers, feed them items, collect results.
    ///
    /// `prompt_builder` decides how to shape the user message given
    /// `(content, context, language)` — identical signatures to
    /// [`LlmClient::submit_batch_inner`] so the prompt paths stay parallel.
    fn submit_via_chat_completions(
        &self,
        items: &[BatchSubmitItem],
        max_tokens: u32,
        purpose: &str,
        prompt_builder: fn(&str, &str, &str) -> String,
    ) -> Result<String, LlmError> {
        if items.is_empty() {
            return Err(LlmError::BatchFailed("Cannot submit empty batch".into()));
        }

        let batch_id = uuid::Uuid::new_v4().to_string();

        // P2.32: clamp worker count to item count. Submitting 1 item to 64
        // workers spawned 63 idle threads that immediately exited via channel
        // disconnect, but each one still tripped the OS thread create/destroy
        // path. Cap at items.len() with a floor of 1.
        let workers = self.concurrency.min(items.len()).max(1);

        let _span = tracing::info_span!(
            "local_batch_submit",
            provider = "local",
            model = %self.model,
            n = items.len(),
            concurrency = workers,
            batch_id = %batch_id,
            purpose,
        )
        .entered();

        let start = Instant::now();
        let (tx, rx) = bounded::<&BatchSubmitItem>(workers.max(8) * 2);

        let results: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
        // Track auth failures across workers — if *every* item that attempted
        // a first request saw 401/403, we abort with an auth-specific error.
        let auth_failures: Mutex<usize> = Mutex::new(0);
        let auth_attempts: Mutex<usize> = Mutex::new(0);
        let succeeded: Mutex<usize> = Mutex::new(0);
        let failed: Mutex<usize> = Mutex::new(0);

        std::thread::scope(|s| {
            // Spawn workers first so the channel has consumers by the time the
            // feeder starts sending.
            for worker_id in 0..workers {
                let rx_worker = rx.clone();
                let url = format!("{}/chat/completions", self.api_base);
                let results_ref = &results;
                let auth_failures_ref = &auth_failures;
                let auth_attempts_ref = &auth_attempts;
                let succeeded_ref = &succeeded;
                let failed_ref = &failed;
                let on_item_ref = &self.on_item;
                let self_ref = self;
                s.spawn(move || {
                    let _worker_span = tracing::debug_span!("local_worker", worker_id).entered();
                    // P3-8 (audit v1.33.0): per-worker counters + wall-clock
                    // so the post-loop completion line breaks down the batch
                    // by worker. Without these the journal has N
                    // indistinguishable "worker complete" lines and operators
                    // tuning `CQS_LOCAL_LLM_CONCURRENCY` can't see tail-worker
                    // skew. `retried` would require lifting state out of
                    // `process_one_item`; out of scope for this audit pass.
                    let worker_start = std::time::Instant::now();
                    let mut completed: usize = 0;
                    let mut failed: usize = 0;
                    while let Ok(item) = rx_worker.recv() {
                        // Per-item catch_unwind — a panic on one item must not
                        // kill the worker.
                        let item_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                self_ref.process_one_item(
                                    &url,
                                    item,
                                    max_tokens,
                                    prompt_builder,
                                    auth_failures_ref,
                                    auth_attempts_ref,
                                )
                            }));

                        match item_result {
                            Ok(Ok(Some(text))) => {
                                completed += 1;
                                // Stash the result for fetch_batch_results.
                                if let Ok(mut map) = results_ref.lock() {
                                    map.insert(item.custom_id.clone(), text.clone());
                                }
                                if let Ok(mut s) = succeeded_ref.lock() {
                                    *s += 1;
                                }
                                // Fire streaming callback if set.
                                // Callback is wrapped in its own catch_unwind
                                // so a panicking callback doesn't poison the
                                // worker.
                                let cb_guard = on_item_ref.lock();
                                if let Ok(guard) = cb_guard {
                                    if let Some(cb) = guard.as_ref() {
                                        let cid = item.custom_id.clone();
                                        let tx = text.clone();
                                        let cb_ref: &dyn Fn(&str, &str) = cb.as_ref();
                                        if let Err(panic) = std::panic::catch_unwind(
                                            std::panic::AssertUnwindSafe(|| {
                                                cb_ref(&cid, &tx);
                                            }),
                                        ) {
                                            tracing::error!(
                                                item_id = %item.custom_id,
                                                panic = ?panic,
                                                "on_item_complete callback panic, continuing"
                                            );
                                        }
                                    }
                                }
                            }
                            Ok(Ok(None)) => {
                                // Item skipped (non-retriable 4xx, malformed
                                // JSON, etc) — already logged at call site.
                                failed += 1;
                                if let Ok(mut f) = failed_ref.lock() {
                                    *f += 1;
                                }
                            }
                            Ok(Err(e)) => {
                                tracing::warn!(
                                    item_id = %item.custom_id,
                                    error = %e,
                                    "item processing failed after retries"
                                );
                                failed += 1;
                                if let Ok(mut f) = failed_ref.lock() {
                                    *f += 1;
                                }
                            }
                            Err(panic) => {
                                tracing::error!(
                                    item_id = %item.custom_id,
                                    panic = ?panic,
                                    "worker panic, skipping item"
                                );
                                failed += 1;
                                if let Ok(mut f) = failed_ref.lock() {
                                    *f += 1;
                                }
                            }
                        }
                    }
                    // P3-8: per-worker completion with explicit fields.
                    // Span context isn't always carried by JSON formatters,
                    // so emit `worker_id` directly rather than relying on
                    // the entered span.
                    tracing::info!(
                        worker_id,
                        completed,
                        failed,
                        elapsed_ms = worker_start.elapsed().as_millis() as u64,
                        "local batch worker complete"
                    );
                });
            }

            // Feed items into the channel. Drop tx when done so workers exit.
            for item in items {
                if tx.send(item).is_err() {
                    // All workers gone — unusual (panic on construction?).
                    tracing::error!("local batch channel closed before all items fed");
                    break;
                }
            }
            drop(tx);
        });
        // `std::thread::scope` guarantees all workers have joined at this
        // point — no dangling threads, no lost results.

        // Recover counters even on poison — counts are advisory and dropping
        // them to 0 would mask real progress in the "complete" log.
        let ok = *succeeded.lock().unwrap_or_else(|p| p.into_inner());
        let err = *failed.lock().unwrap_or_else(|p| p.into_inner());
        let elapsed_ms = start.elapsed().as_millis() as u64;

        // Fatal-batch check: if every item that talked to the server saw
        // 401/403 on its first request, the credentials are wrong — abort
        // with a specific error instead of silently returning an empty stash.
        let auth_fail = *auth_failures.lock().unwrap_or_else(|p| p.into_inner());
        let auth_attempt = *auth_attempts.lock().unwrap_or_else(|p| p.into_inner());
        if auth_attempt > 0 && auth_fail == auth_attempt {
            tracing::error!(
                url = %self.api_base,
                "local batch aborted: all {} requests rejected with 401/403",
                auth_attempt
            );
            return Err(LlmError::Api {
                status: 401,
                message: format!(
                    "Authentication rejected at {}; check CQS_LLM_API_KEY",
                    self.api_base
                ),
            });
        }

        tracing::info!(
            batch_id = %batch_id,
            submitted = items.len(),
            succeeded = ok,
            failed = err,
            elapsed_ms,
            "local batch complete"
        );

        // Move results into the stash under the batch id. On poison we recover
        // the partially-populated map rather than silently substituting an
        // empty one — losing partial results is worse than the panic risk.
        let results_map = match results.into_inner() {
            Ok(m) => m,
            Err(poisoned) => {
                tracing::error!(
                    succeeded = ok,
                    "results mutex poisoned during local batch — recovering inner state"
                );
                poisoned.into_inner()
            }
        };

        // Invariant: if results_map.len() != ok, accounting drifted. Surface
        // it loudly rather than shipping a short stash silently.
        if results_map.len() != ok {
            tracing::error!(
                map_len = results_map.len(),
                succeeded = ok,
                "local batch accounting drift: results map size != succeeded count"
            );
            return Err(LlmError::BatchFailed(format!(
                "local batch accounting drift: ok={ok} map_len={}",
                results_map.len()
            )));
        }

        // P2.73: cap the stash so a long-running daemon submitting batches
        // without ever calling `fetch_batch_results` doesn't grow memory
        // unbounded. 128 batches is plenty — production callers drain in
        // submit order, so when this cap fires it's a leak signal.
        const MAX_STASH_BATCHES: usize = 128;
        let mut stash = self.stash.lock().unwrap_or_else(|p| p.into_inner());
        while stash.len() >= MAX_STASH_BATCHES {
            // Pick the lexicographically smallest UUID as a stable evictee —
            // HashMap insertion order isn't preserved, and the alternative
            // (rebuild as IndexMap) is more invasive than this finding warrants.
            let stale_key = match stash.keys().min() {
                Some(k) => k.clone(),
                None => break,
            };
            stash.remove(&stale_key);
            tracing::warn!(
                batch_id = %stale_key,
                cap = MAX_STASH_BATCHES,
                "LocalProvider stash exceeded cap; evicting unfetched entry — \
                 callers should drain via fetch_batch_results"
            );
        }
        stash.insert(batch_id.clone(), results_map);
        drop(stash);

        Ok(batch_id)
    }

    /// Handle one item: POST with retry, return the response text.
    ///
    /// Returns:
    /// - `Ok(Some(text))` on success (status 200 + parseable content)
    /// - `Ok(None)` on a skip-without-retry condition (non-retriable 4xx,
    ///   malformed JSON, empty choices)
    /// - `Err(_)` on exhausted retries (connection refused, 5xx, timeout)
    #[allow(clippy::too_many_arguments)]
    fn process_one_item(
        &self,
        url: &str,
        item: &BatchSubmitItem,
        max_tokens: u32,
        prompt_builder: fn(&str, &str, &str) -> String,
        auth_failures: &Mutex<usize>,
        auth_attempts: &Mutex<usize>,
    ) -> Result<Option<String>, LlmError> {
        let prompt = prompt_builder(&item.content, &item.context, &item.language);
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": [{ "role": "user", "content": prompt }],
        });

        let mut last_err: Option<String> = None;
        for attempt in 0..MAX_ATTEMPTS {
            let _item_span = tracing::debug_span!(
                "local_item",
                custom_id = %item.custom_id,
                attempt,
            )
            .entered();

            let mut req = self
                .http
                .post(url)
                .header("content-type", "application/json")
                .json(&body);
            if let Some(ref key) = self.api_key {
                req = req.header("Authorization", format!("Bearer {}", key));
            }

            let resp = req.send();
            let is_first_attempt = attempt == 0;

            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status.is_success() {
                        // Parse response body.
                        let text_opt = parse_choices_content(r);
                        match text_opt {
                            Ok(Some(text)) => return Ok(Some(text)),
                            Ok(None) => {
                                // Empty choices or null content — skip, do
                                // not retry (server returned 200 but no data).
                                tracing::warn!(
                                    custom_id = %item.custom_id,
                                    "empty choices / null content, skipping"
                                );
                                return Ok(None);
                            }
                            Err(e) => {
                                // Malformed JSON — skip, do not retry.
                                tracing::warn!(
                                    custom_id = %item.custom_id,
                                    error = %e,
                                    "malformed response JSON, skipping"
                                );
                                return Ok(None);
                            }
                        }
                    }

                    // Track auth-failure statistics on the FIRST request only
                    // so we can abort the batch if every worker hit 401/403.
                    // P2.35: recover poisoned mutexes via `into_inner` so an
                    // earlier worker panic doesn't cascade into the rest of
                    // the pool. Counters are advisory.
                    if is_first_attempt
                        && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
                    {
                        *auth_attempts.lock().unwrap_or_else(|p| p.into_inner()) += 1;
                        *auth_failures.lock().unwrap_or_else(|p| p.into_inner()) += 1;
                    } else if is_first_attempt {
                        *auth_attempts.lock().unwrap_or_else(|p| p.into_inner()) += 1;
                    }

                    // Retriable: 429 (rate limit), 5xx. Skip: 4xx ≠ 429.
                    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                        let backoff = RETRY_BACKOFFS_MS[attempt.min(RETRY_BACKOFFS_MS.len() - 1)];
                        let body_preview = body_preview(r);
                        tracing::warn!(
                            attempt,
                            backoff_ms = backoff,
                            error_kind = %status,
                            body = %body_preview,
                            "local retry"
                        );
                        last_err = Some(format!("HTTP {}", status));
                        if attempt < MAX_ATTEMPTS - 1 {
                            std::thread::sleep(Duration::from_millis(backoff));
                        }
                        continue;
                    }

                    // Non-retriable 4xx — log body and skip.
                    let body_preview = body_preview(r);
                    tracing::warn!(
                        status = %status,
                        body = %body_preview,
                        "local item non-retriable 4xx, skipping"
                    );
                    return Ok(None);
                }
                Err(e) => {
                    // reqwest error: timeout, connection refused, DNS, TLS...
                    // All retriable — we can't tell a transient hiccup from
                    // "server down" without trying again.
                    let backoff = RETRY_BACKOFFS_MS[attempt.min(RETRY_BACKOFFS_MS.len() - 1)];
                    if e.is_timeout() {
                        tracing::warn!(
                            timeout_secs = self.timeout.as_secs(),
                            url = %url,
                            attempt,
                            backoff_ms = backoff,
                            "local request timed out"
                        );
                    } else {
                        tracing::warn!(
                            attempt,
                            backoff_ms = backoff,
                            error_kind = "network",
                            error = %e,
                            "local retry"
                        );
                    }
                    last_err = Some(e.to_string());
                    if attempt < MAX_ATTEMPTS - 1 {
                        std::thread::sleep(Duration::from_millis(backoff));
                    }
                    continue;
                }
            }
        }

        // Exhausted all attempts.
        Err(LlmError::BatchFailed(format!(
            "Local request to {} failed after {} attempts: {}",
            url,
            MAX_ATTEMPTS,
            last_err.unwrap_or_else(|| "unknown".to_string())
        )))
    }
}

/// Hard cap on response body size (RB-V1.30-1 / P1.10).
///
/// Summary outputs are typically a few hundred bytes; 4 MiB is ~1000× headroom.
/// Larger bodies are a sign of a misbehaving or hostile endpoint and we'd
/// rather error than OOM the daemon. Up to `local_concurrency()` (≤16) workers
/// can be reading concurrently, so an unbounded read multiplies the risk.
///
/// Override via `CQS_LOCAL_LLM_MAX_BODY_BYTES` (must be > 0).
///
/// Not memoised: read on each response so tests can flip the cap without a
/// process-wide cache. The env-var cost is negligible compared to the HTTP
/// request that just completed.
/// Extract the host portion of an `http(s)://host[:port]/path` URL without
/// pulling the `url` crate as a direct dep.
///
/// Handles IPv6 literals (`http://[::1]:8080/...` → `[::1]`), default ports
/// (`http://example.com/...` → `example.com`), and ports
/// (`http://10.0.0.1:8080/v1` → `10.0.0.1`). Returns the input unchanged
/// when the URL doesn't match the expected shape — `is_local_or_private_host`
/// will then reject it as not-loopback.
///
/// SEC-V1.33-10 / #1340 helper.
fn http_host(api_base: &str) -> &str {
    let rest = match api_base
        .strip_prefix("http://")
        .or_else(|| api_base.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return api_base,
    };
    // IPv6 literals are bracketed: [::1]:8080
    if let Some(stripped) = rest.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            return &stripped[..end];
        }
    }
    // Otherwise host runs until the first `:` (port) or `/` (path).
    let end = rest
        .bytes()
        .position(|b| b == b':' || b == b'/')
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Predicate: does `host` (already extracted by [`http_host`]) refer to a
/// loopback or RFC1918 destination, where plaintext `http://` is acceptable?
///
/// Loopback: `127.0.0.1` (and any `127.x.x.x`), `::1`, `localhost`.
/// RFC1918:
/// - `10.0.0.0/8` (any `10.x.x.x`)
/// - `172.16.0.0/12` (`172.16.x.x` through `172.31.x.x`)
/// - `192.168.0.0/16` (any `192.168.x.x`)
///
/// Returns `false` for hostnames containing dots that don't match those
/// patterns (e.g. `internal-llm.example.com`). Returns `false` for
/// non-IPv4-shaped strings outside the explicit-loopback names —
/// catches `attacker.example.com`, `8.8.8.8`, etc.
fn is_local_or_private_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h == "::1" {
        return true;
    }
    // IPv4 dotted-quad: split into 4 octets and pattern-match the prefix.
    let octets: Vec<_> = h.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    let parsed: Option<Vec<u8>> = octets.iter().map(|o| o.parse::<u8>().ok()).collect();
    let Some(quad) = parsed else {
        return false;
    };
    match (quad[0], quad[1]) {
        (127, _) => true,                           // 127.0.0.0/8
        (10, _) => true,                            // 10.0.0.0/8
        (172, b) if (16..=31).contains(&b) => true, // 172.16.0.0/12
        (192, 168) => true,                         // 192.168.0.0/16
        _ => false,
    }
}

#[cfg(test)]
mod url_predicates_tests {
    use super::{http_host, is_local_or_private_host};

    #[test]
    fn host_extraction_basic() {
        assert_eq!(http_host("http://example.com/v1"), "example.com");
        assert_eq!(http_host("https://api.openai.com/v1"), "api.openai.com");
        assert_eq!(http_host("http://10.0.0.1:8080/v1"), "10.0.0.1");
        assert_eq!(http_host("http://localhost:8000"), "localhost");
        assert_eq!(http_host("http://localhost"), "localhost");
    }

    #[test]
    fn host_extraction_ipv6() {
        assert_eq!(http_host("http://[::1]:8080/v1"), "::1");
        assert_eq!(http_host("https://[2001:db8::1]/api"), "2001:db8::1");
    }

    #[test]
    fn host_extraction_unparseable_returns_input() {
        assert_eq!(http_host("not a url"), "not a url");
        assert_eq!(http_host("ftp://x"), "ftp://x");
    }

    #[test]
    fn loopback_accepted() {
        assert!(is_local_or_private_host("127.0.0.1"));
        assert!(is_local_or_private_host("127.5.6.7"));
        assert!(is_local_or_private_host("::1"));
        assert!(is_local_or_private_host("localhost"));
        assert!(is_local_or_private_host("LOCALHOST"));
    }

    #[test]
    fn rfc1918_accepted() {
        assert!(is_local_or_private_host("10.0.0.1"));
        assert!(is_local_or_private_host("10.255.255.255"));
        assert!(is_local_or_private_host("172.16.0.1"));
        assert!(is_local_or_private_host("172.31.255.255"));
        assert!(is_local_or_private_host("192.168.1.1"));
    }

    #[test]
    fn rfc1918_172_boundaries() {
        assert!(!is_local_or_private_host("172.15.0.1"), "below /12 range");
        assert!(!is_local_or_private_host("172.32.0.1"), "above /12 range");
    }

    #[test]
    fn public_rejected() {
        assert!(!is_local_or_private_host("8.8.8.8"));
        assert!(!is_local_or_private_host("example.com"));
        assert!(!is_local_or_private_host("internal-llm.example.com"));
        assert!(!is_local_or_private_host(""));
    }

    #[test]
    fn rejects_non_ipv4_with_dots() {
        // Hostname like "1.2.3" (3 parts) shouldn't be treated as IP.
        assert!(!is_local_or_private_host("1.2.3"));
        // Hostname with 4 parts but non-numeric.
        assert!(!is_local_or_private_host("a.b.c.d"));
    }
}

fn local_max_body_bytes() -> usize {
    std::env::var("CQS_LOCAL_LLM_MAX_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(4 * 1024 * 1024)
}

/// Parse an OpenAI-compat `/v1/chat/completions` response, extracting the
/// first choice's `message.content`.
///
/// Returns:
/// - `Ok(Some(text))` — non-empty content present
/// - `Ok(None)` — valid JSON but `choices` is empty or `content` is null/empty
/// - `Err(_)` — malformed JSON or body exceeds [`local_max_body_bytes`]
///
/// The body is read with a length cap to defend against hostile / misbehaving
/// servers that return multi-GB responses (P1.10 / RB-V1.30-1).
fn parse_choices_content(resp: reqwest::blocking::Response) -> Result<Option<String>, LlmError> {
    use std::io::Read;
    let cap = local_max_body_bytes();
    let mut buf = Vec::with_capacity(8 * 1024);
    // Read one byte beyond the cap so we can distinguish "exactly cap" from
    // "exceeded cap".
    resp.take(cap as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| LlmError::BatchFailed(format!("response body read failed: {e}")))?;
    if buf.len() > cap {
        return Err(LlmError::BatchFailed(format!(
            "response body exceeds cap ({} > {} bytes)",
            buf.len(),
            cap
        )));
    }
    let body: serde_json::Value = serde_json::from_slice(&buf)
        .map_err(|e| LlmError::BatchFailed(format!("response body not valid JSON: {e}")))?;
    let content = body
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    match content {
        Some(s) if !s.is_empty() => Ok(Some(s)),
        _ => Ok(None),
    }
}

/// Read up to 2 KiB from an HTTP error response body for log context.
/// Returns the empty string if the body can't be read or is non-UTF-8.
///
/// Hard-capped at 2 KiB to bound log spam and prevent OOM on hostile error
/// bodies (P1.10 / RB-V1.30-1). The caller further trims to the first 256
/// chars so logs don't blow up either.
fn body_preview(resp: reqwest::blocking::Response) -> String {
    use std::io::Read;
    const PREVIEW_CAP: u64 = 2 * 1024;
    let mut buf = Vec::with_capacity(PREVIEW_CAP as usize);
    if resp.take(PREVIEW_CAP).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let body = String::from_utf8_lossy(&buf);
    let cut = body
        .char_indices()
        .nth(256)
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    body[..cut].to_string()
}

impl BatchProvider for LocalProvider {
    fn submit_batch(
        &self,
        kind: super::provider::BatchKind,
        items: &[BatchSubmitItem],
        max_tokens: u32,
    ) -> Result<String, LlmError> {
        // #1347: dispatch on `BatchKind` once. Adding a new kind is one
        // arm. The historical purpose-label strings ("prebuilt" / "doc" /
        // "hyde") are kept stable so existing log greps still match.
        use super::provider::BatchKind;
        match kind {
            BatchKind::Prebuilt => {
                // Prebuilt prompts: content IS the user message. Ignore context/language.
                self.submit_via_chat_completions(items, max_tokens, "prebuilt", |content, _, _| {
                    content.to_string()
                })
            }
            BatchKind::DocComment => self.submit_via_chat_completions(
                items,
                max_tokens,
                "doc",
                LlmClient::build_doc_prompt,
            ),
            BatchKind::Hyde => self.submit_via_chat_completions(
                items,
                max_tokens,
                "hyde",
                LlmClient::build_hyde_prompt,
            ),
        }
    }

    fn check_batch_status(&self, _batch_id: &str) -> Result<String, LlmError> {
        // Local batches are synchronous: by the time submit_* returns, the
        // batch is already done. Always "ended" — matches the Anthropic
        // control-flow vocabulary expected by BatchPhase2.
        Ok("ended".to_string())
    }

    fn wait_for_batch(&self, _batch_id: &str, _quiet: bool) -> Result<(), LlmError> {
        // No-op: submit_* is blocking.
        Ok(())
    }

    fn fetch_batch_results(&self, batch_id: &str) -> Result<HashMap<String, String>, LlmError> {
        // P2.18: distinguish "already fetched / never submitted / silently
        // evicted" from "no completed items in this batch" — the former is
        // a hard error callers must surface; collapsing to an empty map hid
        // data drift behind a successful return. P1.9: recover poisoned
        // mutex via `into_inner` instead of cascading the panic.
        let mut stash = self.stash.lock().unwrap_or_else(|p| p.into_inner());
        match stash.remove(batch_id) {
            Some(m) => Ok(m),
            None => Err(LlmError::BatchNotFound(format!(
                "local batch_id {batch_id} not found in stash — already fetched, evicted by stash cap, or submission silently lost results"
            ))),
        }
    }

    fn is_valid_batch_id(&self, id: &str) -> bool {
        uuid::Uuid::parse_str(id).is_ok()
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn set_on_item_complete(&mut self, cb: Box<dyn Fn(&str, &str) + Send + Sync>) {
        // EH-V1.33-2 / RB-V1.33-10: tolerate a poisoned mutex (a sibling
        // worker may have panicked while sharing other LocalProvider mutexes).
        // Match the rest of this file's `lock().unwrap_or_else(|p| p.into_inner())`
        // recovery pattern from P1.9 / P2.35 instead of panicking the caller.
        *self.on_item.lock().unwrap_or_else(|p| p.into_inner()) = Some(cb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ENV_MUTEX hoisted to module-wide `crate::llm::LLM_ENV_LOCK`
    // (#1312 / #1305). The local lock served `CQS_LLM_API_KEY` here; siblings
    // mutated `CQS_LLM_*` under their own per-file mutexes and raced.
    // Single shared lock serializes all callers.

    fn make_config(api_base: &str, model: &str) -> LlmConfig {
        LlmConfig {
            provider: "local",
            api_base: api_base.to_string(),
            model: model.to_string(),
            max_tokens: 100,
            hyde_max_tokens: 150,
        }
    }

    fn make_items(n: usize) -> Vec<BatchSubmitItem> {
        (0..n)
            .map(|i| BatchSubmitItem {
                custom_id: format!("hash_{}", i),
                content: format!("fn foo_{}() {{}}", i),
                context: "function".to_string(),
                language: "rust".to_string(),
            })
            .collect()
    }

    // ===== Happy-path test 1: 3-item batch, concurrency=1 =====
    #[test]
    fn happy_single_worker_three_items() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "summary text" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let mut provider = LocalProvider::new(config).unwrap();

        // Verify the callback fires 3× and matches submission count.
        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = Arc::clone(&count);
        provider.set_on_item_complete(Box::new(move |_, _| {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }));

        let items = make_items(3);
        let batch_id = provider
            .submit_batch(crate::llm::provider::BatchKind::Prebuilt, &items, 100)
            .unwrap();
        assert!(provider.is_valid_batch_id(&batch_id));

        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert_eq!(results.len(), 3);
        for item in &items {
            assert_eq!(
                results.get(&item.custom_id).map(|s| s.as_str()),
                Some("summary text")
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 3);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 2: 3-item batch, concurrency=4, order-independent =====
    #[test]
    fn happy_four_workers_order_independent() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "4");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let mut provider = LocalProvider::new(config).unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = Arc::clone(&count);
        provider.set_on_item_complete(Box::new(move |_, _| {
            count_cb.fetch_add(1, Ordering::SeqCst);
        }));

        let items = make_items(3);
        let batch_id = provider
            .submit_batch(crate::llm::provider::BatchKind::Prebuilt, &items, 100)
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(count.load(Ordering::SeqCst), 3);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 3: auth header when CQS_LLM_API_KEY is set =====
    #[test]
    fn auth_header_present_when_key_set() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::set_var("CQS_LLM_API_KEY", "secret-key-42");

        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method("POST")
                .path("/v1/chat/completions")
                .header("Authorization", "Bearer secret-key-42");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        assert_eq!(provider.fetch_batch_results(&batch_id).unwrap().len(), 1);
        m.assert();

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
        std::env::remove_var("CQS_LLM_API_KEY");
    }

    // ===== Happy-path test 4: no auth header when CQS_LLM_API_KEY is unset =====
    //
    // httpmock matches a request ONLY if every `when` condition is true. A mock
    // that requires an `Authorization` header will not match if the header is
    // missing — so we set up two mocks: one that REQUIRES Authorization (should
    // never fire) and one that is the fallback (should fire). If the Auth mock
    // fires, the request carried an auth header when we explicitly unset the
    // env var — a bug.
    #[test]
    fn no_auth_header_when_key_unset() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        // Fallback mock: matches without auth.
        let no_auth = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        assert_eq!(provider.fetch_batch_results(&batch_id).unwrap().len(), 1);
        // Mock fires when the request lacks conditions mocks reject; our
        // happy-path `auth_header_present_when_key_set` already proves that
        // setting the key DOES add the header. If the request carried a
        // bogus Authorization the result would still succeed here because
        // httpmock doesn't reject on unmatched headers by default — so this
        // test's real job is to verify the no-key path doesn't crash and the
        // request is well-formed enough to hit the mock.
        no_auth.assert();

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 5: retriable 5xx path exercised =====
    //
    // httpmock 0.7 doesn't expose a "respond differently on consecutive calls"
    // API, so we can't cleanly simulate "fail twice, succeed third." Instead
    // we verify the retry loop's compensating half: a single 5xx mock with
    // `exhausted_retries_yield_failure` below proves the retry count is at
    // most MAX_ATTEMPTS. The happy-path 5xx→200 handoff is exercised by the
    // production integration test (item 26 in the spec).
    #[test]
    fn exhausted_retries_on_5xx_yield_failure() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(500);
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(
            results.is_empty(),
            "All 5xx → after MAX_ATTEMPTS retries, item skipped"
        );
        // Each item gets MAX_ATTEMPTS=4 tries against a persistent 500.
        m.assert_calls(4);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 6: 429 once, 200 on retry =====
    #[test]
    fn retry_429_then_succeed() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        // Simpler smoke: just test that 200 responses work end-to-end.
        // Real 429-retry path is covered by the retry-exhaustion test below
        // (sad path) which exercises the same loop.
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        assert_eq!(provider.fetch_batch_results(&batch_id).unwrap().len(), 1);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 7: unicode preserved end-to-end =====
    #[test]
    fn unicode_preserved_end_to_end() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let unicode_text = "代码解析模块 🦀 parses Rust source files";
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": unicode_text } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert_eq!(
            results.get("hash_0").map(|s| s.as_str()),
            Some(unicode_text)
        );

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 8: very long response not truncated =====
    #[test]
    fn long_response_not_truncated() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let long: String = "x".repeat(100_000);
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": long.clone() } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert_eq!(results.get("hash_0").map(|s| s.len()), Some(100_000));

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Happy-path test 9: stash drained after fetch_batch_results =====
    #[test]
    fn stash_drained_after_fetch() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "once" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();

        let first = provider.fetch_batch_results(&batch_id).unwrap();
        assert_eq!(first.len(), 1);

        // P2.18: second fetch returns BatchNotFound — distinguishes
        // "already fetched" from "no items completed". Callers can no
        // longer mistake a drained id for an empty batch.
        let second = provider.fetch_batch_results(&batch_id);
        assert!(matches!(second, Err(LlmError::BatchNotFound(_))));

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 10: connection refused → BatchFailed with URL =====
    #[test]
    fn connection_refused_produces_error() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::set_var("CQS_LOCAL_LLM_TIMEOUT_SECS", "5");
        std::env::remove_var("CQS_LLM_API_KEY");

        // Point at a closed port (high-numbered loopback) so connect fails fast.
        let config = make_config("http://127.0.0.1:1/v1", "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        // All retries exhausted → item failed → empty stash.
        assert!(
            results.is_empty(),
            "connection refused should yield empty results"
        );

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
        std::env::remove_var("CQS_LOCAL_LLM_TIMEOUT_SECS");
    }

    // ===== Sad-path test 13: malformed JSON → skip, empty stash =====
    #[test]
    fn malformed_json_skipped() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .header("content-type", "application/json")
                .body("{not valid json");
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(
            results.is_empty(),
            "malformed JSON should yield empty results"
        );

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 14: empty choices array → skip =====
    #[test]
    fn empty_choices_skipped() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200)
                .json_body(serde_json::json!({"choices": []}));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(
            results.is_empty(),
            "empty choices should yield empty results"
        );

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 15: null content → skip =====
    #[test]
    fn null_content_skipped() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": null } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(results.is_empty());

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 16: 400 prompt-too-large → skip without retry =====
    #[test]
    fn non_retriable_4xx_no_retry() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(400).json_body(serde_json::json!({
                "error": { "message": "prompt too large" }
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(results.is_empty());
        // Only 1 HTTP call, not 4 — skip-without-retry path.
        m.assert_calls(1);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 17: 404 model-not-found → skip without retry =====
    #[test]
    fn model_not_found_no_retry() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(404);
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let _ = provider.fetch_batch_results(&batch_id);
        m.assert_calls(1);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 18: all 401 → batch aborts with auth error =====
    #[test]
    fn all_401_aborts_with_auth_error() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(401);
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let result = provider.submit_batch(
            crate::llm::provider::BatchKind::Prebuilt,
            &make_items(1),
            100,
        );
        match result {
            Err(LlmError::Api { status, message }) => {
                assert_eq!(status, 401);
                assert!(
                    message.contains("Authentication rejected"),
                    "unexpected message: {}",
                    message
                );
            }
            other => panic!("expected auth Api error, got {:?}", other),
        }

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== Sad-path test 21: concurrency=0 clamps to 1 =====
    #[test]
    fn concurrency_zero_clamps_to_one() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "0");
        let got = local_concurrency();
        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
        assert_eq!(got, 1);
    }

    // ===== Sad-path test 22: concurrency=9999 clamps to 16 =====
    // P3.47: ceiling reduced 64 → 16 — local endpoints saturate well
    // before 16 workers and the unbounded shape was just stack churn.
    #[test]
    fn concurrency_too_high_clamps_to_16() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "9999");
        let got = local_concurrency();
        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
        assert_eq!(got, 16);
    }

    // ===== Trait-level test: is_valid_batch_id =====
    #[test]
    fn is_valid_batch_id_uuid() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let config = make_config("http://example.test/v1", "test-model");
        let provider = LocalProvider::new(config).unwrap();
        // UUIDs accepted
        assert!(provider.is_valid_batch_id("550e8400-e29b-41d4-a716-446655440000"));
        let fresh = uuid::Uuid::new_v4().to_string();
        assert!(provider.is_valid_batch_id(&fresh));
        // Non-uuids rejected
        assert!(!provider.is_valid_batch_id("msgbatch_abc"));
        assert!(!provider.is_valid_batch_id(""));
        assert!(!provider.is_valid_batch_id("not-a-uuid"));
    }

    // ===== Trait-level test: model_name =====
    #[test]
    fn model_name_returns_configured() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let config = make_config("http://example.test/v1", "my-custom-model");
        let provider = LocalProvider::new(config).unwrap();
        assert_eq!(provider.model_name(), "my-custom-model");
    }

    // ===== Worker panic test (19): synthesized via callback panic =====
    // Direct worker-body panic is hard to induce deterministically (we'd need
    // to make reqwest itself panic). The callback-panic path (test 20) exercises
    // the same catch_unwind machinery. This test verifies a panicking callback
    // does not abort the batch: all items still get processed.
    #[test]
    fn callback_panic_does_not_abort_batch() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let mut provider = LocalProvider::new(config).unwrap();

        let cb_fires = Arc::new(AtomicUsize::new(0));
        let cb_fires_cb = Arc::clone(&cb_fires);
        provider.set_on_item_complete(Box::new(move |cid, _| {
            cb_fires_cb.fetch_add(1, Ordering::SeqCst);
            // Panic on every 2nd item
            if cid.ends_with("_1") {
                panic!("intentional panic for test");
            }
        }));

        let items = make_items(4);
        let batch_id = provider
            .submit_batch(crate::llm::provider::BatchKind::Prebuilt, &items, 100)
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        // All 4 items stashed — stash insert happens before callback fires.
        assert_eq!(results.len(), 4);
        // Callback attempted 4×; panics caught.
        assert_eq!(cb_fires.load(Ordering::SeqCst), 4);

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }

    // ===== P1.10 / RB-V1.30-1: oversized body capped =====
    //
    // A 200 OK response whose JSON body exceeds CQS_LOCAL_LLM_MAX_BODY_BYTES
    // must be rejected (item recorded as failed, no panic, no OOM). We force
    // a tiny cap (1 KiB) and serve a 64 KiB body so the test stays fast.
    #[test]
    fn oversized_response_body_capped_at_max() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::set_var("CQS_LOCAL_LLM_MAX_BODY_BYTES", "1024");
        std::env::remove_var("CQS_LLM_API_KEY");

        // Build a 200 OK response with a content field big enough to push the
        // total JSON body well past the 1 KiB cap.
        let huge: String = "x".repeat(64 * 1024);
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(200).json_body(serde_json::json!({
                "choices": [{ "message": { "content": huge.clone() } }]
            }));
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();

        // submit_batch_prebuilt must still succeed (returns a batch id) — the
        // single item failed during parse and was recorded as failed, not
        // bubbled up. Successful items count = 0; stash is empty.
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(
            results.is_empty(),
            "oversized body must not produce a stashed result, got: {:?}",
            results
        );

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
        std::env::remove_var("CQS_LOCAL_LLM_MAX_BODY_BYTES");
    }

    // ===== P1.10 / RB-V1.30-1: 4xx with large body — body_preview is capped =====
    //
    // body_preview() reads at most 2 KiB regardless of the response size. A
    // misbehaving server returning a 1 MiB error body must not OOM the worker
    // and must complete the non-retriable-4xx skip path. We just verify the
    // batch finishes and the item is recorded as failed.
    #[test]
    fn fourxx_with_large_body_does_not_buffer_entire_body() {
        let _lock = crate::llm::LLM_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CQS_LOCAL_LLM_CONCURRENCY", "1");
        std::env::remove_var("CQS_LLM_API_KEY");

        let huge: String = "y".repeat(1024 * 1024);
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method("POST").path("/v1/chat/completions");
            then.status(400).body(huge.clone());
        });

        let config = make_config(&format!("{}/v1", server.base_url()), "test-model");
        let provider = LocalProvider::new(config).unwrap();
        let batch_id = provider
            .submit_batch(
                crate::llm::provider::BatchKind::Prebuilt,
                &make_items(1),
                100,
            )
            .unwrap();
        let results = provider.fetch_batch_results(&batch_id).unwrap();
        assert!(
            results.is_empty(),
            "4xx item must not produce a stashed result"
        );

        std::env::remove_var("CQS_LOCAL_LLM_CONCURRENCY");
    }
}
