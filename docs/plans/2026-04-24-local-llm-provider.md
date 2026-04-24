# Local LLM Provider (OpenAI-compat)

Tracked by issue #1098 (audit EX-V1.29-3).

## Summary

Add a `LocalProvider` implementing the existing `BatchProvider` trait against any OpenAI-compat `/v1/chat/completions` endpoint (llama.cpp server, vLLM, Ollama, LMStudio, text-generation-webui). Thread the trait through `create_client` so it returns `Box<dyn BatchProvider>`. Add a per-item streaming persist hook so local runs survive Ctrl-C without losing completed work.

No new commands, no schema bump, no reindex. Default behaviour (`CQS_LLM_PROVIDER=anthropic`) unchanged.

## Scope

**In:**
- `LocalProvider` in `src/llm/local.rs` implementing `BatchProvider`
- `LlmClient` (Anthropic) becomes `impl BatchProvider` — currently free-standing with methods in `src/llm/batch.rs`
- `create_client` returns `Box<dyn BatchProvider>` instead of `Result<LlmClient>`
- Streaming per-item persist hook on the trait; wired into `summary.rs` / `doc_comments.rs` / `hyde.rs`
- Env-var configuration + actionable error messages on misconfig
- Full unit + integration test suite per §6

**Out (defer until a real user need surfaces):**
- OpenAI Batches API provider
- Groq / Together / other hosted providers
- Multimodal (image) requests
- Streaming response parsing (SSE)

## Architecture

Four changes, in order.

### 1. `LlmClient` impl `BatchProvider`

Existing free-standing methods in `src/llm/batch.rs` wrap into a `impl BatchProvider for LlmClient` block. `create_client` returns `Result<Box<dyn BatchProvider>>`. All callsites updated (6–8 sites based on re-exports in `mod.rs`).

### 2. Streaming persist hook on the trait

New method with default no-op:

```rust
pub trait BatchProvider {
    // ... existing methods

    /// Optional streaming callback invoked once per completed item.
    ///
    /// Callers (e.g. `llm_summary_pass`) can set this to persist results
    /// to SQLite as they arrive, enabling crash-safe partial completion
    /// without changing the store-all-at-end contract of `fetch_batch_results`.
    ///
    /// **Concurrency contract:** the callback may be invoked from multiple
    /// worker threads concurrently. Implementations must be `Fn + Send + Sync`
    /// and must serialize any shared mutable state internally (typically via
    /// `Mutex<Connection>`). Panics in the callback are caught and logged;
    /// they do not abort the batch. SQLite `INSERT OR IGNORE` on the
    /// `content_hash` primary key gracefully handles redundant writes from
    /// both streaming and `fetch_batch_results` paths.
    ///
    /// Default: no-op. The Anthropic path uses fetch-at-end semantics and
    /// ignores the callback.
    fn set_on_item_complete(&mut self, _cb: Box<dyn Fn(&str, &str) + Send + Sync>) {}
}
```

`LocalProvider` stores the callback and invokes `(cb)(&custom_id, &text)` per worker completion. Anthropic path: default no-op preserves today's behaviour.

### 3. `LocalProvider` impl

New file `src/llm/local.rs`. Fields:

```rust
pub struct LocalProvider {
    http: reqwest::blocking::Client,
    api_base: String,
    model: String,
    concurrency: usize,
    api_key: Option<String>,
    timeout: Duration,
    on_item: Mutex<Option<Box<dyn Fn(&str, &str) + Send + Sync>>>,
    stash: Mutex<HashMap<String, HashMap<String, String>>>, // batch_id → (custom_id → text)
}
```

All three `submit_*` variants share `submit_via_chat_completions(items, max_tokens, prompt_shape)`:

- `std::thread::scope` spawns `concurrency` workers
- `crossbeam_channel::bounded(max(concurrency * 2, 16))` feeds `BatchSubmitItem`s
- Each worker: POST `${api_base}/chat/completions`, parse `choices[0].message.content`
- Per-item body wrapped in `std::panic::catch_unwind` — panics logged at `error!`, item skipped, worker continues
- Callback invocation separately `catch_unwind`-wrapped
- Returns uuid batch_id after all workers join (`std::thread::scope` guarantees this)
- `check_batch_status` / `wait_for_batch`: trivial stash reads; wait is no-op
- `fetch_batch_results`: removes and returns entry from stash (drain)
- `is_valid_batch_id`: accepts any parseable uuid
- `model_name`: returns `&self.model`

Prompt-shape handling: the existing three submit variants (`prebuilt` / `doc` / `hyde`) differ only in how the prompt is built. `LocalProvider` reuses the prompt builders from `src/llm/prompts.rs` exactly as the Anthropic path does.

### 4. Outer loop streaming adoption

`src/llm/summary.rs`, `src/llm/doc_comments.rs`, `src/llm/hyde.rs` wire the callback:

```rust
let conn = Arc::new(Mutex::new(/* caller's SQLite handle */));
let conn_cb = Arc::clone(&conn);
provider.set_on_item_complete(Box::new(move |custom_id, text| {
    if let Ok(c) = conn_cb.lock() {
        // INSERT OR IGNORE INTO summaries ...
    }
}));
```

Anthropic path: `set_on_item_complete` no-op → callback never fires → store-all-at-once unchanged.
Local path: per-item persist on arrival + redundant `fetch_batch_results` pass at end (no-op work via `INSERT OR IGNORE`).

## Configuration

| Env var                         | Required for local | Default                         | Purpose                                  |
|---------------------------------|--------------------|---------------------------------|------------------------------------------|
| `CQS_LLM_PROVIDER=local`        | yes                | `anthropic`                     | activates local path                     |
| `CQS_LLM_API_BASE`              | yes                | `https://api.anthropic.com/v1`  | no sensible default for local            |
| `CQS_LLM_MODEL`                 | yes                | `claude-haiku-4-5`              | most servers reject empty                |
| `CQS_LOCAL_LLM_CONCURRENCY`     | no                 | `4`                             | `<=0` clamps to 1; `>64` clamps to 64    |
| `CQS_LOCAL_LLM_TIMEOUT_SECS`    | no                 | `120`                           | per-request timeout (Anthropic uses 60)  |
| `CQS_LLM_API_KEY`               | no                 | (unset)                         | `Authorization: Bearer` if set           |
| `CQS_LLM_ALLOW_INSECURE=1`      | for `http://`      | unset                           | existing SEC-V1.25-13 opt-in             |

### Actionable errors on misconfig

- Missing `CQS_LLM_API_BASE`: `"Set CQS_LLM_API_BASE=http://localhost:8080/v1 (or your server's URL)"`
- Missing `CQS_LLM_MODEL`: `"Set CQS_LLM_MODEL=<your-model-name>; try curl ${CQS_LLM_API_BASE}/models to list available"`
- Connection refused on first call: `"No LLM server reachable at ${api_base}. Is your vLLM/llama.cpp/Ollama running?"`
- 401/403 across all workers on first item: `"Authentication rejected at <url>; check CQS_LLM_API_KEY"`

## Error handling

### Retry policy

**4 attempts, exponential backoff: 500ms → 1s → 2s → 4s** (7.5s max per item).

| Class                                     | Action                              |
|-------------------------------------------|-------------------------------------|
| `429` (rate limit)                        | retry                               |
| `5xx`                                     | retry                               |
| `4xx` ≠ 429 (model-not-found, prompt-too-large, auth) | **skip, do not retry**  |
| Connection refused / timeout / DNS        | retry                               |
| Malformed JSON / empty `choices` / null `content` | skip, warn                  |
| Worker panic                              | catch, log, skip item, continue     |
| Callback panic                            | catch, log, skip callback, continue |

### Fatal batch aborts

- All workers see 401/403 on first request: `Api { status, message }` with auth-specific message
- `create_client` called without required env vars: `ApiKeyMissing`-style error before HTTP traffic

### Resource hygiene

- `fetch_batch_results` drains its entry from the stash → no daemon memory growth
- Worker pool is `std::thread::scope`-bound → panics on join would propagate, but `catch_unwind` per-item prevents them
- HTTP client reused across workers (reqwest connection pool)

## Tracing

Every function entry gets a span (MEMORY.md pattern). Specifically:

```rust
// Outer submit
tracing::info_span!("local_batch_submit", provider="local", model, n=items.len(), concurrency).entered()

// Per-worker
tracing::debug_span!("local_worker", worker_id).entered()

// Per-item
tracing::debug_span!("local_item", custom_id, attempt)

// Events
tracing::warn!(attempt, backoff_ms, error_kind, "local retry");
tracing::warn!(timeout_secs, url, "local request timed out");
tracing::warn!(status, body, "local item non-retriable 4xx, skipping");
tracing::error!(item_id, panic = ?err, "worker panic, skipping item");
tracing::info!(batch_id, submitted=n, succeeded=ok, failed=err, elapsed_ms, "local batch complete");
```

Smoke verification: `RUST_LOG=cqs::llm=debug cqs summarize` surfaces the span tree.

## Testing

### Happy paths (mocked HTTP, `src/llm/local.rs::tests`)

1. 3-item batch, concurrency=1 — all results returned, callback fires 3×
2. 3-item batch, concurrency=4 — all results returned, callback fires 3×, order-independent
3. `CQS_LLM_API_KEY` set → `Authorization: Bearer <token>` header on each request
4. `CQS_LLM_API_KEY` unset → no auth header
5. 5xx on first 2 attempts, 200 on 3rd → succeeds after retry
6. 429 on first attempt, 200 on 2nd → succeeds after retry
7. Unicode preserved end-to-end (CJK + emoji) — parity with existing Anthropic tests
8. Very long response (100k chars) not truncated
9. Stash drained after `fetch_batch_results` — second fetch returns empty map

### Sad paths (mocked HTTP)

10. Connection refused → `BatchFailed` with URL in message
11. Request timeout (mock delays > timeout) → `BatchFailed` with timeout message
12. Server restart mid-batch (mock refuses items 3-5) → items 1-2 succeed, items 3-5 skipped, partial stash returned
13. Malformed JSON response (`{garbage}`) → `Json` error, item skipped
14. Empty `choices` array → item skipped, no panic
15. `choices[0].message.content = null` → item skipped, no panic
16. 400 with prompt-too-large → skip without retry (verify only 1 HTTP call, not 4)
17. 404 model-not-found → skip without retry
18. All workers see 401 on first request → batch aborts with auth error
19. Worker panic on item 2 of 5 → items 1, 3, 4, 5 succeed; item 2 logged at `error!`; batch completes
20. Callback panic on item 2 → remaining items still callback; batch completes
21. `CQS_LOCAL_LLM_CONCURRENCY=0` → clamped to 1, batch succeeds
22. `CQS_LOCAL_LLM_CONCURRENCY=9999` → clamped to 64, batch succeeds

### Config sad paths (`src/llm/mod.rs::tests`)

23. `CQS_LLM_PROVIDER=local` without `CQS_LLM_API_BASE` → `create_client` returns actionable error
24. `CQS_LLM_PROVIDER=local` without `CQS_LLM_MODEL` → `create_client` returns actionable error
25. `CQS_LLM_PROVIDER=local CQS_LLM_API_BASE=http://...` without `CQS_LLM_ALLOW_INSECURE=1` → existing SEC-V1.25-13 error applies to the local variant

### Integration (`#[ignore]`-gated, `tests/local_provider_integration.rs`)

26. `httpmock`-backed 5-chunk fixture through `llm_summary_pass` — all 5 cache entries land
27. Disconnect mock after item 3/5 → first 3 cache entries survive (streaming persist works); 4-5 absent
28. Re-run after partial — first run 3/5 → second run processes remaining 2 (content-hash cache prevents re-summarize)
29. Full `llm_summary_pass` with concurrency=1 AND concurrency=4 → output equivalent

### Trait-level tests

- `LocalProvider::is_valid_batch_id` accepts uuids, rejects non-uuids
- `LocalProvider::model_name` returns configured model
- Trait default `set_on_item_complete` preserved on Anthropic path (no-op)

## Acceptance criteria

- [ ] All 1679 lib tests pass; no existing test regresses
- [ ] 29 new tests added per §Testing; all pass
- [ ] `CQS_LLM_PROVIDER=local CQS_LLM_API_BASE=<url> CQS_LLM_MODEL=<name> cqs summarize` against a live OpenAI-compat server produces cached summaries (manual acceptance — documented in PR description)
- [ ] Ctrl-C at ~50% of a local run and re-invoke: content-hash-cached items skipped, remainder processed
- [ ] README env var table updated (7 new/changed rows)
- [ ] `cqs doctor` surfaces local LLM misconfig when relevant env vars missing or endpoint unreachable
- [ ] No new clippy warnings under `--features gpu-index`
- [ ] Tracing spans verified present via `RUST_LOG=cqs::llm=debug` smoke run

## Known limitations

- **Blocking submit wedges the daemon socket** during long runs. Pre-existing on the Anthropic path (`wait_for_batch` polls synchronously). Not addressed in this PR; future work could move batch operations to a daemon worker pool.
- **No progress bar, only tracing.** Terminal-blocking UX acceptable per decision on this spec; revisit if it bites.
- **Single-process stash.** No cross-process resume — stash lives in the `LocalProvider` instance. Content-hash cache covers this for the common case.
- **No OpenAI Batches API / Groq / Together.** The trait plumbing makes these trivial to add later (new file + factory arm); deferred until real need.
